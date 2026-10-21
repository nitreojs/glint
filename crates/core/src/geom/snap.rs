use super::rect::Rect;

/// A stretch of edge a selection can snap to: at `at` on one axis, spanning `from..=to` on the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Segment {
    pub at: i32,
    pub from: i32,
    pub to: i32,
}

/// Candidate edges, sorted by position: `xs` are vertical (at an x), `ys` horizontal. Right and
/// bottom edges are exclusive like [`Rect::right`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub xs: Vec<Segment>,
    pub ys: Vec<Segment>,
}

impl Edges {
    pub fn offset(&self, dx: i32, dy: i32) -> Edges {
        let shift = |v: &[Segment], d: i32, e: i32| -> Vec<Segment> {
            v.iter().map(|s| Segment { at: s.at + d, from: s.from + e, to: s.to + e }).collect()
        };
        Edges { xs: shift(&self.xs, dx, dy), ys: shift(&self.ys, dy, dx) }
    }

    fn sorted(mut self) -> Edges {
        for v in [&mut self.xs, &mut self.ys] {
            v.sort_unstable();
            v.dedup();
        }
        self
    }
}

/// Every edge of `rects`, whole.
pub fn edges(rects: &[Rect]) -> Edges {
    visible(rects, &[], &[])
}

/// What can be seen of the edges of `frames` (windows, topmost first), of `parts` (rects inside
/// the window at that index: its client area, UI elements) and of `monitors`: a window's edges
/// and its parts' count only where no window above covers them; monitor edges always.
pub fn visible(frames: &[Rect], parts: &[(usize, Rect)], monitors: &[Rect]) -> Edges {
    let mut e = Edges::default();
    for &m in monitors {
        add(&mut e, m, &[]);
    }
    for (i, &f) in frames.iter().enumerate() {
        add(&mut e, f, &frames[..i]);
    }
    for &(i, r) in parts {
        add(&mut e, r, &frames[..i.min(frames.len())]);
    }
    e.sorted()
}

/// Adds the four edges of `r`, less what `above` covers.
fn add(e: &mut Edges, r: Rect, above: &[Rect]) {
    if r.w <= 0 || r.h <= 0 {
        return;
    }
    // Each edge is judged by the rect's own outermost pixels.
    for (at, col) in [(r.x, r.x), (r.right(), r.right() - 1)] {
        let covers = above.iter().filter(|o| o.x <= col && col < o.right()).map(|o| (o.y, o.bottom()));
        e.xs.extend(uncovered(r.y, r.bottom(), covers).map(|(from, to)| Segment { at, from, to }));
    }
    for (at, row) in [(r.y, r.y), (r.bottom(), r.bottom() - 1)] {
        let covers = above.iter().filter(|o| o.y <= row && row < o.bottom()).map(|o| (o.x, o.right()));
        e.ys.extend(uncovered(r.x, r.right(), covers).map(|(from, to)| Segment { at, from, to }));
    }
}

/// The parts of `lo..hi` outside every interval of `covers`.
fn uncovered(lo: i32, hi: i32, covers: impl Iterator<Item = (i32, i32)>) -> impl Iterator<Item = (i32, i32)> {
    let mut left = vec![(lo, hi)];
    for (a, b) in covers {
        left = left
            .into_iter()
            .flat_map(|(x, y)| [(x, y.min(a)), (x.max(b), y)])
            .filter(|(x, y)| x < y)
            .collect();
    }
    left.into_iter()
}

/// The nearest candidate to `value` within `threshold` whose span reaches `across` (the moving
/// edge's extent on the other axis; a point for a dragged corner), give or take `threshold` so
/// corners meet corners; else `value`. `candidates` must be sorted.
pub fn snap_edge(value: i32, across: (i32, i32), candidates: &[Segment], threshold: i32) -> i32 {
    let start = candidates.partition_point(|s| s.at < value - threshold);
    candidates[start..]
        .iter()
        .take_while(|s| s.at <= value + threshold)
        .filter(|s| s.from - threshold <= across.1 && across.0 <= s.to + threshold)
        .map(|s| s.at)
        .min_by_key(|c| (c - value).abs())
        .unwrap_or(value)
}

