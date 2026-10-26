//! A hovered recording or gif playing in its preview, muted and looping: a worker decodes it at
//! the preview's size (recordings on the GPU) and hands over each frame when it's due. One thread
//! plays every preview, one after another: a thread started while the window is drawing can hold
//! its presents up for tens of milliseconds (in the graphics driver), which a thread per preview
//! did on every hover.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, ColorImage};
use glint_core::log::log;
use glint_media::clip::media::{self, Com};

/// GIF delays this short are played at 100 ms, as browsers do.
const MIN_GIF_DELAY: u16 = 2;

pub(super) struct Playback {
    rx: Receiver<ColorImage>,
    /// Dropped to stop the worker.
    _stop: SyncSender<()>,
    /// How far it has played, in thousandths of its length.
    pos: Arc<AtomicU32>,
}

impl Playback {
    /// Plays `path` as a recording (`gif` false) or a gif, decoded at `size`.
    pub fn start(path: PathBuf, gif: bool, size: (u32, u32), length_ms: u32, ctx: &egui::Context) -> Playback {
        let (tx, rx) = mpsc::sync_channel(2);
        let (stop, stopped) = mpsc::sync_channel(1);
        let pos = Arc::new(AtomicU32::new(0));
        let out = Out { tx, stop: stopped, pos: pos.clone(), ctx: ctx.clone(), length_ms };
        send(Job { path, gif, size, out });
        Playback { rx, _stop: stop, pos }
    }

    /// The newest frame since the last call.
    pub fn poll(&self) -> Option<ColorImage> {
        self.rx.try_iter().last()
    }

    /// 0..=1 through the loop.
    pub fn progress(&self) -> f32 {
        self.pos.load(Ordering::Relaxed) as f32 / 1000.0
    }
}

/// A preview for the player thread to play.
struct Job {
    path: PathBuf,
    gif: bool,
    size: (u32, u32),
    out: Out,
}

static PLAYER: OnceLock<Option<Sender<Job>>> = OnceLock::new();

/// Hands `job` to the player thread, started on the first one.
fn send(job: Job) {
    let player = PLAYER.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new().name("preview".into()).spawn(move || serve(&rx)) {
            Ok(_) => Some(tx),
            Err(e) => {
                log(&format!("gallery: can't start the previews: {e}"));
                None
            }
        }
    });
    if let Some(tx) = player {
        let _ = tx.send(job);
    }
}

fn serve(jobs: &Receiver<Job>) {
    let _com = Com::init();
    for Job { path, gif, size, out } in jobs {
        // closed again before its turn came
        if out.gone() {
            continue;
        }
        let res = if gif { play_gif(&path, size, &out) } else { play_video(&path, size, &out) };
        if let Err(e) = res {
            log(&format!("gallery: can't play {}: {e}", path.display()));
        }
    }
}

struct Out {
    tx: SyncSender<ColorImage>,
    stop: Receiver<()>,
    pos: Arc<AtomicU32>,
    ctx: egui::Context,
    length_ms: u32,
}

impl Out {
    /// The preview is gone.
    fn gone(&self) -> bool {
        matches!(self.stop.try_recv(), Err(TryRecvError::Disconnected))
    }

    /// Waits until `due`; false once the preview is gone.
    fn wait(&self, due: Instant) -> bool {
        let left = due.saturating_duration_since(Instant::now());
        matches!(self.stop.recv_timeout(left), Err(RecvTimeoutError::Timeout))
    }

    fn show(&self, image: ColorImage, at_ms: u64) {
        if self.length_ms > 0 {
            let share = (at_ms % self.length_ms as u64) * 1000 / self.length_ms as u64;
            self.pos.store(share as u32, Ordering::Relaxed);
        }
        // a full queue means the window isn't drawing: drop the frame rather than wait
        let _ = self.tx.try_send(image);
        self.ctx.request_repaint();
    }
}

fn play_video(path: &Path, size: (u32, u32), out: &Out) -> Result<(), String> {
    let reader = media::rgb_reader_on(path, super::thumbs::gpu(), size)?;
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut start = Instant::now();
    // where this loop starts on the clock, and the first frame's time in the file (100 ns units)
    let (mut lap, mut first, mut end) = (0i64, None, 0i64);
    loop {
        let Some(f) = media::next_video(&reader)? else {
            if end <= 0 {
                return Err("the video has no frames".into());
            }
            media::seek(&reader, 0)?;
            lap += end - first.unwrap_or(0);
            first = None;
            continue;
        };
        let origin = *first.get_or_insert(f.ts);
        end = end.max(f.ts + f.dur.max(1));
        let at = lap + f.ts - origin;
        let due = start + Duration::from_nanos(at.max(0) as u64 * 100);
        let late = Instant::now().saturating_duration_since(due);
        if late > Duration::from_millis(250) {
            // fell far behind (the machine was busy): carry on from here instead of racing to catch up
            start += late;
        } else if late > Duration::from_millis(40) {
            continue;
        }
        let image = media::with_pixels(&f.sample, size.0, |rows| {
            let mut px = Vec::with_capacity(w * h);
            for y in 0..h {
                px.extend(rows.row(y).chunks_exact(4).map(|c| Color32::from_rgb(c[2], c[1], c[0])));
            }
            ColorImage::new([w, h], px)
        })?;
        if !out.wait(start + Duration::from_nanos(at.max(0) as u64 * 100)) {
            return Ok(());
        }
        out.show(image, (at / 10_000) as u64);
    }
}

