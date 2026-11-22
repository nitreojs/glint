use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Context, Id, Mesh, Painter, Rect, Shape, pos2};
use glint_core::clip::edit::gain;
use glint_core::clip::math::SECOND;
use glint_core::clip::timeline_cache::Cache;
use glint_core::log::log;
use glint_core::record::mp4;
use glint_media::clip::media::{self, ALL, ANY, Com, Got};
use glint_media::record::Ctx;
use glint_ui::widgets::{Look, anim_ctx, ease, mix, motion};
use windows::Win32::Media::MediaFoundation::IMFSourceReader;

use super::overlay::danger;

pub(super) use glint_core::clip::timeline_cache::BUCKET;
const BATCH: usize = 25;
const UNLOADED: u8 = 0;
const SILENCE: u8 = 1;
const MIN_SECTION: i64 = 10 * SECOND;
const SEEK_PREROLL: i64 = SECOND / 4;

pub(super) const WAVES: [Color32; 6] = [
    Color32::from_rgb(0x6A, 0xA7, 0xE8),
    Color32::from_rgb(0x9A, 0xD1, 0x7A),
    Color32::from_rgb(0xD6, 0x8A, 0xE0),
    Color32::from_rgb(0xE8, 0xA8, 0x6A),
    Color32::from_rgb(0x6A, 0xD1, 0xC4),
    Color32::from_rgb(0xE0, 0xD4, 0x6A),
];

pub(super) struct Waveform {
    pub(super) color: Color32,
    hot: Color32,
    gain: f32,
}

impl Waveform {
    pub(super) fn recorded(
        ctx: &Context,
        look: &Look,
        path: &Path,
        source: usize,
        volume: u16,
        muted: bool,
    ) -> Self {
        let id = Id::unique(("recorded-wave", path, source));

        Self::new(ctx, look, id, WAVES[source % WAVES.len()], volume, muted)
    }

    fn new(
        ctx: &Context,
        look: &Look,
        id: Id,
        hue: Color32,
        volume: u16,
        muted: bool,
    ) -> Self {
        let light = ctx.theme() == egui::Theme::Light;
        let ink = if light { mix(hue, Color32::BLACK, 0.48) } else { hue };
        let mute = ease(anim_ctx(ctx, id.with("mute"), muted, 0.3));
        // cached peaks are square roots of the linear sample amplitudes
        let target = gain(volume).sqrt();
        let gain = if motion() {
            ctx.animate_value_with_time(id.with("gain"), target, 0.15)
        } else {
            target
        };

        Self {
            color: mix(ink, look.p.t3, mute),
            hot: mix(danger(light), look.p.t3, mute),
            gain,
        }
    }

