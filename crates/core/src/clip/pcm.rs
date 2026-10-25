use std::collections::VecDeque;

use super::math::SECOND;

const SLACK: i64 = 2;
/// Where [`soft_clip`] starts bending: anything quieter passes untouched.
const KNEE: f32 = 0.9;
/// How long a gain takes to slide from silence to full: quick enough to feel instant, slow enough
/// not to click.
pub const RAMP: i64 = SECOND / 100;

pub fn frames(hns: i64, rate: u32) -> i64 {
    (hns as i128 * rate as i128 + SECOND as i128 / 2).div_euclid(SECOND as i128) as i64
}

pub fn hns(frames: i64, rate: u32) -> i64 {
    (frames as i128 * SECOND as i128 / rate as i128) as i64
}

pub fn to_stereo(data: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    match channels {
        0 => {}
        1 => out.extend(data.iter().flat_map(|&s| [s, s])),
        2 => out.extend_from_slice(data),
        n => out.extend(data.chunks_exact(n).flat_map(|c| [c[0], c[1]])),
    }
}

pub struct Lane {
    buf: VecDeque<f32>,
    start: i64,
}

impl Lane {
    pub fn new(at: i64) -> Self {
        Lane { buf: VecDeque::new(), start: at }
    }

    pub fn reset(&mut self, at: i64) {
        self.buf.clear();
        self.start = at;
    }

    pub fn end(&self) -> i64 {
        self.start + self.buf.len() as i64 / 2
    }

    pub fn push(&mut self, at: i64, stereo: &[f32]) {
        let end = self.end();
        let at = if (at - end).abs() <= SLACK { end } else { at };
        if at > end {
            self.buf.extend(std::iter::repeat_n(0.0, (at - end) as usize * 2));
        }
        let skip = ((end - at).max(0) as usize * 2).min(stereo.len());
        self.buf.extend(&stereo[skip..]);
    }

    pub fn pad_to(&mut self, at: i64) {
        let end = self.end();
        if at > end {
            self.buf.extend(std::iter::repeat_n(0.0, (at - end) as usize * 2));
        }
    }

    pub fn take_into(&mut self, out: &mut [f32], gain: f32) {
        let n = out.len().min(self.buf.len());
        if gain != 0.0 {
            for (o, s) in out.iter_mut().zip(self.buf.iter()) {
                *o += s * gain;
            }
        }
        self.buf.drain(..n);
        self.start += out.len() as i64 / 2;
    }

    /// [`Lane::take_into`] with `gain` sliding toward `to` by `step` a frame, so a change while it
    /// plays fades in over a few milliseconds instead of jumping.
    pub fn take_ramped(&mut self, out: &mut [f32], gain: &mut f32, to: f32, step: f32) {
        if *gain == to {
            return self.take_into(out, to);
        }
        let mut src = self.buf.iter();
        for o in out.chunks_exact_mut(2) {
            *gain = if *gain < to { (*gain + step).min(to) } else { (*gain - step).max(to) };
            if let (Some(l), Some(r)) = (src.next(), src.next()) {
                o[0] += l * *gain;
                o[1] += r * *gain;
            }
        }
        let n = out.len().min(self.buf.len());
        self.buf.drain(..n);
        self.start += out.len() as i64 / 2;
    }
}

/// The per-frame step that slides a gain by 1.0 over [`RAMP`].
pub fn ramp_step(rate: u32) -> f32 {
    1.0 / frames(RAMP, rate).max(1) as f32
}

/// `s` as is up to the knee, then bent smoothly toward full scale, so sound boosted (or summed)
/// past it saturates gently instead of clipping hard.
pub fn soft_clip(s: f32) -> f32 {
    let a = s.abs();
    if a <= KNEE {
        return s;
    }
    let room = 1.0 - KNEE;
    (KNEE + room * ((a - KNEE) / room).tanh()).copysign(s)
}

pub fn to_i16(data: &[f32]) -> Vec<u8> {
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

    #[test]
    fn gain_changes_slide_over_the_ramp() {
        assert_eq!(ramp_step(48_000), 1.0 / 480.0);
        let mut lane = Lane::new(0);
        lane.push(0, &[0.5; 2 * 600]);
        let (mut out, mut gain) = (vec![0.0; 2 * 600], 1.0);
        lane.take_ramped(&mut out, &mut gain, 0.0, ramp_step(48_000));
        assert_eq!(gain, 0.0);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        assert!(left.windows(2).all(|w| w[1] <= w[0] && w[0] - w[1] < 0.5 / 400.0), "no step bigger than the ramp's");
        assert!(left[0] > 0.49 && left[479] < 0.01 && left[480..].iter().all(|&s| s == 0.0));
        assert_eq!(lane.end(), 600);

        lane.push(600, &[0.5; 8]);
        let mut out = vec![0.0; 8];
        lane.take_ramped(&mut out, &mut gain, 0.0, ramp_step(48_000));
        assert_eq!(out, [0.0; 8], "a settled gain mixes like take_into");
    }

    #[test]
    fn soft_clipping_leaves_quiet_sound_alone_and_never_passes_full_scale() {
        for s in [0.0, 0.3, -0.5, 0.9, -0.9] {
            assert_eq!(soft_clip(s), s);
        }
        let curve: Vec<f32> = (0..=400).map(|i| soft_clip(i as f32 / 100.0)).collect();
        assert!(curve.windows(2).all(|w| w[1] >= w[0]), "it only bends, never folds back");
        assert!(curve.iter().all(|&s| s <= 1.0) && curve[400] > 0.999);
        assert!((soft_clip(0.91) - 0.91).abs() < 1e-4, "smooth at the knee");
        assert_eq!(soft_clip(-2.0), -soft_clip(2.0));
    }
}
