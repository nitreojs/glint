//! The gallery's grid: a heading per day, then rows of equal tiles, in content coordinates (x from
//! the content's left, y from its top), so only the rows in view need drawing.

use std::ops::Range;

use emath::{Pos2, Rect, Vec2, pos2, vec2};

use super::Day;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    /// The content's width.
    pub width: f32,
    /// Tiles are at least this wide; as many as fit share the width.
    pub min_tile: f32,
    /// A tile's height over its width.
    pub aspect: f32,
    pub gap: f32,
    /// A day heading's height, the gap to its first row included.
    pub heading: f32,
    /// Space above a heading that follows a day's tiles.
    pub day_gap: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Line {
    /// The heading of `days[i]`.
    Heading(usize),
    /// Items `first..end`, left to right.
    Tiles { first: usize, end: usize },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub top: f32,
    pub height: f32,
    pub line: Line,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    pub cols: usize,
    pub tile: Vec2,
    pub gap: f32,
    pub rows: Vec<Row>,
    pub height: f32,
    /// The row each item sits in.
    item_row: Vec<u32>,
}

impl Spec {
    pub fn cols(&self) -> usize {
        (((self.width + self.gap) / (self.min_tile + self.gap)).floor() as usize).max(1)
    }
}

impl Grid {
    pub fn new(spec: Spec, days: &[Day]) -> Grid {
        let cols = spec.cols();
        let w = ((spec.width - spec.gap * (cols - 1) as f32) / cols as f32).max(1.0);
        let tile = vec2(w, (w * spec.aspect).round());
        let mut g = Grid { cols, tile, gap: spec.gap, ..Grid::default() };
        let mut y = 0.0;
        for (d, day) in days.iter().enumerate() {
            if d > 0 {
                y += spec.day_gap - spec.gap;
            }
            g.rows.push(Row { top: y, height: spec.heading, line: Line::Heading(d) });
            y += spec.heading;
            let mut first = day.items.start;
            while first < day.items.end {
                let end = (first + cols).min(day.items.end);
                g.item_row.resize(end, g.rows.len() as u32);
                g.rows.push(Row { top: y, height: tile.y, line: Line::Tiles { first, end } });
                y += tile.y + spec.gap;
                first = end;
            }
        }
        g.height = if days.is_empty() { 0.0 } else { y - spec.gap };
        g
    }

    pub fn len(&self) -> usize {
        self.item_row.len()
    }

    pub fn is_empty(&self) -> bool {
        self.item_row.is_empty()
    }

    /// The rows that reach into `top..bottom`.
    pub fn visible(&self, top: f32, bottom: f32) -> Range<usize> {
        let start = self.rows.partition_point(|r| r.top + r.height <= top);
        let end = self.rows.partition_point(|r| r.top < bottom);
        start..end.max(start)
    }

    /// The items in `rows`.
    pub fn items_in(&self, rows: Range<usize>) -> Range<usize> {
        let tiles = |r: &Row| match r.line {
            Line::Tiles { first, end } => Some(first..end),
            Line::Heading(_) => None,
        };
        let first = self.rows[rows.clone()].iter().find_map(tiles);
        let last = self.rows[rows].iter().rev().find_map(tiles);
        match (first, last) {
            (Some(a), Some(b)) => a.start..b.end,
            _ => 0..0,
        }
    }

    /// Item `i`'s tile.
    pub fn tile(&self, i: usize) -> Rect {
        let row = self.rows[self.item_row[i] as usize];
        let Line::Tiles { first, .. } = row.line else { unreachable!("items sit in tile rows") };
        let x = (i - first) as f32 * (self.tile.x + self.gap);
        Rect::from_min_size(pos2(x, row.top), self.tile)
    }

    /// The item whose tile is under `at`.
    pub fn at(&self, at: Pos2) -> Option<usize> {
        let r = self.rows.partition_point(|r| r.top + r.height <= at.y);
        let row = self.rows.get(r)?;
        let Line::Tiles { first, end } = row.line else { return None };
        if at.y < row.top || at.x < 0.0 {
            return None;
        }
        let pitch = self.tile.x + self.gap;
        let col = (at.x / pitch).floor();
        let inside = at.x - col * pitch < self.tile.x;
        let i = first + col as usize;
        (inside && i < end).then_some(i)
    }

