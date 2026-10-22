use super::math::SECOND;

pub const GLIDE_SECS: f64 = 0.12;
const SNAP: i64 = SECOND / 4;
const CATCH_UP: f64 = 0.15;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glide {
    from: i64,
    to: i64,
    start: f64,
    played: Option<(i64, f64)>,
}

impl Glide {
    pub fn new(t: i64) -> Glide {
        Glide { from: t, to: t, start: f64::NEG_INFINITY, played: None }
    }

    pub fn set(&mut self, t: i64) {
        *self = Glide::new(t);
    }

    pub fn glide(&mut self, now: f64, t: i64, motion: bool, ease: impl Fn(f32) -> f32) {
        if t == self.to && self.played.is_none() {
            return;
        }
        let from = self.at(now, ease);
        *self = Glide { from, to: t, start: if motion { now } else { f64::NEG_INFINITY }, played: None };
    }

    pub fn play(&mut self, now: f64, clock: i64) -> i64 {
        let next = match self.played {
            Some((t, at)) => {
                let dt = (now - at).max(0.0);
                let guess = t + (dt * SECOND as f64) as i64;
                let off = clock - guess;
                if off.abs() > SNAP { clock } else { (guess + (off as f64 * (dt / CATCH_UP).min(1.0)) as i64).max(t) }
            }
            None => clock,
        };
        *self = Glide { from: next, to: next, start: f64::NEG_INFINITY, played: Some((next, now)) };
        next
    }

    pub fn at(&self, now: f64, ease: impl Fn(f32) -> f32) -> i64 {
        let k = ((now - self.start) / GLIDE_SECS).clamp(0.0, 1.0) as f32;
        self.from + ((self.to - self.from) as f64 * ease(k) as f64).round() as i64
    }

    pub fn moving(&self, now: f64) -> bool {
        now - self.start < GLIDE_SECS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = SECOND;
    const MS: i64 = SECOND / 1000;

    fn linear(t: f32) -> f32 {
        t
    }

    fn smooth(t: f32) -> f32 {
        t * t * (3.0 - 2.0 * t)
    }

    #[test]
    fn a_jump_glides_from_where_it_was_drawn_and_lands_on_time() {
        let mut g = Glide::new(10 * S);
        g.glide(1.0, 20 * S, true, linear);
        assert_eq!(g.at(1.0, linear), 10 * S);
        assert_eq!(g.at(1.0 + GLIDE_SECS / 2.0, linear), 15 * S);
        assert_eq!(g.at(1.0 + GLIDE_SECS / 2.0, smooth), 15 * S);
        assert!(g.at(1.0 + GLIDE_SECS / 4.0, smooth) < g.at(1.0 + GLIDE_SECS / 4.0, linear), "eased: slow start");
        assert!(g.moving(1.0 + GLIDE_SECS * 0.9));
        assert_eq!(g.at(1.0 + GLIDE_SECS, linear), 20 * S);
        assert!(!g.moving(1.0 + GLIDE_SECS));

        let same = g;
        g.glide(1.05, 20 * S, true, linear);
        assert_eq!(g, same, "asking for the same spot every frame doesn't restart it");

        g.glide(1.06, 0, true, linear);
        let mid = 10 * S + 10 * S / 2;
        assert_eq!(g.at(1.06, linear), mid, "a new jump mid-glide starts from what's drawn");
        assert_eq!(g.at(1.06 + GLIDE_SECS, linear), 0);
    }

    #[test]
    fn dragging_and_reduced_motion_never_lag() {
        let mut g = Glide::new(0);
        g.glide(1.0, 30 * S, true, linear);
        g.set(5 * S);
        assert_eq!((g.at(1.0, linear), g.moving(1.0)), (5 * S, false));
        g.glide(2.0, 40 * S, false, linear);
        assert_eq!((g.at(2.0, linear), g.moving(2.0)), (40 * S, false));
    }

    #[test]
    fn playback_moves_at_wall_speed_and_hides_clock_steps() {
        let mut g = Glide::new(0);
        assert_eq!(g.play(0.0, 2 * S), 2 * S);
        let mut last = 2 * S;
        let mut t = 0.0;
        for frame in 1..=144 {
            t = frame as f64 / 144.0;
            let clock = 2 * S + (t * 100.0).floor() as i64 * 10 * MS - if frame % 7 == 0 { 8 * MS } else { 0 };
            let drawn = g.play(t, clock);
            assert!(drawn >= last, "never back: {drawn} after {last} at frame {frame}");
            assert!(drawn - last < 12 * MS, "no visible step at frame {frame}: {}", drawn - last);
            assert!((drawn - clock).abs() < 20 * MS, "stays with the clock at frame {frame}");
            last = drawn;
        }
        assert!((last - (2 * S + (t * S as f64) as i64)).abs() < 20 * MS);
        assert_eq!(g.play(t + 0.007, 9 * S), 9 * S, "a far-off clock is jumped to");
        assert_eq!(g.at(t + 1.0, linear), 9 * S);
    }
}
