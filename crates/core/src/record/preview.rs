use std::collections::VecDeque;
use std::path::Path;

use crate::clip::math::SECOND;
use crate::clip::timeline_cache::{BUCKET, Cache, Filmstrip, MAX_THUMBS, Thumb};

use super::mix::RATE;

pub const FRAME_STEP: i64 = SECOND / 2;
const PCM_BUCKET: usize = RATE as usize / 100;
const MAX_BUCKETS: usize = 8 * 60 * 60 * 100;

#[derive(Default)]
struct Peaks {
    first: usize,
    data: VecDeque<u8>,
    overflow: bool,
}

impl Peaks {
    fn push(&mut self, at: i64, pcm: &[i16]) {
        let Ok(mut frame) = usize::try_from(at) else { return };
        let mut pcm = pcm;
        while pcm.len() >= 2 {
            let bucket = frame / PCM_BUCKET;
            let n = (PCM_BUCKET - frame % PCM_BUCKET).min(pcm.len() / 2);
            let (batch, rest) = pcm.split_at(n * 2);
            if let Some(index) = bucket.checked_sub(self.first) {
                if index >= MAX_BUCKETS {
                    self.overflow = true;
                    return;
                }

                let max = batch.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                let peak = 1 + ((f32::from(max) / 32_768.0).sqrt() * 254.0).round() as u8;
                if self.data.len() <= index {
                    self.data.resize(index + 1, 1);
                }
                self.data[index] = self.data[index].max(peak);
            }
            frame += n;
            pcm = rest;
        }
    }

    fn trim(&mut self, at: i64) {
        let first = (at.max(0) / BUCKET) as usize;
        if first > self.first {
            self.data.drain(..(first - self.first).min(self.data.len()));
            self.first = first;
        }
    }

    fn slice(&self, start: i64, end: i64, audible: i64) -> Option<Vec<u8>> {
        let start = start.max(0);
        let first = (start / BUCKET) as usize;
        if self.overflow || first < self.first {
            return None;
        }

        let count = (end.saturating_sub(start).max(0) as u64).div_ceil(BUCKET as u64) as usize;
        if count > MAX_BUCKETS {
            return None;
        }

        let audible = audible.max(start);
        let peak = |bucket| self.data.get(bucket - self.first).copied().unwrap_or(1);
        let data = (0..count).map(|i| {
            let at = start + i as i64 * BUCKET;
            if at + BUCKET <= audible {
                return 1;
            }

            let n = (at.max(audible) / BUCKET) as usize;
            let last = (((at + BUCKET).min(end) - 1) / BUCKET) as usize;
            if last > n { peak(n).max(peak(last)) } else { peak(n) }
        }).collect();

        Some(data)
    }
}

#[derive(Default)]
pub struct CapturePreview {
    thumbs: VecDeque<Thumb>,
    audio: Vec<Peaks>,
    thumb_step: i64,
}

impl CapturePreview {
    pub fn add_track(&mut self) {
        self.audio.push(Peaks::default());
    }

    pub fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) {
        if let Some(peaks) = self.audio.get_mut(track) {
            peaks.push(at, pcm);
        }
    }

    pub fn thumbnail(&mut self, thumb: Thumb) {
        let step = self.thumb_step.max(FRAME_STEP);
        if self.thumbs.back().is_some_and(|last| thumb.at / step <= last.at / step) {
            return;
        }

        if self.thumbs.len() == MAX_THUMBS {
            self.thumb_step = step * 2;
            let mut i = 0;
            self.thumbs.retain(|_| {
                i += 1;

                i % 2 == 1
            });
        }
        self.thumbs.push_back(thumb);
    }

    pub fn trim(&mut self, at: i64) {
        while self.thumbs.front().is_some_and(|thumb| thumb.at < at) {
            self.thumbs.pop_front();
        }
        for peaks in &mut self.audio {
            peaks.trim(at);
        }
    }

    pub fn snapshot(&self, start: i64, end: i64, audio: &[(usize, i64)]) -> Preview {
        let thumbs = self.thumbs.iter().filter(|thumb| (start..end).contains(&thumb.at)).map(|thumb| {
            Thumb { at: thumb.at - start, size: thumb.size, rgb: thumb.rgb.clone() }
        }).collect();
        let peaks = audio.iter().map(|&(track, audible)| self.audio.get(track)?.slice(start, end, audible)).collect();

        Preview { thumbs, peaks }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview {
    thumbs: Vec<Thumb>,
    peaks: Option<Vec<Vec<u8>>>,
}

impl Preview {
    #[cfg(test)]
    pub(crate) fn parts(self) -> (Vec<Thumb>, Option<Vec<Vec<u8>>>) {
        (self.thumbs, self.peaks)
    }

    pub fn save(self, path: &Path) {
        let Some(cache) = Cache::for_file(path) else { return };
        if !self.thumbs.is_empty() {
            let _ = cache.save_filmstrip(&Filmstrip { thumbs: self.thumbs, cuts: None });
        }
        if let Some(peaks) = self.peaks.filter(|p| !p.is_empty()) {
            let _ = cache.save_waves(&peaks);
        }
    }
}
