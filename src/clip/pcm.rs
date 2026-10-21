use std::collections::VecDeque;

use super::math::SECOND;

const SLACK: i64 = 2;

pub(super) fn frames(hns: i64, rate: u32) -> i64 {
    (hns as i128 * rate as i128 + SECOND as i128 / 2).div_euclid(SECOND as i128) as i64
}

pub(super) fn hns(frames: i64, rate: u32) -> i64 {
    (frames as i128 * SECOND as i128 / rate as i128) as i64
}

pub(super) fn to_stereo(data: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    match channels {
        0 => {}
        1 => out.extend(data.iter().flat_map(|&s| [s, s])),
        2 => out.extend_from_slice(data),
        n => out.extend(data.chunks_exact(n).flat_map(|c| [c[0], c[1]])),
    }
}

pub(super) struct Lane {
    buf: VecDeque<f32>,
    start: i64,
}

impl Lane {
    pub(super) fn new(at: i64) -> Self {
        Lane { buf: VecDeque::new(), start: at }
    }

    pub(super) fn reset(&mut self, at: i64) {
        self.buf.clear();
        self.start = at;
    }

    pub(super) fn end(&self) -> i64 {
        self.start + self.buf.len() as i64 / 2
    }

    pub(super) fn push(&mut self, at: i64, stereo: &[f32]) {
        let end = self.end();
        let at = if (at - end).abs() <= SLACK { end } else { at };
        if at > end {
            self.buf.extend(std::iter::repeat_n(0.0, (at - end) as usize * 2));
        }
        let skip = ((end - at).max(0) as usize * 2).min(stereo.len());
        self.buf.extend(&stereo[skip..]);
    }

    pub(super) fn pad_to(&mut self, at: i64) {
        let end = self.end();
        if at > end {
            self.buf.extend(std::iter::repeat_n(0.0, (at - end) as usize * 2));
        }
    }

    pub(super) fn take_into(&mut self, out: &mut [f32], gain: f32) {
        let n = out.len().min(self.buf.len());
        if gain != 0.0 {
            for (o, s) in out.iter_mut().zip(self.buf.iter()) {
                *o += s * gain;
            }
        }
        self.buf.drain(..n);
        self.start += out.len() as i64 / 2;
    }
}

pub(super) fn to_i16(data: &[f32]) -> Vec<u8> {
    data.iter().flat_map(|&s| ((s.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_times_convert_at_the_rate() {
        assert_eq!(frames(SECOND, 48_000), 48_000);
        assert_eq!(frames(-SECOND / 2, 48_000), -24_000);
        assert_eq!(hns(1024, 48_000), 213_333);
        assert_eq!(frames(hns(48_000 * 7 + 13, 48_000), 48_000), 48_000 * 7 + 13);
    }

    #[test]
    fn lanes_place_audio_by_time_and_mix_with_gains() {
        let mut a = Lane::new(100);
        a.push(90, &[1.0; 40]);
        assert_eq!(a.end(), 110);
        a.push(111, &[1.0; 4]);
        assert_eq!(a.end(), 112);
        a.push(120, &[0.5; 4]);
        assert_eq!(a.end(), 122);
        let mut b = Lane::new(100);
        b.push(100, &[0.25; 8]);
        let mut out = vec![0.0; 2 * 22];
        a.take_into(&mut out, 1.0);
        b.take_into(&mut out, 2.0);
        assert_eq!(&out[..8], &[1.5; 8]);
        assert_eq!(&out[8..24], &[1.0; 16]);
        assert_eq!(&out[24..40], &[0.0; 16]);
        assert_eq!(&out[40..], &[0.5; 4]);
        assert_eq!((a.end(), b.end()), (122, 122));
        b.pad_to(130);
        assert_eq!(b.end(), 130);
        b.reset(5);
        assert_eq!(b.end(), 5);
    }

    #[test]
    fn channels_fold_to_stereo() {
        let mut out = Vec::new();
        to_stereo(&[0.1, 0.2], 1, &mut out);
        assert_eq!(out, [0.1, 0.1, 0.2, 0.2]);
        to_stereo(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3, &mut out);
        assert_eq!(out, [1.0, 2.0, 4.0, 5.0]);
        assert_eq!(to_i16(&[2.0, -1.0, 0.0]), [0xFF, 0x7F, 0x01, 0x80, 0, 0]);
    }
}
