use std::collections::VecDeque;

pub const RATE: u32 = 48_000;
pub const HNS: i64 = 10_000_000;
const RESYNC_FRAMES: i64 = RATE as i64 / 50;

pub fn hns_to_frames(hns: i64) -> i64 {
    (hns as i128 * RATE as i128 / HNS as i128) as i64
}

pub fn frames_to_hns(frames: i64) -> i64 {
    (frames as i128 * HNS as i128 / RATE as i128) as i64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    F32,
    I16,
    I24,
    I32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Format {
    pub rate: u32,
    sample: Sample,
    pub block: usize,
    gains: Vec<(f32, f32)>,
}

const SPEAKER_GAINS: [(u32, (f32, f32)); 11] = [
    (0x1, (1.0, 0.0)),
    (0x2, (0.0, 1.0)),
    (0x4, (0.707, 0.707)),
    (0x8, (0.0, 0.0)),
    (0x10, (0.707, 0.0)),
    (0x20, (0.0, 0.707)),
    (0x40, (0.707, 0.0)),
    (0x80, (0.0, 0.707)),
    (0x100, (0.5, 0.5)),
    (0x200, (0.707, 0.0)),
    (0x400, (0.0, 0.707)),
];

fn default_mask(channels: usize) -> u32 {
    match channels {
        1 => 0x4,
        2 => 0x3,
        3 => 0x7,
        4 => 0x33,
        5 => 0x37,
        6 => 0x3F,
        7 => 0x13F,
        8 => 0x63F,
        _ => 0,
    }
}

impl Format {
    pub fn new(rate: u32, sample: Sample, channels: usize, mask: u32) -> Result<Format, String> {
        if channels == 0 || rate == 0 {
            return Err(format!("unsupported audio format: {channels} channels at {rate} Hz"));
        }
        let bytes = match sample {
            Sample::F32 | Sample::I32 => 4,
            Sample::I16 => 2,
            Sample::I24 => 3,
        };
        let gains = if channels == 1 {
            vec![(1.0, 1.0)]
        } else {
            let mask = if mask == 0 { default_mask(channels) } else { mask };
            let mut speakers = SPEAKER_GAINS.iter().filter(|(bit, _)| mask & bit != 0).map(|&(_, g)| g);
            (0..channels).map(|i| speakers.next().unwrap_or(if i % 2 == 0 { (0.5, 0.0) } else { (0.0, 0.5) })).collect()
        };
        Ok(Format { rate, sample, block: bytes * channels, gains })
    }

    pub fn decode(&self, data: &[u8], out: &mut Vec<f32>) {
        let ch = self.gains.len();
        let bytes = self.block / ch;
        out.reserve(data.len() / self.block * 2);
        for frame in data.chunks_exact(self.block) {
            let (mut l, mut r) = (0.0, 0.0);
            for (s, &(gl, gr)) in frame.chunks_exact(bytes).zip(&self.gains) {
                let v = match self.sample {
                    Sample::F32 => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    Sample::I16 => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
                    Sample::I24 => i32::from_le_bytes([0, s[0], s[1], s[2]]) as f32 / 2_147_483_648.0,
                    Sample::I32 => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                };
                l += v * gl;
                r += v * gr;
            }
            out.push(l);
            out.push(r);
        }
    }
}

pub(super) struct Resampler {
    step: f64,
    pos: f64,
    last: [f32; 2],
}

impl Resampler {
    pub(super) fn new(from: u32) -> Resampler {
        Resampler { step: from as f64 / RATE as f64, pos: 0.0, last: [0.0; 2] }
    }

    pub(super) fn reset(&mut self) {
        self.pos = 0.0;
        self.last = [0.0; 2];
    }

    pub(super) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.step == 1.0 {
            out.extend_from_slice(input);
            return;
        }
        let n = input.len() / 2;
        let at = |i: usize| if i == 0 { self.last } else { [input[2 * i - 2], input[2 * i - 1]] };
        while self.pos < n as f64 {
            let i = self.pos as usize;
            let t = (self.pos - i as f64) as f32;
            let (a, b) = (at(i), at(i + 1));
            out.push(a[0] + (b[0] - a[0]) * t);
            out.push(a[1] + (b[1] - a[1]) * t);
            self.pos += self.step;
        }
        if n > 0 {
            self.pos -= n as f64;
            self.last = at(n);
        }
    }
}