pub fn threshold(scale: f32) -> i32 {
    (8.0 * scale).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(at: i32, from: i32, to: i32) -> Segment {
        Segment { at, from, to }
    }

    #[test]
    fn edges_collect_both_sides_once() {
        let a = Rect { x: 10, y: 20, w: 100, h: 50 };
        let b = Rect { x: 110, y: 20, w: 30, h: 10 };
        let e = edges(&[a, a, b]);
        assert_eq!(e.xs, [seg(10, 20, 70), seg(110, 20, 30), seg(110, 20, 70), seg(140, 20, 30)]);
        assert_eq!(e.ys, [seg(20, 10, 110), seg(20, 110, 140), seg(30, 110, 140), seg(70, 10, 110)]);
    }

    #[test]
    fn values_snap_to_the_nearest_edge_within_the_threshold() {
        let xs: Vec<Segment> = [0, 100, 110, 400].map(|at| seg(at, 0, 1000)).to_vec();
        let at = |v: i32| snap_edge(v, (500, 500), &xs, 8);
        assert_eq!(at(96), 100);
        assert_eq!(at(106), 110);
        assert_eq!(at(104), 100);
        assert_eq!(at(392), 400);
        // Beyond the threshold it lets go.
        assert_eq!(at(391), 391);
        assert_eq!(at(-20), -20);
        assert_eq!(snap_edge(5, (0, 0), &[], 8), 5);
    }

    #[test]
    fn edges_pull_only_alongside_themselves() {
        let xs = [seg(100, 0, 300), seg(104, 500, 900)];
        assert_eq!(snap_edge(97, (200, 200), &xs, 8), 100);
        assert_eq!(snap_edge(97, (308, 308), &xs, 8), 100, "corners meet within the threshold");
        assert_eq!(snap_edge(97, (309, 309), &xs, 8), 97);
        assert_eq!(snap_edge(97, (1300, 1300), &xs, 8), 97, "an edge 1000 px away doesn't pull");
        assert_eq!(snap_edge(97, (600, 600), &xs, 8), 104, "the nearer one is out of reach");
        assert_eq!(snap_edge(97, (250, 600), &xs, 8), 100, "a moving side meets both");
    }

    #[test]
    fn covered_edges_are_left_out() {
        let top = Rect { x: 0, y: 0, w: 1000, h: 800 };
        let hidden = Rect { x: 100, y: 100, w: 300, h: 200 };
        let partly = Rect { x: 900, y: 600, w: 400, h: 400 };
        let client = Rect { x: 908, y: 640, w: 384, h: 352 };
        let e = visible(&[top, hidden, partly], &[(1, hidden.inflate(-10)), (2, client)], &[]);
        let at = |v: &[Segment], a: i32| v.iter().filter(|s| s.at == a).copied().collect::<Vec<_>>();
        assert!(at(&e.xs, 100).is_empty() && at(&e.ys, 110).is_empty(), "a covered window and its parts add nothing");
        assert_eq!(at(&e.xs, 900), [seg(900, 800, 1000)], "only below the top window");
        assert_eq!(at(&e.ys, 600), [seg(600, 1000, 1300)], "only right of it");
        assert_eq!(at(&e.xs, 1300), [seg(1300, 600, 1000)]);
        assert_eq!(at(&e.xs, 908), [seg(908, 800, 992)], "its client area as far as it shows");
        assert_eq!(at(&e.ys, 640), [seg(640, 1000, 1292)]);
        assert_eq!(at(&e.xs, 1000), [seg(1000, 0, 800)], "the top window whole");
        let monitor = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        let e = visible(&[top], &[], &[monitor]);
        assert_eq!(at(&e.xs, 0), [seg(0, 0, 800), seg(0, 0, 1080)], "monitors are never covered");
    }

    #[test]
    fn offsets_move_both_axes() {
        let e = edges(&[Rect { x: 10, y: 20, w: 30, h: 40 }]).offset(-10, -20);
        assert_eq!(e, edges(&[Rect { x: 0, y: 0, w: 30, h: 40 }]));
    }

    #[test]
    fn the_threshold_follows_the_dpi() {
        assert_eq!((threshold(1.0), threshold(1.5), threshold(2.0)), (8, 12, 16));
    }
}