    /// Where the keyboard focus goes from `i`: along the list, or to the nearest tile in the row
    /// above or below (skipping headings).
    pub fn step(&self, i: usize, step: Step) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        let i = i.min(n - 1);
        let row = self.item_row[i] as usize;
        let Line::Tiles { first, .. } = self.rows[row].line else { return i };
        let col = i - first;
        let pick = |r: &Row| match r.line {
            Line::Tiles { first, end } => Some((first + col).min(end - 1)),
            Line::Heading(_) => None,
        };
        match step {
            Step::Left => i.saturating_sub(1),
            Step::Right => (i + 1).min(n - 1),
            Step::Up => self.rows[..row].iter().rev().find_map(pick).unwrap_or(i),
            Step::Down => self.rows[row + 1..].iter().find_map(pick).unwrap_or(i),
        }
    }
}

/// Where a tile's preview grows to: `aspect` (width over height) at `scale` times the tile's width
/// (or `min_w`, if wider), no bigger than `limit` and inside `bounds`, centred on the tile as far as
/// `bounds` let it.
pub fn grown(tile: Rect, aspect: f32, bounds: Rect, scale: f32, min_w: f32, limit: Vec2) -> Rect {
    let aspect = aspect.max(0.05);
    let w = (tile.width() * scale).max(min_w);
    let size = vec2(w, w / aspect);
    let room = vec2(bounds.width().min(limit.x), bounds.height().min(limit.y)).max(vec2(1.0, 1.0));
    let size = size * (room.x / size.x).min(room.y / size.y).min(1.0);
    let r = Rect::from_center_size(tile.center(), size);
    let dx = (bounds.left() - r.left()).max(0.0) + (bounds.right() - r.right()).min(0.0);
    let dy = (bounds.top() - r.top()).max(0.0) + (bounds.bottom() - r.bottom()).min(0.0);
    r.translate(vec2(dx, dy))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: Spec = Spec { width: 1000.0, min_tile: 200.0, aspect: 0.625, gap: 10.0, heading: 40.0, day_gap: 30.0 };

    fn days(sizes: &[usize]) -> Vec<Day> {
        let mut start = 0;
        sizes
            .iter()
            .map(|&n| {
                let d = Day { date: None, items: start..start + n };
                start += n;
                d
            })
            .collect()
    }

    #[test]
    fn as_many_columns_as_fit_share_the_width() {
        assert_eq!(SPEC.cols(), 4);
        assert_eq!(Spec { width: 829.0, ..SPEC }.cols(), 3);
        assert_eq!(Spec { width: 830.0, ..SPEC }.cols(), 4);
        assert_eq!(Spec { width: 50.0, ..SPEC }.cols(), 1);
        let g = Grid::new(SPEC, &days(&[1]));
        assert_eq!(g.tile, vec2(242.5, 152.0));
    }

    #[test]
    fn days_start_new_rows_under_their_heading() {
        let g = Grid::new(SPEC, &days(&[5, 2]));
        let lines: Vec<Line> = g.rows.iter().map(|r| r.line).collect();
        assert_eq!(
            lines,
            [
                Line::Heading(0),
                Line::Tiles { first: 0, end: 4 },
                Line::Tiles { first: 4, end: 5 },
                Line::Heading(1),
                Line::Tiles { first: 5, end: 7 },
            ]
        );
        let tops: Vec<f32> = g.rows.iter().map(|r| r.top).collect();
        // 40 heading, 152 + 10 a row, the last row's gap grows to the day gap
        assert_eq!(tops, [0.0, 40.0, 202.0, 384.0, 424.0]);
        assert_eq!(g.height, 576.0);
        assert_eq!(g.len(), 7);
        assert_eq!(g.tile(0), Rect::from_min_size(pos2(0.0, 40.0), vec2(242.5, 152.0)));
        assert_eq!(g.tile(3).min, pos2(757.5, 40.0));
        assert_eq!(g.tile(4).min, pos2(0.0, 202.0));
        assert_eq!(g.tile(6).min, pos2(252.5, 424.0));
        assert_eq!(Grid::new(SPEC, &[]).height, 0.0);
    }

    #[test]
    fn only_rows_in_view_are_visible() {
        let g = Grid::new(SPEC, &days(&[1000]));
        // heading 0..40, then 250 rows at 40 + 162 k
        assert_eq!(g.rows.len(), 251);
        assert_eq!(g.visible(0.0, 600.0), 0..5);
        assert_eq!(g.items_in(0..5), 0..16);
        // a row that ends exactly at the top is out, one that starts exactly at the bottom too
        assert_eq!(g.visible(202.0, 364.0), 2..3);
        assert_eq!(g.visible(191.0, 365.0), 1..4);
        assert_eq!(g.visible(195.0, 200.0), 2..2); // only the gap between two rows
        let far = g.visible(100_000.0, 100_800.0);
        assert_eq!(far, 251..251);
        let mid = g.visible(20_000.0, 21_000.0);
        let items = g.items_in(mid.clone());
        assert!(items.len() <= 4 * 8 && items.start > 400, "{mid:?} {items:?}");
        for i in items.clone() {
            let t = g.tile(i);
            assert!(t.max.y > 20_000.0 && t.min.y < 21_000.0);
        }
        assert!(g.tile(items.start - 4).max.y <= 20_000.0);
        assert_eq!(g.visible(-50.0, -1.0), 0..0);
        assert_eq!(g.items_in(0..1), 0..0);
    }

    #[test]
    fn points_hit_tiles_not_gaps_or_headings() {
        let g = Grid::new(SPEC, &days(&[5, 2]));
        assert_eq!(g.at(pos2(5.0, 45.0)), Some(0));
        assert_eq!(g.at(pos2(245.0, 45.0)), None); // in the gap
        assert_eq!(g.at(pos2(260.0, 100.0)), Some(1));
        assert_eq!(g.at(pos2(999.0, 191.0)), Some(3));
        assert_eq!(g.at(pos2(5.0, 195.0)), None); // between rows
        assert_eq!(g.at(pos2(300.0, 210.0)), None); // past the day's last tile
        assert_eq!(g.at(pos2(5.0, 10.0)), None); // the heading
        assert_eq!(g.at(pos2(260.0, 430.0)), Some(6));
        assert_eq!(g.at(pos2(5.0, 5000.0)), None);
        assert_eq!(g.at(pos2(-1.0, 45.0)), None);
    }

    #[test]
    fn arrows_walk_the_list_and_jump_rows_by_column() {
        let g = Grid::new(SPEC, &days(&[5, 2]));
        assert_eq!(g.step(0, Step::Left), 0);
        assert_eq!(g.step(3, Step::Right), 4);
        assert_eq!(g.step(6, Step::Right), 6);
        assert_eq!(g.step(2, Step::Down), 4); // the short row's last tile
        assert_eq!(g.step(4, Step::Down), 5); // over the heading
        assert_eq!(g.step(6, Step::Up), 4);
        assert_eq!(g.step(4, Step::Up), 0);
        assert_eq!(g.step(1, Step::Up), 1);
        assert_eq!(g.step(6, Step::Down), 6);
        assert_eq!(g.step(99, Step::Left), 5);
        assert_eq!(Grid::new(SPEC, &[]).step(0, Step::Down), 0);
    }

    #[test]
    fn previews_grow_around_their_tile_and_stay_inside() {
        let bounds = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 800.0));
        let big = vec2(f32::INFINITY, f32::INFINITY);
        let tile = |x: f32, y: f32| Rect::from_min_size(pos2(x, y), vec2(200.0, 125.0));
        let r = |x0, y0, x1, y1| Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
        assert_eq!(grown(tile(400.0, 300.0), 2.0, bounds, 2.5, 0.0, big), r(250.0, 237.5, 750.0, 487.5));
        // in a corner it moves in rather than shrinking
        assert_eq!(grown(tile(0.0, 0.0), 2.0, bounds, 2.5, 0.0, big), r(0.0, 0.0, 500.0, 250.0));
        assert_eq!(grown(tile(800.0, 675.0), 2.0, bounds, 2.5, 0.0, big), r(500.0, 550.0, 1000.0, 800.0));
        // a tall picture fits the height
        assert_eq!(grown(tile(400.0, 300.0), 0.25, bounds, 2.5, 0.0, big), r(400.0, 0.0, 600.0, 800.0));
        // a small picture grows no further than it's allowed to
        let small = grown(tile(400.0, 300.0), 2.0, bounds, 2.5, 0.0, vec2(300.0, 150.0));
        assert_eq!(small.size(), vec2(300.0, 150.0));
        assert_eq!(small.center(), pos2(500.0, 362.5));
        assert_eq!(grown(tile(400.0, 300.0), 2.0, bounds, 1.0, 560.0, big).width(), 560.0);
    }
}