#[derive(Default)]
pub struct Mix {
    base: i64,
    buf: VecDeque<f32>,
}

impl Mix {
    pub(super) fn add(&mut self, pos: i64, samples: &[f32]) {
        let skip = (self.base - pos).max(0) as usize * 2;
        if skip >= samples.len() {
            return;
        }
        let at = (pos.max(self.base) - self.base) as usize * 2;
        let samples = &samples[skip..];
        if self.buf.len() < at + samples.len() {
            self.buf.resize(at + samples.len(), 0.0);
        }
        for (d, s) in self.buf.range_mut(at..).zip(samples) {
            *d += s;
        }
    }

    pub(super) fn base(&self) -> i64 {
        self.base
    }

    pub fn take(&mut self, upto: i64) -> Vec<f32> {
        let n = (upto - self.base).max(0) as usize * 2;
        let have = n.min(self.buf.len());
        let mut out: Vec<f32> = self.buf.drain(..have).collect();
        out.resize(n, 0.0);
        self.base = self.base.max(upto);
        out
    }
}

pub fn mix_tracks(sources: &[Vec<f32>], gains: &[f32], tracks: &[Vec<usize>]) -> Vec<Vec<i16>> {
    let len = sources.iter().map(Vec::len).max().unwrap_or(0);
    tracks
        .iter()
        .map(|members| {
            let mut sum = vec![0.0f32; len];
            for &m in members {
                for (d, s) in sum.iter_mut().zip(&sources[m]) {
                    *d += s * gains[m];
                }
            }
            sum.into_iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16).collect()
        })
        .collect()
}

/// loopback delivers no packets while nothing plays, so packets are placed by their timestamps.
pub struct Placement {
    resampler: Resampler,
    next: Option<i64>,
    scratch: Vec<f32>,
}

impl Placement {
    pub fn new(rate: u32) -> Placement {
        Placement { resampler: Resampler::new(rate), next: None, scratch: Vec::new() }
    }

    pub fn push(&mut self, at: Option<i64>, stereo: &[f32], discontinuity: bool, mix: &mut Mix) {
        let pos = match (self.next, at) {
            (Some(next), Some(at)) if !discontinuity && (at - next).abs() <= RESYNC_FRAMES => next,
            (Some(next), None) if !discontinuity => next,
            (_, at) => {
                self.resampler.reset();
                at.or(self.next).unwrap_or(mix.base())
            }
        };
        self.scratch.clear();
        self.resampler.process(stereo, &mut self.scratch);
        mix.add(pos, &self.scratch);
        self.next = Some(pos + (self.scratch.len() / 2) as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_f32(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn decodes_to_stereo() {
        let mut out = Vec::new();
        Format::new(48_000, Sample::F32, 2, 0).unwrap().decode(&bytes_f32(&[0.5, -0.25]), &mut out);
        assert_eq!(out, [0.5, -0.25]);

        out.clear();
        let mono: Vec<u8> = [16384i16, -32768].iter().flat_map(|x| x.to_le_bytes()).collect();
        Format::new(16_000, Sample::I16, 1, 0).unwrap().decode(&mono, &mut out);
        assert_eq!(out, [0.5, 0.5, -1.0, -1.0]);

        out.clear();
        let f51 = Format::new(48_000, Sample::F32, 6, 0).unwrap();
        f51.decode(&bytes_f32(&[0.1, 0.2, 0.3, 1.0, 0.4, 0.5]), &mut out);
        assert!((out[0] - (0.1 + 0.707 * 0.3 + 0.707 * 0.4)).abs() < 1e-6);
        assert!((out[1] - (0.2 + 0.707 * 0.3 + 0.707 * 0.5)).abs() < 1e-6);

        out.clear();
        Format::new(48_000, Sample::I24, 2, 0).unwrap().decode(&[0, 0, 0x40, 0, 0, 0xC0], &mut out);
        assert_eq!(out, [0.5, -0.5]);
    }

    #[test]
    fn resampling_keeps_the_rate() {
        let mut r = Resampler::new(44_100);
        let mut out = Vec::new();
        let input: Vec<f32> = (0..441 * 2).map(|i| (i / 2) as f32).collect();
        for chunk in input.chunks(2 * 37) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() / 2).abs_diff(480) <= 1);
        for (k, f) in out.chunks(2).enumerate().skip(2) {
            let want = k as f32 * 44_100.0 / 48_000.0 - 1.0;
            assert!((f[0] - want).abs() < 1e-3 && f[0] == f[1], "frame {k}: {} vs {want}", f[0]);
        }

        let mut same = Resampler::new(48_000);
        let mut out = Vec::new();
        same.process(&[0.1, 0.2, 0.3, 0.4], &mut out);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);

        let mut down = Resampler::new(96_000);
        let mut out = Vec::new();
        down.process(&vec![0.25; 2 * 960], &mut out);
        assert_eq!(out.len() / 2, 480);
    }

