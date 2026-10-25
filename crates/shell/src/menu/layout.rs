//! Where rows sit inside a panel and where panels sit on screen. Pure: lengths come in measured,
//! in physical pixels.

use glint_core::geom::rect::Rect;

use super::{Item, Kind};

/// The menu's lengths at one DPI, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Metrics {
    pub s: f32,
    /// Room round the panel for its shadow; the window is the panel grown by this.
    pub shadow: f32,
    pub radius: f32,
    /// Above the first row and below the last.
    pub pad: f32,
    pub row: f32,
    pub separator: f32,
    pub header: f32,
    /// The highlight's gap to the panel's sides.
    pub inset: f32,
    pub glyph_x: f32,
    pub glyph: f32,
    /// Where labels start when rows have glyphs.
    pub text_x: f32,
    /// Where labels start when none has.
    pub bare_x: f32,
    pub right: f32,
    /// At least this between a label and its hint.
    pub gap: f32,
    pub chevron: f32,
    pub switch: (f32, f32),
    pub min_w: f32,
    /// How far a submenu overlaps its parent.
    pub overlap: f32,
}

impl Metrics {
    pub fn new(s: f32) -> Metrics {
        let px = |v: f32| (v * s).round();
        Metrics {
            s,
            shadow: px(16.0),
            radius: px(9.0),
            pad: px(5.0),
            row: px(30.0),
            separator: px(9.0),
            header: px(38.0),
            inset: px(5.0),
            glyph_x: px(15.0),
            glyph: px(16.0),
            text_x: px(43.0),
            bare_x: px(17.0),
            right: px(15.0),
            gap: px(32.0),
            chevron: px(10.0),
            switch: (px(26.0), px(14.0)),
            min_w: px(196.0),
            overlap: px(3.0),
        }
    }
}

/// A row's top and height inside its panel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Row {
    pub y: f32,
    pub h: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub w: f32,
    pub h: f32,
    /// The header's height, 0 without one.
    pub top: f32,
    pub text_x: f32,
    pub rows: Vec<Row>,
}

impl Layout {
    /// `widths[i]`: the label's and the hint's widths of `items[i]`; `header`: the header's text
    /// width, if there is one.
    pub fn new(m: &Metrics, items: &[Item], widths: &[(f32, f32)], header: Option<f32>) -> Layout {
        let glyphs = items.iter().any(|i| i.glyph.is_some());
        let text_x = if glyphs { m.text_x } else { m.bare_x };
        let top = if header.is_some() { m.header } else { 0.0 };
        let mut y = top + m.pad;
        let mut w = header.map_or(0.0, |h| text_x + h + m.right);
        let mut rows = Vec::with_capacity(items.len());
        for (item, &(label, hint)) in items.iter().zip(widths) {
            let h = if matches!(item.kind, Kind::Separator) { m.separator } else { m.row };
            rows.push(Row { y, h });
            y += h;
            let trail = match (&item.kind, item.check) {
                // a submenu's hint sits left of its chevron
                (Kind::Sub(_), _) if hint > 0.0 => hint + 2.0 * m.chevron,
                (Kind::Sub(_), _) => m.chevron,
                (_, Some(_)) => m.switch.0,
                _ => hint,
            };
            let trail = if trail > 0.0 { m.gap + trail } else { 0.0 };
            w = w.max(text_x + label + trail + m.right);
        }
        Layout { w: w.max(m.min_w).ceil(), h: (y + m.pad).ceil(), top, text_x, rows }
    }

    /// The row at `y` (panel coordinates), if any.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || x >= self.w {
            return None;
        }
        self.rows.iter().position(|r| (r.y..r.y + r.h).contains(&y))
    }
}

/// The side of the panel it opened from, so it slides in away from there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// Opened downwards from the point.
    Above,
    Below,
    Left,
    Right,
}

