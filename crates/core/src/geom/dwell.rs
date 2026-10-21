use std::time::{Duration, Instant};

use super::rect::Rect;

/// How long the cursor rests on a big target before the hover highlight goes there.
pub const DWELL: Duration = Duration::from_millis(100);

/// What a hover highlight shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Element,
    Window,
    /// A monitor, or every monitor.
    Monitor,
}

pub type Target = (Rect, Kind);

fn area(r: Rect) -> i64 {
    r.w as i64 * r.h as i64
}

/// Whether the highlight waits before moving from `from` to `to`: onto a monitor from anything
/// else, or onto something over 2.5 times bigger unless both are windows.
pub fn waits(from: Target, to: Target) -> bool {
    match (from.1, to.1) {
        (Kind::Monitor, Kind::Monitor) | (Kind::Window, Kind::Window) => false,
        (_, Kind::Monitor) => true,
        _ => area(to.0) * 2 > area(from.0) * 5,
    }
}

/// Holds the hover highlight back from big targets until the cursor has rested on one.
#[derive(Debug, Default)]
pub struct Dwell {
    shown: Option<Target>,
    since: Option<(Rect, Instant)>,
}

impl Dwell {
    /// The highlight shows `to`, no questions asked.
    pub fn set(&mut self, to: Option<Target>) {
        self.shown = to;
        self.since = None;
    }

    /// The cursor is over `to` at `now`: None to show it now (it becomes the shown target), or
    /// how much longer to wait before asking again.
    pub fn hover(&mut self, to: Option<Target>, now: Instant) -> Option<Duration> {
        let (Some(from), Some(next)) = (self.shown, to) else {
            self.set(to);
            return None;
        };
        if from.0 == next.0 || !waits(from, next) {
            self.set(to);
            return None;
        }
        match self.since {
            Some((r, at)) if r == next.0 => {
                let rested = now.saturating_duration_since(at);
                if rested >= DWELL {
                    self.set(to);
                    None
                } else {
                    Some(DWELL - rested)
                }
            }
            _ => {
                self.since = Some((next.0, now));
                Some(DWELL)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUTTON: Rect = Rect { x: 100, y: 100, w: 80, h: 30 };
    const PANEL: Rect = Rect { x: 50, y: 50, w: 600, h: 400 };
    const WINDOW: Rect = Rect { x: 0, y: 0, w: 800, h: 600 };
    const OTHER: Rect = Rect { x: 900, y: 0, w: 1600, h: 1200 };
    const MONITOR: Rect = Rect { x: 0, y: 0, w: 3440, h: 1440 };

    #[test]
    fn big_targets_wait() {
        assert!(waits((WINDOW, Kind::Window), (MONITOR, Kind::Monitor)));
        assert!(waits((BUTTON, Kind::Element), (MONITOR, Kind::Monitor)));
        assert!(!waits((MONITOR, Kind::Monitor), (OTHER, Kind::Monitor)), "monitor to monitor");
        assert!(!waits((WINDOW, Kind::Window), (OTHER, Kind::Window)), "window to a bigger window");
        assert!(waits((BUTTON, Kind::Element), (PANEL, Kind::Element)), "a much bigger element");
        assert!(waits((PANEL, Kind::Element), (OTHER, Kind::Window)), "an element's much bigger window");
        assert!(!waits((PANEL, Kind::Element), (WINDOW, Kind::Window)), "2x isn't much bigger");
        assert!(!waits((MONITOR, Kind::Monitor), (BUTTON, Kind::Element)), "smaller never waits");
    }

    #[test]
    fn the_highlight_moves_once_the_cursor_rests() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut d = Dwell::default();
        assert_eq!(d.hover(Some((BUTTON, Kind::Element)), t0), None, "the first target shows at once");
        assert_eq!(d.hover(Some((MONITOR, Kind::Monitor)), ms(10)), Some(DWELL));
        assert_eq!(d.hover(Some((MONITOR, Kind::Monitor)), ms(70)), Some(Duration::from_millis(40)));
        assert_eq!(d.hover(Some((BUTTON, Kind::Element)), ms(80)), None, "back on the shown target");
        assert_eq!(d.hover(Some((MONITOR, Kind::Monitor)), ms(90)), Some(DWELL), "the wait starts over");
        assert_eq!(d.hover(Some((PANEL, Kind::Element)), ms(150)), Some(DWELL), "another big target, another wait");
        assert_eq!(d.hover(Some((PANEL, Kind::Element)), ms(250)), None, "rested long enough");
        assert_eq!(d.hover(Some((MONITOR, Kind::Monitor)), ms(260)), Some(DWELL));
        assert_eq!(d.hover(Some((BUTTON, Kind::Element)), ms(270)), None, "a smaller one switches at once");
        d.set(Some((MONITOR, Kind::Monitor)));
        assert_eq!(d.hover(Some((WINDOW, Kind::Window)), ms(280)), None);
        assert_eq!(d.hover(Some((OTHER, Kind::Window)), ms(281)), None, "window to window glides at once");
        assert_eq!(d.hover(None, ms(282)), None);
    }
}