    pub(super) fn paint(
        &self,
        painter: &Painter,
        rect: Rect,
        peaks: &[u8],
        source_at: impl Fn(f32) -> i64,
    ) {
        const PITCH: f32 = 2.0;
        const WIDTH: f32 = 1.5;

        let visible = rect.intersect(painter.clip_rect());
        if !visible.is_positive() || rect.height() < 3.0 || peaks.is_empty() {
            return;
        }

        let first = ((visible.left() - rect.left()) / PITCH).floor() as usize;
        let last = ((visible.right() - rect.left()) / PITCH).ceil() as usize;
        let mut at = source_at(rect.left() + first as f32 * PITCH).max(0) as u64;
        if at / BUCKET as u64 >= peaks.len() as u64 {
            return;
        }

        let cy = rect.center().y;
        let room = rect.height() * 0.8;
        let ceiling = rect.height() - 2.0;
        let quiet = self.color.gamma_multiply(0.35);
        let mut mesh = Mesh {
            indices: Vec::with_capacity(6 * (last - first)),
            vertices: Vec::with_capacity(4 * (last - first)),
            ..Mesh::default()
        };
        for column in first..last {
            let start = (at / BUCKET as u64) as usize;
            if start >= peaks.len() {
                break;
            }

            let x = rect.left() + column as f32 * PITCH;
            at = source_at((x + PITCH).min(rect.right())).max(0) as u64;
            let end = at.div_ceil(BUCKET as u64) as usize;
            let end = end.clamp(start + 1, peaks.len());
            let peak = peaks[start..end].iter().copied().max().unwrap_or(UNLOADED);
            if peak == UNLOADED {
                continue;
            }

            let level = f32::from(peak - SILENCE) / 254.0 * self.gain;
            let (height, color) = if level == 0.0 {
                if column % 3 != 0 {
                    continue;
                }

                (1.0, quiet)
            } else {
                ((level * room).clamp(1.0, ceiling), if level > 1.0 { self.hot } else { self.color })
            };
            let right = (x + WIDTH).min(rect.right());
            let bar = Rect::from_min_max(pos2(x, cy - height / 2.0), pos2(right, cy + height / 2.0));
            mesh.add_colored_rect(bar, color);
        }

        if !mesh.is_empty() {
            painter.with_clip_rect(visible).add(Shape::mesh(mesh));
        }
    }
}

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
    pub(super) fn start(path: PathBuf, streams: Vec<u32>, duration: i64, ctx: egui::Context) -> Self {
        let parallelism = thread::available_parallelism().map_or(1, |n| n.get());
        let sections = (duration / MIN_SECTION).clamp(1, parallelism.saturating_sub(1).clamp(1, 4) as i64);

        Self::launch(path, streams, duration, sections, true, ctx)
    }

    #[cfg(test)]
    pub(super) fn start_sections(
        path: PathBuf,
        streams: Vec<u32>,
        duration: i64,
        sections: i64,
        ctx: egui::Context,
    ) -> Self {
        Self::launch(path, streams, duration, sections, false, ctx)
    }

    fn launch(
        path: PathBuf,
        streams: Vec<u32>,
        duration: i64,
        sections: i64,
        compact: bool,
        ctx: egui::Context,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let thread = thread::spawn(move || {
            let cache = compact.then(|| Cache::for_file(&path)).flatten();
            if let Some(peaks) = cache.as_ref().and_then(|cache| cache.waves(streams.len())) {
                for (track, peaks) in peaks.into_iter().enumerate() {
                    let _ = tx.send(Chunk { track, at: 0, peaks });
                }
                ctx.request_repaint();
                return;
            }

            let saved = cache.as_ref().map(|_| Mutex::new(vec![Vec::new(); streams.len()]));
            let emit = |chunk: Chunk| {
                if let Some(saved) = &saved {
                    merge(&mut saved.lock().unwrap()[chunk.track], &chunk);
                }
                let _ = tx.send(chunk);
            };
            let _com = Com::init();
            let mut complete = false;
            if compact {
                let view = mp4::audio_view(&path, || stop.load(Ordering::Relaxed))
                    .and_then(|bytes| media::audio_view_reader(&bytes));
                if let Ok((reader, order)) = view
                    && order.len() == streams.len()
                    && run(&reader, &order, 0..duration, &emit, &stop, &ctx).is_ok()
                {
                    complete = true;
                }
            }

            if !complete && !stop.load(Ordering::Relaxed) {
                if let Some(saved) = &saved {
                    saved.lock().unwrap().iter_mut().for_each(Vec::clear);
                }
                complete = run_sections(&path, &streams, duration, sections, &emit, &stop, &ctx);
            }
            if complete && !stop.load(Ordering::Relaxed)
                && let (Some(cache), Some(saved)) = (cache, saved)
            {
                let _ = cache.save_waves(&saved.into_inner().unwrap());
            }
            ctx.request_repaint();
        });

        Waves { rx, cancel, thread: Some(thread) }
    }

    /// Moves what's arrived into `peaks`, one list per track.
    pub(super) fn poll(&self, peaks: &mut [Vec<u8>]) {
        while let Ok(c) = self.rx.try_recv() {
            let Some(v) = peaks.get_mut(c.track) else { continue };
            merge(v, &c);
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

fn merge(peaks: &mut Vec<u8>, chunk: &Chunk) {
    let end = chunk.at + chunk.peaks.len();
    if peaks.len() < end {
        peaks.resize(end, UNLOADED);
    }
    peaks[chunk.at..end].copy_from_slice(&chunk.peaks);
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
    (peak.clamp(0.0, 1.0).sqrt() * 254.0).round() as u8 + SILENCE
}

impl Peaks {
    fn push(&mut self, ts: i64, rate: u32, channels: usize, data: &[f32], range: &Range<i64>) -> bool {
        let frames = (data.len() / channels) as i64;
        let rate = i64::from(rate);
        let frame_at = |at: i64| ((at - ts).max(0) * rate + SECOND - 1) / SECOND;
        let end = frame_at(range.end);
        let limit = end.min(frames);
        let mut i = frame_at(range.start);
        while i < limit {
            let t = ts + i * SECOND / rate;
            let b = (t / BUCKET) as usize;
            match self.bucket {
                None => {
                    self.bucket = Some(b);
                    self.at = b;
                }
                Some(cur) if b > cur => {
                    self.pending.push(level(self.peak));
                    self.pending.extend(std::iter::repeat_n(SILENCE, b - cur - 1));
                    self.bucket = Some(b);
                    self.peak = 0.0;
                }
                _ => {}
            }

            let next = frame_at((b as i64 + 1) * BUCKET).min(limit);
            let samples = &data[i as usize * channels..next as usize * channels];
            self.peak = samples.iter().fold(self.peak, |m, s| m.max(s.abs()));
            i = next;
        }

        end <= frames
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

fn run_sections(
    path: &Path,
    streams: &[u32],
    duration: i64,
    sections: i64,
    emit: &(impl Fn(Chunk) + Sync),
    stop: &AtomicBool,
    ctx: &egui::Context,
) -> bool {
    let buckets = duration.div_euclid(BUCKET);
    let failed = AtomicBool::new(false);
    thread::scope(|scope| {
        for section in 0..sections {
            let start = buckets * section / sections * BUCKET;
            let end = if section + 1 == sections {
                duration
            } else {
                buckets * (section + 1) / sections * BUCKET
            };
            let failed = &failed;

            scope.spawn(move || {
                let _com = Com::init();
                let result = media::open(path, None)
                    .and_then(|reader| run(&reader, streams, start..end, emit, stop, ctx));
                if let Err(e) = result {
                    failed.store(true, Ordering::Relaxed);
                    if !stop.load(Ordering::Relaxed) && path.exists() {
                        log(&format!("clip: drawing the waveforms stopped: {e}"));
                    }
                }
                ctx.request_repaint();
            });
        }
    });

    !failed.load(Ordering::Relaxed)
}

fn run(
    reader: &IMFSourceReader,
    streams: &[u32],
    range: Range<i64>,
    emit: &impl Fn(Chunk),
    stop: &AtomicBool,
    ctx: &egui::Context,
) -> Result<(), String> {
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
    if range.start > 0 {
        let at = (range.start - SEEK_PREROLL).max(0);
        if let Err(e) = media::seek(&reader, at) {
            log(&format!("clip: waveform seek failed, reading from the start: {e}"));
        }
    }

    let mut scratch = Vec::new();
    let mut shown = Instant::now();
    let send = |k: usize, t: &mut Track, all: bool| {
        if let Some((at, peaks)) = t.peaks.take(all) {
            emit(Chunk { track: k, at, peaks });
        }
    };
    while tracks.iter().flatten().any(|t| !t.ended) && !stop.load(Ordering::Relaxed) {
        let got = media::read(&reader, ANY)?;
        let stream = match &got {
            Got::Sample { stream, .. } | Got::End(stream) => *stream,
        };
        let Some((k, track)) = tracks.iter_mut().enumerate().filter_map(|(k, t)| Some((k, t.as_mut()?)))
            .find(|(_, t)| t.stream == stream && !t.ended)
        else {
            continue;
        };
        let done = match got {
            Got::Sample { ts, sample, .. } if ts < range.end => {
                media::floats(&sample, &mut scratch)?;
                track.peaks.push(ts, track.rate, track.channels, &scratch, &range)
            }
            _ => true,
        };
        send(k, track, done);
        if done {
            track.ended = true;
            unsafe { reader.SetStreamSelection(stream, false).ctx("SetStreamSelection")? };
        }
        if shown.elapsed() > Duration::from_millis(60) {
            shown = Instant::now();
            ctx.request_repaint();
        }
    }
    if tracks.iter().any(Option::is_none) {
        return Err("one or more waveform tracks couldn't be opened".into());
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
        let range = 0..7 * BUCKET;
        let loud: Vec<f32> = (0..20).flat_map(|i| [if i == 3 { -1.0 } else { 0.0 }, 0.25]).collect();
        p.push(0, rate, 2, &loud, &range);
        assert_eq!(p.take(false), None);
        p.push(5 * BUCKET, rate, 2, &[0.04, 0.0], &range);
        assert_eq!(p.take(true), Some((0, vec![255, 128, 1, 1, 1, 52])));
        assert_eq!(p.take(true), None);
        p.push(6 * BUCKET, rate, 1, &[0.0; 10], &range);
        assert_eq!(p.take(true), Some((6, vec![1])));
    }
}