/// Puts the top-left corner of a `w`×`h` panel at `at`, flipped left / up where it would leave
/// `work`, then kept inside it (a cursor on a bottom taskbar puts the menu right above it).
pub(crate) fn place_at((x, y): (i32, i32), (w, h): (i32, i32), work: Rect) -> (Rect, Side) {
    let left = if x + w > work.right() { x - w } else { x };
    let (top, from) = if y + h > work.bottom() { (y - h, Side::Below) } else { (y, Side::Above) };
    (clamp(Rect { x: left, y: top, w, h }, work), from)
}

/// Puts a submenu beside `parent` (both panels, screen coordinates), its first row level with
/// the row it opened from (`row_top`, `pad` above its rows): on the side the cascade runs
/// (`leftwards` once a submenu had to open left), the other side where there's no room, shifted
/// up to stay on `work`.
pub(crate) fn place_beside(
    parent: Rect,
    row_top: i32,
    (w, h): (i32, i32),
    work: Rect,
    (overlap, pad): (i32, i32),
    leftwards: bool,
) -> (Rect, Side) {
    let (right, left) = (parent.right() - overlap, parent.x + overlap - w);
    let fits_right = right + w <= work.right();
    let fits_left = left >= work.x;
    let go_left = if leftwards { fits_left || !fits_right } else { !fits_right && fits_left };
    let (x, from) = if go_left { (left, Side::Right) } else { (right, Side::Left) };
    (clamp(Rect { x, y: row_top - pad, w, h }, work), from)
}

