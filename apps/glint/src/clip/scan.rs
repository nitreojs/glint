use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

use eframe::egui::{self, Color32, ColorImage};
use glint_core::clip::math::SECOND;
use glint_core::log::log;
use glint_media::clip::media::{self, Com};

const THUMB_H: u32 = 64;
const THUMBS: i64 = 240;
const CUT_DIFF: f32 = 30.0;
const CUT_RATIO: f32 = 4.0;
const CUT_GAP: i64 = SECOND / 2;

pub(super) enum Found {
    Cut(i64),
    Thumb(i64, ColorImage),
}

pub(super) struct Scan {
    rx: Receiver<Found>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub(super) fn interval(duration: i64) -> i64 {
    (duration / THUMBS).max(SECOND / 4)
}

impl Scan {
    pub(super) fn start(path: PathBuf, size: (u32, u32), duration: i64, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let w = ((THUMB_H as f32 * size.0 as f32 / size.1.max(1) as f32).round() as u32).clamp(8, 512) & !1;
        let thumb = (w, THUMB_H);
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            let res = (|| -> Result<(), String> {
                let reader = media::rgb_reader(&path, thumb)?;
                let every = interval(duration);
                let (mut next_thumb, mut cuts) = (0, Cuts::default());
                let mut luma = Vec::new();
                while !stop.load(Ordering::Relaxed) {
                    let Some(f) = media::next_video(&reader)? else { break };
                    let (tw, th) = (thumb.0 as usize, thumb.1 as usize);
                    let take = f.ts >= next_thumb;
                    let mut px = Vec::with_capacity(if take { tw * th } else { 0 });
                    media::with_pixels(&f.sample, thumb.0, |rows| {
                        luma.clear();
                        for y in 0..th {
                            for c in rows.row(y).chunks_exact(4) {
                                luma.push(((c[2] as u32 * 77 + c[1] as u32 * 150 + c[0] as u32 * 29) >> 8) as u8);
                                if take {
                                    px.push(Color32::from_rgb(c[2], c[1], c[0]));
                                }
                            }
                        }
                    })?;
                    let mut sent = false;
                    if let Some(t) = cuts.push(f.ts, &luma) {
                        sent |= tx.send(Found::Cut(t)).is_ok();
                    }
                    if take {
                        next_thumb = f.ts - f.ts % every + every;
                        sent |= tx.send(Found::Thumb(f.ts, ColorImage::new([tw, th], px))).is_ok();
                    }
                    if sent {
                        ctx.request_repaint();
                    }
                }
                Ok(())
            })();
            if let Err(e) = res
                && !stop.load(Ordering::Relaxed)
                && path.exists()
            {
                log(&format!("clip: scanning for scene cuts stopped: {e}"));
            }
        });
        Scan { rx, cancel, thread: Some(thread) }
    }

    pub(super) fn poll(&self) -> Option<Found> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Default)]
struct Cuts {
    prev: Vec<u8>,
    avg: f32,
    last: Option<i64>,
}

impl Cuts {
    fn push(&mut self, ts: i64, luma: &[u8]) -> Option<i64> {
        let mut cut = None;
        if self.prev.len() == luma.len() && !luma.is_empty() {
            let sum: u64 = self.prev.iter().zip(luma).map(|(&a, &b)| a.abs_diff(b) as u64).sum();
            let d = sum as f32 / luma.len() as f32;
            if d > CUT_DIFF && d > self.avg * CUT_RATIO && self.last.is_none_or(|l| ts - l >= CUT_GAP) {
                self.last = Some(ts);
                cut = Some(ts);
            }
            self.avg = self.avg * 0.9 + d * 0.1;
        }
        self.prev.clear();
        self.prev.extend_from_slice(luma);
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_need_a_big_sudden_change_and_some_distance() {
        let mut c = Cuts::default();
        let (dark, light, grey) = ([10u8; 64], [200u8; 64], [30u8; 64]);
        assert_eq!(c.push(0, &dark), None);
        assert_eq!(c.push(1, &dark), None);
        assert_eq!(c.push(2, &grey), None);
        assert_eq!(c.push(SECOND, &light), Some(SECOND));
        assert_eq!(c.push(SECOND + 1, &dark), None);
        for k in 0..30 {
            c.push(2 * SECOND + k, &dark);
        }
        assert_eq!(c.push(3 * SECOND, &light), Some(3 * SECOND));
    }
}