fn gif_decoder(path: &Path, decode: bool) -> Result<gif::Decoder<BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut opts = gif::DecodeOptions::new();
    opts.set_color_output(gif::ColorOutput::RGBA);
    opts.skip_frame_decoding(!decode);
    opts.read_info(BufReader::new(file)).map_err(|e| e.to_string())
}

fn delay_ms(delay: u16) -> u64 {
    if delay < MIN_GIF_DELAY { 100 } else { delay as u64 * 10 }
}

/// How long one loop of a gif plays, in milliseconds (0 if it can't be read).
pub(super) fn gif_length(path: &Path) -> u32 {
    let Ok(mut d) = gif_decoder(path, false) else { return 0 };
    let mut total = 0u64;
    while let Ok(Some(f)) = d.read_next_frame() {
        total += delay_ms(f.delay);
    }
    total.min(u32::MAX as u64) as u32
}

fn play_gif(path: &Path, size: (u32, u32), out: &Out) -> Result<(), String> {
    let start = Instant::now();
    let mut clock = 0u64;
    loop {
        let mut d = gif_decoder(path, true)?;
        let (cw, ch) = (d.width() as usize, d.height() as usize);
        let mut canvas = vec![0u8; cw * ch * 4];
        let mut frames = 0;
        while let Some(f) = d.read_next_frame().map_err(|e| e.to_string())? {
            frames += 1;
            let dispose = f.dispose;
            let (fx, fy, fw, fh) = (f.left as usize, f.top as usize, f.width as usize, f.height as usize);
            let before = (dispose == gif::DisposalMethod::Previous).then(|| canvas.clone());
            for y in 0..fh.min(ch.saturating_sub(fy)) {
                for x in 0..fw.min(cw.saturating_sub(fx)) {
                    let src = &f.buffer[(y * fw + x) * 4..][..4];
                    if src[3] > 0 {
                        canvas[((fy + y) * cw + fx + x) * 4..][..4].copy_from_slice(src);
                    }
                }
            }
            let delay = delay_ms(f.delay);
            let image = shrink(&canvas, cw, ch, size.0 as usize, size.1 as usize);
            if !out.wait(start + Duration::from_millis(clock)) {
                return Ok(());
            }
            out.show(image, clock);
            clock += delay;
            match (dispose, before) {
                (gif::DisposalMethod::Background, _) => {
                    for y in fy..(fy + fh).min(ch) {
                        canvas[(y * cw + fx) * 4..(y * cw + (fx + fw).min(cw)) * 4].fill(0);
                    }
                }
                (gif::DisposalMethod::Previous, Some(b)) => canvas = b,
                _ => {}
            }
        }
        if frames <= 1 {
            // a still: nothing more to play
            return Ok(());
        }
    }
}

/// RGBA `src` averaged down to `dw` x `dh` (or as it is when that isn't smaller), premultiplied.
fn shrink(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> ColorImage {
    let (dw, dh) = (dw.clamp(1, sw.max(1)), dh.clamp(1, sh.max(1)));
    let mut px = Vec::with_capacity(dw * dh);
    for y in 0..dh {
        let (y0, y1) = (y * sh / dh, ((y + 1) * sh / dh).max(y * sh / dh + 1));
        for x in 0..dw {
            let (x0, x1) = (x * sw / dw, ((x + 1) * sw / dw).max(x * sw / dw + 1));
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for p in src[(sy * sw + x0) * 4..(sy * sw + x1) * 4].chunks_exact(4) {
                    let a = p[3] as u32;
                    sum[0] += p[0] as u32 * a;
                    sum[1] += p[1] as u32 * a;
                    sum[2] += p[2] as u32 * a;
                    sum[3] += a;
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let c = |v: u32| (v / (255 * n)) as u8;
            px.push(Color32::from_rgba_premultiplied(c(sum[0]), c(sum[1]), c(sum[2]), (sum[3] / n) as u8));
        }
    }
    ColorImage::new([dw, dh], px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gifs_shrink_by_averaging_and_never_grow() {
        // 2x2: white, black, transparent, white
        let src = [255, 255, 255, 255, 0, 0, 0, 255, 9, 9, 9, 0, 255, 255, 255, 255];
        let one = shrink(&src, 2, 2, 1, 1);
        assert_eq!(one.size, [1, 1]);
        // half of the three opaque pixels' worth of white over the four, premultiplied
        assert_eq!(one.pixels[0], Color32::from_rgba_premultiplied(127, 127, 127, 191));
        let same = shrink(&src, 2, 2, 8, 8);
        assert_eq!(same.size, [2, 2]);
        assert_eq!(same.pixels[1], Color32::BLACK);
        assert_eq!(same.pixels[2], Color32::TRANSPARENT);
        assert_eq!((delay_ms(0), delay_ms(1), delay_ms(2), delay_ms(7)), (100, 100, 20, 70));
    }
}
