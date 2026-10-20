use crate::platform::monitors::Output;
use crate::platform::rect::Rect;
use crate::platform::windows::MIN_PICK;

/// How a selection is sized: recordings need the encoder's minimum and even sides,
/// screenshots take any size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fit {
    pub min: (i32, i32),
    pub even: bool,
}

impl Fit {
    pub(super) const ANY: Fit = Fit { min: (1, 1), even: false };
}

/// Maps a drag from `anchor` to `cursor` (desktop coordinates) onto the virtual screen `area`,
/// clipped to it; the drag may cross monitors. Selections below `fit.min` grow *away from the
/// anchor* (pushed back only at the edge of `area`), and with `fit.even` odd sizes lose their
/// last pixel on the cursor side, so the corner where the drag started never moves.
pub(super) fn fit_drag(anchor: (i32, i32), cursor: (i32, i32), area: Rect, fit: Fit) -> Rect {
    let axis = |a: i32, c: i32, lo: i32, hi: i32, min: i32| -> (i32, i32) {
        let (a, c) = (a.clamp(lo, hi), c.clamp(lo, hi));
        let len = (c - a).abs().max(min.min(hi - lo));
        let len = if fit.even { len & !1 } else { len };
        if c >= a {
            let start = a.min(hi - len);
            (start, len)
        } else {
            let end = a.max(lo + len);
            (end - len, len)
        }
    };
    let (x, w) = axis(anchor.0, cursor.0, area.x, area.right(), fit.min.0);
    let (y, h) = axis(anchor.1, cursor.1, area.y, area.bottom(), fit.min.1);
    Rect { x, y, w, h }
}

/// Maps a fixed rectangle (a window, a monitor, the virtual screen) into `area`: clipped to
/// it, then sized like [`fit_drag`] from its top-left corner.
pub(super) fn fit_rect(rect: Rect, area: Rect, fit: Fit) -> Rect {
    fit_drag((rect.x, rect.y), (rect.right(), rect.bottom()), area, fit)
}

/// What picking window `win` takes: the whole window across every output it shows on.
/// Strips thinner than [`MIN_PICK`] on an output (a maximized window's frame overhanging onto
/// its neighbours) don't count; None if nothing else is left.
pub(super) fn window_pick(win: Rect, outputs: &[Output]) -> Option<Rect> {
    outputs
        .iter()
        .filter_map(|o| win.intersect(&o.rect))
        .filter(|r| r.w >= MIN_PICK && r.h >= MIN_PICK)
        .reduce(hull)
}

pub(super) fn hull(a: Rect, b: Rect) -> Rect {
    Rect::from_points(a.x.min(b.x), a.y.min(b.y), a.right().max(b.right()), a.bottom().max(b.bottom()))
}

/// Ctrl+click: adds `r` to the chosen windows, or takes it out again.
pub(super) fn toggle(chosen: &mut Vec<Rect>, r: Rect) {
    match chosen.iter().position(|&c| c == r) {
        Some(i) => {
            chosen.remove(i);
        }
        None => chosen.push(r),
    }
}

/// What the chosen windows pick: their bounding box, fitted into `area`, and with `cutout` the windows
/// themselves inside it (None when one of them already covers the whole box).
pub(super) fn multi_pick(chosen: &[Rect], area: Rect, fit: Fit, cutout: bool) -> Option<(Rect, Option<Vec<Rect>>)> {
    let rect = fit_rect(chosen.iter().copied().reduce(hull)?, area, fit);
    let cut: Option<Vec<Rect>> = cutout.then(|| chosen.iter().filter_map(|c| c.intersect(&rect)).collect());
    Some((rect, cut.filter(|c| !c.contains(&rect))))
}

/// The outputs `rect` overlaps, in enumeration order.
pub(super) fn overlapped(rect: Rect, outputs: &[Output]) -> impl Iterator<Item = &Output> {
    outputs.iter().filter(move |o| o.rect.intersect(&rect).is_some())
}

