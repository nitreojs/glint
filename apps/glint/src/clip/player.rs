use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use eframe::egui::{self, Color32, ColorImage};
use glint_core::clip::math::SECOND;
use glint_media::clip::media::{self, Com, Decoded};
use windows::Win32::Media::MediaFoundation::IMFSourceReader;

pub(super) const NO_CLOCK: i64 = i64::MIN;
pub(super) const AHEAD: usize = 4;

pub(super) struct Shown {
    pub epoch: u64,
    pub ts: i64,
    pub dur: i64,
    pub image: Arc<ColorImage>,
}

pub(super) enum Cmd {
    Seek { t: i64, epoch: u64, play: bool, clock: i64 },
    Pause,
}

pub(super) struct Player {
    tx: Option<Sender<Cmd>>,
    rx: Receiver<Result<Shown, String>>,
    queued: Arc<AtomicUsize>,
    /// Where playback is (`i64::MIN` while it isn't running): frames the decoder can only finish
    /// after their time has passed are skipped, so a slow machine shows fewer frames, not late ones.
    clock: Arc<AtomicI64>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub(super) fn start(path: PathBuf, size: (u32, u32), ctx: egui::Context) -> Self {
        let (tx, cmds) = mpsc::channel();
        let (out, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let clock = Arc::new(AtomicI64::new(NO_CLOCK));
        let (q, c) = (queued.clone(), clock.clone());
        let thread = thread::spawn(move || {
            let _com = Com::init();
            let mut w = Worker {
                size, out, queued: q, clock: c, ctx, epoch: 0,
                head: None, ahead: None, sent: false, fresh: true, eof: false,
            };
            if let Err(e) = w.run(&path, &cmds) {
                let _ = w.out.send(Err(e));
                w.ctx.request_repaint();
            }
        });
        Player { tx: Some(tx), rx, queued, clock, thread: Some(thread) }
    }

    /// Where playback is now, while it runs.
    pub(super) fn set_clock(&self, t: i64) {
        self.clock.store(t, Ordering::Relaxed);
    }

    pub(super) fn send(&self, cmd: Cmd) {
        let clock = match cmd {
            Cmd::Seek { clock, .. } => clock,
            Cmd::Pause => NO_CLOCK,
        };
        self.set_clock(clock);
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
            if let Some(worker) = &self.thread {
                worker.thread().unpark();
            }
        }
    }

    pub(super) fn poll(&self) -> Option<Result<Shown, String>> {
        let got = self.rx.try_recv().ok()?;
        if got.is_ok()
            && self.queued.fetch_sub(1, Ordering::Relaxed) == AHEAD
            && let Some(worker) = &self.thread
        {
            worker.thread().unpark();
        }

        Some(got)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FrameAction {
    Show,
    ReadAhead,
    Advance,
}

pub(super) fn frame_action(t: i64, ts: i64, dur: i64, ahead: Option<i64>, eof: bool) -> FrameAction {
    if ahead.is_some_and(|next| next <= t) {
        return FrameAction::Advance;
    }
    if ts >= t || (dur > 0 && ts.saturating_add(dur) > t) || ahead.is_some() || eof {
        return FrameAction::Show;
    }

    FrameAction::ReadAhead
}

struct Frame {
    decoded: Decoded,
    image: Option<Arc<ColorImage>>,
}

struct Worker {
    size: (u32, u32),
    out: Sender<Result<Shown, String>>,
    queued: Arc<AtomicUsize>,
    clock: Arc<AtomicI64>,
    ctx: egui::Context,
    epoch: u64,
    head: Option<Frame>,
    ahead: Option<Decoded>,
    sent: bool,
    fresh: bool,
    eof: bool,
}

impl Worker {
    fn run(&mut self, path: &Path, cmds: &Receiver<Cmd>) -> Result<(), String> {
        let reader = media::rgb_reader(path, self.size)?;
        let mut playing = false;
        let mut seek = None;
        loop {
            let mut reposition = false;
            loop {
                match cmds.try_recv() {
                    Ok(Cmd::Seek { t, epoch, play, .. }) => {
                        self.epoch = epoch;
                        playing = play;
                        seek = Some(t);
                        reposition = true;
                    }
                    Ok(Cmd::Pause) => {
                        playing = false;
                        seek = None;
                        reposition = false;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            if reposition && let Some(t) = seek {
                self.seek(&reader, t)?;
            }
            if (!playing && seek.is_none()) || self.queued.load(Ordering::Relaxed) >= AHEAD {
                thread::park();
                continue;
            }
            if self.head.is_none() || (seek.is_none() && self.sent) {
                if !self.advance(&reader)? {
                    playing = false;
                    seek = None;
                }
                continue;
            }

            let clock = self.clock.load(Ordering::Relaxed);
            let t = seek.map_or(clock, |target| target.max(clock));
            let f = &self.head.as_ref().unwrap().decoded;
            match frame_action(t, f.ts, f.dur, self.ahead.as_ref().map(|f| f.ts), self.eof) {
                FrameAction::Show => {
                    self.show()?;
                    self.sent = true;
                    seek = None;
                }
                FrameAction::ReadAhead => self.ahead = self.read(&reader)?,
                FrameAction::Advance => {
                    self.advance(&reader)?;
                }
            }
        }
    }

    fn seek(&mut self, reader: &IMFSourceReader, t: i64) -> Result<(), String> {
        let near = |ts| ts <= t && t - ts < SECOND;
        if self.ahead.as_ref().is_some_and(|f| near(f.ts)) {
            self.advance(reader)?;
        } else if !self.head.as_ref().is_some_and(|f| near(f.decoded.ts)) && !(self.fresh && t == 0) {
            media::seek(reader, t)?;
            self.head = None;
            self.ahead = None;
            self.eof = false;
            self.fresh = false;
        }
        self.sent = false;

        Ok(())
    }

    fn advance(&mut self, reader: &IMFSourceReader) -> Result<bool, String> {
        let decoded = match self.ahead.take() {
            Some(f) => Some(f),
            None => self.read(reader)?,
        };
        let Some(decoded) = decoded else { return Ok(false) };
        self.head = Some(Frame { decoded, image: None });
        self.sent = false;

        Ok(true)
    }

    fn read(&mut self, reader: &IMFSourceReader) -> Result<Option<Decoded>, String> {
        if self.eof {
            return Ok(None);
        }
        self.fresh = false;
        let next = media::next_video(reader)?;
        self.eof = next.is_none();

        Ok(next)
    }

    fn show(&mut self) -> Result<(), String> {
        let head = self.head.as_mut().unwrap();
        let f = &head.decoded;
        if head.image.is_none() {
            let (w, h) = (self.size.0 as usize, self.size.1 as usize);
            let pixels = media::with_pixels(&f.sample, self.size.0, |rows| {
                let mut px = Vec::with_capacity(w * h);
                for y in 0..h {
                    px.extend(rows.row(y).chunks_exact(4).map(|c| Color32::from_rgb(c[2], c[1], c[0])));
                }

                px
            })?;
            head.image = Some(Arc::new(ColorImage::new([w, h], pixels)));
        }
        let image = head.image.as_ref().unwrap().clone();
        self.queued.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(Ok(Shown { epoch: self.epoch, ts: f.ts, dur: f.dur, image }));
        self.ctx.request_repaint();

        Ok(())
    }
}