fn clamp(r: Rect, work: Rect) -> Rect {
    let x = r.x.min(work.right() - r.w).max(work.x);
    let y = r.y.min(work.bottom() - r.h).max(work.y);
    Rect { x, y, ..r }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::Glyph;

    const WORK: Rect = Rect { x: 0, y: 0, w: 1920, h: 1032 };

    #[test]
    fn rows_stack_under_the_header_and_separators_are_thin() {
        let m = Metrics::new(1.0);
        let items = [Item::command(1, "a").glyph(Glyph::Folder), Item::separator(), Item::command(2, "b")];
        let l = Layout::new(&m, &items, &[(40.0, 0.0), (0.0, 0.0), (40.0, 0.0)], Some(60.0));
        assert_eq!(l.top, 38.0);
        assert_eq!(l.rows, [Row { y: 43.0, h: 30.0 }, Row { y: 73.0, h: 9.0 }, Row { y: 82.0, h: 30.0 }]);
        assert_eq!(l.h, 117.0);
        assert_eq!((l.w, l.text_x), (196.0, 43.0), "narrow items take the minimum width");
        assert_eq!(l.hit(10.0, 50.0), Some(0));
        assert_eq!(l.hit(10.0, 75.0), Some(1));
        assert_eq!(l.hit(10.0, 20.0), None, "the header isn't a row");
        assert_eq!(l.hit(-1.0, 50.0), None);
    }

    #[test]
    fn width_fits_the_widest_label_and_hint_and_doubles_at_200_percent() {
        let items = [Item::command(1, "record").keys(Some("Alt+F9")), Item::submenu("recent", vec![Item::separator()])];
        let l = Layout::new(&Metrics::new(1.0), &items, &[(200.0, 50.0), (60.0, 0.0)], None);
        assert_eq!(l.text_x, 17.0, "no glyphs: labels start near the edge");
        assert_eq!(l.w, 17.0 + 200.0 + 32.0 + 50.0 + 15.0);
        let l2 = Layout::new(&Metrics::new(2.0), &items, &[(400.0, 100.0), (120.0, 0.0)], None);
        assert_eq!(l2.w, l.w * 2.0);
        assert_eq!(l2.h, l.h * 2.0);
    }

    #[test]
    fn a_submenu_row_makes_room_for_its_hint_and_chevron() {
        let items = [Item::submenu("02:07", vec![Item::separator()]).hint(Some("4.2 MB"))];
        let l = Layout::new(&Metrics::new(1.0), &items, &[(200.0, 40.0)], None);
        assert_eq!(l.w, 17.0 + 200.0 + 32.0 + 40.0 + 2.0 * 10.0 + 15.0);
    }

    #[test]
    fn hotkeys_show_lowercase_and_details_as_given() {
        assert_eq!(Item::command(1, "record").keys(Some("Alt+F9")).hint, "alt+f9");
        assert_eq!(Item::command(1, "02:07").hint(Some("3.7 MB")).hint, "3.7 MB");
        assert_eq!(Item::command(1, "quit").keys(None).hint, "");
    }

    #[test]
    fn opens_down_and_right_of_the_point_when_it_fits() {
        let (r, from) = place_at((100, 100), (220, 300), WORK);
        assert_eq!((r, from), (Rect { x: 100, y: 100, w: 220, h: 300 }, Side::Above));
    }

    #[test]
    fn a_bottom_right_tray_flips_it_up_and_left_above_the_taskbar() {
        // the cursor is on the taskbar, below the work area
        let (r, from) = place_at((1800, 1060), (220, 300), WORK);
        assert_eq!(from, Side::Below);
        assert_eq!(r, Rect { x: 1580, y: 732, w: 220, h: 300 }, "its bottom sits on the work area's");
        // a tray near the corner, the menu wider than what's left of it
        let (r, _) = place_at((150, 1060), (220, 300), WORK);
        assert_eq!(r.x, 150);
    }

    #[test]
    fn a_top_or_left_taskbar_keeps_it_inside_the_work_area() {
        let work = Rect { x: 0, y: 48, w: 1920, h: 1032 };
        let (r, from) = place_at((1800, 20), (220, 300), work);
        assert_eq!((r, from), (Rect { x: 1580, y: 48, w: 220, h: 300 }, Side::Above));
        let work = Rect { x: 60, y: 0, w: 1860, h: 1080 };
        let (r, _) = place_at((30, 1000), (220, 300), work);
        assert_eq!(r, Rect { x: 60, y: 700, w: 220, h: 300 });
    }

    #[test]
    fn works_on_a_monitor_left_of_the_main_one() {
        let work = Rect { x: -2560, y: -200, w: 2560, h: 1400 };
        let (r, from) = place_at((-40, 1220), (300, 400), work);
        assert_eq!((r, from), (Rect { x: -340, y: 800, w: 300, h: 400 }, Side::Below));
    }

    #[test]
    fn submenus_open_right_or_flip_left_and_shift_up() {
        let parent = Rect { x: 1000, y: 600, w: 220, h: 300 };
        let (r, from) = place_beside(parent, 700, (200, 150), WORK, (3, 5), false);
        assert_eq!((r, from), (Rect { x: 1217, y: 695, w: 200, h: 150 }, Side::Left));
        let parent = Rect { x: 1600, y: 600, w: 220, h: 300 };
        let (r, from) = place_beside(parent, 900, (200, 150), WORK, (3, 5), false);
        assert_eq!(from, Side::Right);
        assert_eq!(r, Rect { x: 1403, y: 882, w: 200, h: 150 }, "no room right: left, and up off the bottom");
        // too wide for either side: right, pushed back inside
        let (r, _) = place_beside(Rect { x: 10, y: 0, w: 1900, h: 300 }, 0, (200, 100), WORK, (3, 5), false);
        assert_eq!(r.x, 1720);
    }

    #[test]
    fn a_cascade_that_turned_left_keeps_going_left() {
        let parent = Rect { x: 1000, y: 600, w: 220, h: 300 };
        let (r, from) = place_beside(parent, 700, (200, 150), WORK, (3, 5), true);
        assert_eq!((r.x, from), (803, Side::Right), "room on both sides: it keeps the way it runs");
        let (r, from) = place_beside(Rect { x: 100, ..parent }, 700, (200, 150), WORK, (3, 5), true);
        assert_eq!((r.x, from), (317, Side::Left), "no room left: right after all");
    }
}
