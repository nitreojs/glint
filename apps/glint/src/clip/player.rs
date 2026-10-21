use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui::{self, Color32, ColorImage};
use glint_core::clip::math::SECOND;
use glint_media::clip::media::{self, Com, Decoded};
use windows::Win32::Media::MediaFoundation::IMFSourceReader;

const AHEAD: usize = 4;

pub(super) struct Shown {
    pub epoch: u64,
    pub ts: i64,
    pub dur: i64,
    pub image: ColorImage,
}

pub(super) enum Cmd {
    Seek { t: i64, epoch: u64 },
    Play(bool),
}

pub(super) struct Player {
    tx: Option<Sender<Cmd>>,
    rx: Receiver<Result<Shown, String>>,
    queued: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Player {
    pub(super) fn start(path: PathBuf, size: (u32, u32), ctx: egui::Context) -> Self {
        let (tx, cmds) = mpsc::channel();
        let (out, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let q = queued.clone();
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            let mut w = Worker { size, out, queued: q, ctx, epoch: 0, head: None, head_sent: false };
            if let Err(e) = w.run(&path, &cmds) {
                let _ = w.out.send(Err(e));
                w.ctx.request_repaint();
            }
        });
        Player { tx: Some(tx), rx, queued, thread: Some(thread) }
    }

    pub(super) fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }

    pub(super) fn poll(&self) -> Option<Result<Shown, String>> {
        let got = self.rx.try_recv().ok()?;
        if got.is_ok() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
        }
        Some(got)
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Worker {
    size: (u32, u32),
    out: Sender<Result<Shown, String>>,
    queued: Arc<AtomicUsize>,
    ctx: egui::Context,
    epoch: u64,
    head: Option<Decoded>,
    head_sent: bool,
}

impl Worker {
    fn run(&mut self, path: &std::path::Path, cmds: &Receiver<Cmd>) -> Result<(), String> {
        let reader = media::rgb_reader(path, self.size)?;
        let mut playing = false;
        loop {
            let room = self.queued.load(Ordering::Relaxed) < AHEAD;
            let cmd = if playing && room {
                match cmds.try_recv() {
                    Ok(c) => Some(c),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            } else if playing {
                match cmds.recv_timeout(Duration::from_millis(4)) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                match cmds.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return Ok(()),
                }
            };
            if let Some(first) = cmd {
                let mut seek = None;
                for c in std::iter::once(first).chain(cmds.try_iter()) {
                    match c {
                        Cmd::Seek { t, epoch } => seek = Some((t, epoch)),
                        Cmd::Play(p) => playing = p,
                    }
                }
                if let Some((t, epoch)) = seek {
                    self.epoch = epoch;
                    self.seek(&reader, t)?;
                }
                continue;
            }
            if !playing || !room {
                continue;
            }
            match self.head.take() {
                Some(h) if !self.head_sent => self.show(h)?,
                _ => match media::next_video(&reader)? {
                    Some(f) => self.show(f)?,
                    None => playing = false,
                },
            }
        }
    }

    fn seek(&mut self, reader: &IMFSourceReader, t: i64) -> Result<(), String> {
        let best = match self.head.take() {
            Some(h) if h.ts <= t && t - h.ts < SECOND => Some(h),
            _ => {
                media::seek(reader, t)?;
                None
            }
        };
        let (found, ahead) = media::decode_until(reader, t, best)?;
        let Some(found) = found else { return Ok(()) };
        match ahead {
            Some(a) => {
                self.send(&found)?;
                self.head = Some(a);
                self.head_sent = false;
            }
            None => self.show(found)?,
        }
        Ok(())
    }

    fn show(&mut self, f: Decoded) -> Result<(), String> {
        self.send(&f)?;
        self.head = Some(f);
        self.head_sent = true;
        Ok(())
    }

    fn send(&self, f: &Decoded) -> Result<(), String> {
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let pixels = media::with_pixels(&f.sample, self.size.0, |rows| {
            let mut px = Vec::with_capacity(w * h);
            for y in 0..h {
                px.extend(rows.row(y).chunks_exact(4).map(|c| Color32::from_rgb(c[2], c[1], c[0])));
            }
            px
        })?;
        let image = ColorImage::new([w, h], pixels);
        self.queued.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(Ok(Shown { epoch: self.epoch, ts: f.ts, dur: f.dur, image }));
        self.ctx.request_repaint();
        Ok(())
    }
}
