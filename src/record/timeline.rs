use super::mix::{RATE, frames_to_hns, hns_to_frames};

/// capture-clock spans in 100 ns units; the last may still be open.
#[derive(Default, Debug)]
pub(super) struct Pauses {
    spans: Vec<(i64, Option<i64>)>,
}

impl Pauses {
    pub(super) fn paused(&self) -> bool {
        self.spans.last().is_some_and(|s| s.1.is_none())
    }

    pub(super) fn pause(&mut self, t: i64) {
        if !self.paused() {
            self.spans.push((t, None));
        }
    }

    pub(super) fn resume(&mut self, t: i64) {
        if let Some(s) = self.spans.last_mut().filter(|s| s.1.is_none()) {
            s.1 = Some(t.max(s.0));
        }
    }

    pub(super) fn active(&self, t: i64) -> Option<i64> {
        let mut out = t;
        for &(from, to) in &self.spans {
            if t < from {
                break;
            }
            match to {
                Some(to) if t >= to => out -= to - from,
                _ => return None,
            }
        }
        Some(out)
    }

    pub(super) fn active_clamped(&self, t: i64) -> i64 {
        let mut out = t;
        for &(from, to) in &self.spans {
            if t < from {
                break;
            }
            match to {
                Some(to) if t >= to => out -= to - from,
                _ => return out - (t - from),
            }
        }
        out
    }
}

#[derive(Default)]
pub(super) struct AudioPlacer {
    pub next: i64,
}

impl AudioPlacer {
    pub(super) fn place(&mut self, start: i64, data: &[i16], pauses: &Pauses, origin: i64) -> Vec<i16> {
        let tolerance = RATE as i64 / 50;
        let mut out = Vec::with_capacity(data.len());
        for (i, frame) in data.chunks_exact(2).enumerate() {
            let Some(t) = pauses.active(start + frames_to_hns(i as i64)) else { continue };
            if t < origin {
                continue;
            }
            let target = hns_to_frames(t - origin);
            let pos = self.next + (out.len() / 2) as i64;
            if target > pos + tolerance {
                out.resize(out.len() + 2 * (target - pos) as usize, 0);
            } else if target < pos - tolerance {
                continue;
            }
            out.extend_from_slice(frame);
        }
        self.next += (out.len() / 2) as i64;
        out
    }

    pub(super) fn pad(&mut self, end: i64) -> Vec<i16> {
        let n = (hns_to_frames(end) - self.next).max(0);
        self.next += n;
        vec![0; 2 * n as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 10_000;

    #[test]
    fn pauses_shift_time() {
        let mut p = Pauses::default();
        p.pause(100 * MS);
        assert!(p.paused());
        assert_eq!(p.active(50 * MS), Some(50 * MS));
        assert_eq!(p.active(150 * MS), None);
        p.resume(300 * MS);
        p.resume(400 * MS);
        assert!(!p.paused());
        assert_eq!(p.active(300 * MS), Some(100 * MS));
        assert_eq!(p.active(350 * MS), Some(150 * MS));
        p.pause(500 * MS);
        p.pause(550 * MS);
        p.resume(600 * MS);
        assert_eq!(p.active(700 * MS), Some(400 * MS));
        assert_eq!(p.active_clamped(550 * MS), 300 * MS);
        assert_eq!(p.active_clamped(250 * MS), 100 * MS);
        assert_eq!(p.active_clamped(700 * MS), 400 * MS);
        p.pause(800 * MS);
        assert_eq!(p.active_clamped(900 * MS), 500 * MS);
    }

    #[test]
    fn audio_skips_paused_frames_without_a_gap() {
        let mut p = Pauses::default();
        p.pause(10 * MS);
        p.resume(30 * MS);
        let mut a = AudioPlacer::default();
        let data: Vec<i16> = (0..1920i16).flat_map(|i| [i, i]).collect();
        let out = a.place(0, &data, &p, 0);
        assert_eq!(out.len() / 2, 960);
        assert_eq!((out[2 * 479], out[2 * 480]), (479, 1440));
        assert_eq!(a.next, 960);
    }

    #[test]
    fn audio_aligns_to_the_first_video_frame() {
        let p = Pauses::default();
        let ramp: Vec<i16> = (0..480i16).flat_map(|i| [i, i]).collect();
        let mut a = AudioPlacer::default();
        let out = a.place(0, &ramp, &p, 5 * MS);
        assert_eq!((out.len() / 2, out[0]), (240, 240));
        let mut a = AudioPlacer::default();
        let out = a.place(55 * MS, &ramp, &p, 5 * MS);
        assert_eq!(out.len() / 2, 2400 + 480);
        assert_eq!((out[2 * 2399], out[2 * 2400]), (0, 0));
        assert_eq!(out[2 * 2401], 1);
        let out = a.place(65 * MS, &ramp, &p, 5 * MS);
        assert_eq!((out.len() / 2, a.next), (480, 3360));
        assert_eq!(a.pad(100 * MS).len() / 2, 4800 - 3360);
    }
}