pub(super) fn output_at(outputs: &[Output], x: i32, y: i32) -> Option<usize> {
    outputs.iter().position(|o| o.rect.contains(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Unequal sides, so a mixed-up axis shows.
    const MIN: Fit = Fit { min: (130, 34), even: true };
    const SINGLE: Rect = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    /// The bounding box of [`layout`].
    const AREA: Rect = Rect { x: -1920, y: -1440, w: 5360, h: 3060 };

    fn output(x: i32, y: i32, w: i32, h: i32) -> Output {
        Output { adapter_idx: 0, output_idx: 0, rect: Rect { x, y, w, h }, refresh_hz: 144.0 }
    }

    /// A wide main monitor, one to its lower left and one above it, with gaps between them.
    fn layout() -> [Output; 3] {
        [output(0, 0, 3440, 1440), output(-1920, 540, 1920, 1080), output(365, -1440, 2560, 1440)]
    }

    fn on(rect: Rect, outs: &[Output]) -> Vec<Rect> {
        overlapped(rect, outs).map(|o| o.rect).collect()
    }

    #[test]
    fn tiny_drags_grow_away_from_the_anchor() {
        // A few pixels above the start point: the bottom edge stays at the anchor.
        let r = fit_drag((500, 400), (530, 390), SINGLE, MIN);
        assert_eq!((r.x, r.bottom(), r.w, r.h), (500, 400, MIN.min.0, MIN.min.1));
        // Moving further up (still under the minimum) must not move the box at all.
        assert_eq!(fit_drag((500, 400), (530, 380), SINGLE, MIN), r);
        // Down-right: the top-left corner stays at the anchor.
        let r = fit_drag((500, 400), (510, 410), SINGLE, MIN);
        assert_eq!((r.x, r.y, r.w, r.h), (500, 400, MIN.min.0, MIN.min.1));
    }

    #[test]
    fn screenshots_keep_exact_sizes() {
        let r = fit_drag((500, 400), (503, 401), SINGLE, Fit::ANY);
        assert_eq!((r.w, r.h), (3, 1));
        let win = Rect { x: 101, y: 50, w: 701, h: 333 };
        assert_eq!(fit_rect(win, SINGLE, Fit::ANY), win);
    }

    #[test]
    fn odd_sizes_trim_the_cursor_side() {
        let r = fit_drag((1000, 800), (499, 299), SINGLE, MIN); // up-left, 501 x 501
        assert_eq!((r.right(), r.bottom(), r.w, r.h), (1000, 800, 500, 500));
    }

    #[test]
    fn growth_crosses_monitor_edges_and_is_pushed_back_only_at_the_virtual_screen_edge() {
        let outs = layout();
        // Bottom right of the main monitor: the right edge is the virtual screen's, the bottom isn't.
        let r = fit_drag((3430, 1435), (3436, 1438), AREA, MIN);
        assert_eq!((r.right(), r.y, r.w, r.h), (3440, 1435, MIN.min.0, MIN.min.1));
        assert_eq!(on(r, &outs), [outs[0].rect]);
        // Bottom left of the left monitor: both are virtual screen edges.
        let r = fit_drag((-1915, 1615), (-1910, 1618), AREA, MIN);
        assert_eq!((r.x, r.bottom(), r.w, r.h), (-1915, 1620, MIN.min.0, MIN.min.1));
        assert_eq!(on(r, &outs), [outs[1].rect]);
    }

    #[test]
    fn drags_span_monitors() {
        let outs = layout();
        // From the main monitor up onto the one above it, across the gap at its left.
        let r = fit_drag((500, 1000), (2000, -1000), AREA, MIN);
        assert_eq!(r, Rect { x: 500, y: -1000, w: 1500, h: 2000 });
        assert_eq!(on(r, &outs), [outs[0].rect, outs[2].rect]);
        // Past the virtual screen: clipped to it.
        let r = fit_drag((500, 1000), (9999, -9999), AREA, MIN);
        assert_eq!(r, Rect { x: 500, y: -1440, w: 2940, h: 2440 });
        // Onto the left monitor, odd-sized: trimmed on the cursor side.
        let r = fit_drag((300, 600), (-501, 901), AREA, MIN);
        assert_eq!(r, Rect { x: -500, y: 600, w: 800, h: 300 });
        assert_eq!(on(r, &outs), [outs[0].rect, outs[1].rect]);
        // Into the gap at the top left: the selection still lies on the anchor's monitor.
        let r = fit_drag((100, 100), (-1000, -1000), AREA, Fit::ANY);
        assert_eq!(r, Rect { x: -1000, y: -1000, w: 1100, h: 1100 });
        assert_eq!(on(r, &outs), [outs[0].rect]);
    }

    #[test]
    fn windows_are_picked_whole_across_monitors() {
        let outs = layout();
        // Straddling the main and the left monitor, odd-sized: all of it, even.
        let win = Rect { x: -301, y: 600, w: 1001, h: 501 };
        let r = fit_rect(window_pick(win, &outs).unwrap(), AREA, MIN);
        assert_eq!(r, Rect { x: -301, y: 600, w: 1000, h: 500 });
        assert_eq!(on(r, &outs), [outs[0].rect, outs[1].rect]);
        // Straddling the main and the upper monitor next to the gap: whole, gap included.
        let win = Rect { x: 300, y: -500, w: 700, h: 1000 };
        assert_eq!(window_pick(win, &outs), Some(win));
        // Hanging off the virtual screen: clipped to it.
        let win = Rect { x: 3000, y: 100, w: 1000, h: 500 };
        assert_eq!(window_pick(win, &outs), Some(Rect { x: 3000, y: 100, w: 440, h: 500 }));
        // Only a sliver shows: not a pick.
        assert_eq!(window_pick(Rect { x: 3430, y: 100, w: 500, h: 500 }, &outs), None);
    }

    #[test]
    fn a_maximized_windows_frame_overhang_stays_off_the_neighbours() {
        let outs = layout();
        let max = Rect { x: -8, y: -8, w: 3456, h: 1456 };
        assert_eq!(fit_rect(window_pick(max, &outs).unwrap(), AREA, MIN), outs[0].rect);
        let max = Rect { x: 357, y: -1448, w: 2576, h: 1456 };
        assert_eq!(window_pick(max, &outs), Some(outs[2].rect));
    }

    #[test]
    fn ctrl_click_toggles_windows_in_and_out() {
        let (a, b) = (Rect { x: 10, y: 10, w: 100, h: 50 }, Rect { x: 300, y: 40, w: 80, h: 80 });
        let mut chosen = Vec::new();
        toggle(&mut chosen, a);
        toggle(&mut chosen, b);
        assert_eq!(chosen, [a, b]);
        toggle(&mut chosen, a);
        assert_eq!(chosen, [b]);
        toggle(&mut chosen, a);
        assert_eq!(chosen, [b, a]);
    }

    #[test]
    fn chosen_windows_pick_their_bounding_box() {
        let outs = layout();
        // One on the main monitor, one on the left monitor, odd sizes.
        let a = Rect { x: 101, y: 700, w: 301, h: 201 };
        let b = Rect { x: -900, y: 600, w: 400, h: 151 };
        let bounds = Rect { x: -900, y: 600, w: 1302, h: 301 };
        let (rect, cut) = multi_pick(&[a, b], AREA, Fit::ANY, true).unwrap();
        assert_eq!((rect, cut), (bounds, Some(vec![a, b])));
        assert_eq!(on(rect, &outs), [outs[0].rect, outs[1].rect]);
        // Recordings get the even-sized box and no cutout.
        let (rect, cut) = multi_pick(&[a, b], AREA, MIN, false).unwrap();
        assert_eq!((rect, cut), (Rect { w: 1302, h: 300, ..bounds }, None));
        // A window covering the others needs no cutout; nothing chosen picks nothing.
        let big = Rect { x: -1000, y: 500, w: 1500, h: 500 };
        assert_eq!(multi_pick(&[a, big], AREA, Fit::ANY, true), Some((big, None)));
        assert_eq!(multi_pick(&[], AREA, Fit::ANY, true), None);
    }

    #[test]
    fn cutouts_stay_inside_the_virtual_screen() {
        // Hanging off the bottom right of the main monitor; the cutout is clipped with the box.
        let a = Rect { x: 3300, y: 1300, w: 400, h: 400 };
        let b = Rect { x: 3000, y: 1000, w: 100, h: 100 };
        let (rect, cut) = multi_pick(&[a, b], AREA, Fit::ANY, true).unwrap();
        assert_eq!(rect, Rect { x: 3000, y: 1000, w: 440, h: 620 });
        assert_eq!(cut, Some(vec![Rect { x: 3300, y: 1300, w: 140, h: 320 }, b]));
    }
}
