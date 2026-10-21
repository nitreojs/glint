use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui;

use super::math::SECOND;
use super::media::{self, ALL, ANY, Com, Got};
use crate::platform::log::log;
use crate::record::Ctx;

/// One peak per 10 ms of sound.
pub(super) const BUCKET: i64 = SECOND / 100;
const BATCH: usize = 100;

struct Chunk {
    track: usize,
    at: usize,
    peaks: Vec<u8>,
}

/// Decodes every sound track in the background and streams their peaks in as they're found.
pub(super) struct Waves {
    rx: Receiver<Chunk>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Waves {
    pub(super) fn start(path: PathBuf, streams: Vec<u32>, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            if let Err(e) = run(&path, &streams, &tx, &stop, &ctx) {
                log(&format!("clip: drawing the waveforms stopped: {e}"));
            }
            ctx.request_repaint();
        });
        Waves { rx, cancel, thread: Some(thread) }
    }

    /// Moves what's arrived into `peaks`, one list per track.
    pub(super) fn poll(&self, peaks: &mut [Vec<u8>]) {
        while let Ok(c) = self.rx.try_recv() {
            let Some(v) = peaks.get_mut(c.track) else { continue };
            let end = c.at + c.peaks.len();
            if v.len() < end {
                v.resize(end, 0);
            }
            v[c.at..end].copy_from_slice(&c.peaks);
        }
    }
}

impl Drop for Waves {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Peaks per bucket, square-rooted so quiet sound still shows.
#[derive(Default)]
pub(super) struct Peaks {
    bucket: Option<usize>,
    peak: f32,
    at: usize,
    pending: Vec<u8>,
}

fn level(peak: f32) -> u8 {
    (peak.clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
}

impl Peaks {
    pub(super) fn push(&mut self, ts: i64, rate: u32, channels: usize, data: &[f32]) {
        for (i, frame) in data.chunks_exact(channels.max(1)).enumerate() {
            let t = ts + (i as i64 * SECOND) / rate as i64;
            let b = (t.max(0) / BUCKET) as usize;
            match self.bucket {
                None => {
                    self.bucket = Some(b);
                    self.at = b;
                }
                Some(cur) if b > cur => {
                    self.pending.push(level(self.peak));
                    self.pending.extend(std::iter::repeat_n(0, b - cur - 1));
                    self.bucket = Some(b);
                    self.peak = 0.0;
                }
                _ => {}
            }
            self.peak = frame.iter().fold(self.peak, |m, s| m.max(s.abs()));
        }
    }

    /// The finished peaks so far, from bucket `.0`; `all` closes the open bucket too.
    pub(super) fn take(&mut self, all: bool) -> Option<(usize, Vec<u8>)> {
        if all && self.bucket.is_some() {
            self.pending.push(level(self.peak));
            self.bucket = None;
            self.peak = 0.0;
        }
        if self.pending.is_empty() || (!all && self.pending.len() < BATCH) {
            return None;
        }
        let at = self.at;
        self.at += self.pending.len();
        Some((at, std::mem::take(&mut self.pending)))
    }
}

struct Track {
    stream: u32,
    rate: u32,
    channels: usize,
    peaks: Peaks,
    ended: bool,
}

fn run(path: &Path, streams: &[u32], tx: &Sender<Chunk>, stop: &AtomicBool, ctx: &egui::Context) -> Result<(), String> {
    let reader = media::open(path, None)?;
    unsafe { reader.SetStreamSelection(ALL, false).ctx("SetStreamSelection")? };
    let mut tracks: Vec<Option<Track>> = streams
        .iter()
        .map(|&stream| {
            unsafe { reader.SetStreamSelection(stream, true).ok()? };
            match media::float_native(&reader, stream) {
                Ok((rate, channels)) => Some(Track { stream, rate, channels, peaks: Peaks::default(), ended: false }),
                Err(e) => {
                    log(&format!("clip: waveform of stream {stream}: {e}"));
                    unsafe {
                        let _ = reader.SetStreamSelection(stream, false);
                    }
                    None
                }
            }
        })
        .collect();
    let mut scratch = Vec::new();
    let mut shown = Instant::now();
    let send = |k: usize, t: &mut Track, all: bool| {
        if let Some((at, peaks)) = t.peaks.take(all) {
            let _ = tx.send(Chunk { track: k, at, peaks });
        }
    };
    while tracks.iter().flatten().any(|t| !t.ended) && !stop.load(Ordering::Relaxed) {
        match media::read(&reader, ANY)? {
            Got::Sample { stream, ts, sample } => {
                let Some((k, t)) = tracks.iter_mut().enumerate().find_map(|(k, t)| Some((k, t.as_mut()?)).filter(|(_, t)| t.stream == stream))
                else {
                    continue;
                };
                media::floats(&sample, &mut scratch)?;
                t.peaks.push(ts, t.rate, t.channels, &scratch);
                send(k, t, false);
            }
            Got::End(stream) => {
                if let Some((k, t)) = tracks.iter_mut().enumerate().find_map(|(k, t)| Some((k, t.as_mut()?)).filter(|(_, t)| t.stream == stream)) {
                    t.ended = true;
                    send(k, t, true);
                }
            }
        }
        if shown.elapsed() > Duration::from_millis(60) {
            shown = Instant::now();
            ctx.request_repaint();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peaks_fill_buckets_in_order_with_silence_for_gaps() {
        let mut p = Peaks::default();
        let rate = 1000;
        let loud: Vec<f32> = (0..20).flat_map(|i| [if i == 3 { -1.0 } else { 0.0 }, 0.25]).collect();
        p.push(0, rate, 2, &loud);
        assert_eq!(p.take(false), None);
        p.push(5 * BUCKET, rate, 2, &[0.04, 0.0]);
        assert_eq!(p.take(true), Some((0, vec![255, 128, 0, 0, 0, 51])));
        assert_eq!(p.take(true), None);
        p.push(6 * BUCKET, rate, 1, &[0.0; 10]);
        assert_eq!(p.take(true), Some((6, vec![0])));
    }
}