    #[test]
    fn mixing_sums_and_clips() {
        let mut m = Mix::default();
        m.add(0, &[0.5, -0.5, 0.75, 0.0]);
        m.add(1, &[0.5, -0.75, 0.25, 0.25]);
        let one = |m: &mut Mix, upto| mix_tracks(&[m.take(upto)], &[1.0], &[vec![0]]).remove(0);
        assert_eq!(one(&mut m, 3), [16384, -16384, 32767, -24575, 8192, 8192]);
        m.add(2, &[1.0, 1.0, 0.5, 0.5]);
        assert_eq!(one(&mut m, 5), [16384, 16384, 0, 0]);
        assert_eq!(m.base(), 5);
    }

    #[test]
    fn tracks_get_their_sources_at_their_volume() {
        let desktop = vec![0.25, -0.25, 0.5, 0.5];
        let app = vec![0.5, 0.5, 0.0, 0.0];
        let mic = vec![0.1, 0.1, 0.1, 0.1];
        let gains = [1.0, 2.0, 0.0];
        let tracks = [vec![0, 1, 2], vec![0], vec![1], vec![2], vec![]];
        let out = mix_tracks(&[desktop, app, mic], &gains, &tracks);
        assert_eq!(out.len(), 5);
        assert_eq!(out[0], [32767, 24575, 16384, 16384]);
        assert_eq!(out[1], [8192, -8192, 16384, 16384]);
        assert_eq!(out[2], [32767, 32767, 0, 0]);
        assert_eq!(out[3], [0; 4]);
        assert_eq!(out[4], [0; 4]);
        let quiet = mix_tracks(&[vec![0.5, 0.5]], &[0.5], &[vec![0]]);
        assert_eq!(quiet[0], [8192, 8192]);
    }

    #[test]
    fn silence_fills_gaps_by_the_clock() {
        let mut m = Mix::default();
        let mut p = Placement::new(48_000);
        let packet = vec![0.5f32; 2 * 480];
        p.push(Some(0), &packet, false, &mut m);
        p.push(Some(470), &packet, false, &mut m);
        p.push(Some(48_960), &packet, false, &mut m);
        let out = m.take(48_960 + 480);
        let frame = |i: usize| out[2 * i];
        assert_eq!((frame(0), frame(959)), (0.5, 0.5));
        assert_eq!((frame(960), frame(48_959)), (0.0, 0.0));
        assert_eq!((frame(48_960), frame(49_439)), (0.5, 0.5));

        let mut m = Mix::default();
        let mut p = Placement::new(48_000);
        p.push(Some(0), &[0.5; 4], false, &mut m);
        p.push(Some(10), &[0.5; 4], true, &mut m);
        assert_eq!(m.take(12)[2 * 2..2 * 10], [0.0; 16]);
    }
}
