use glint_core::geom::rect::Rect;

pub(super) struct Node {
    pub(super) rect: Rect,
    /// None until fetched.
    pub(super) kids: Option<Vec<usize>>,
}

/// A window's element tree as far as it has been fetched; node 0 is the window itself and
/// every node is clipped to its parent (an element without bounds takes its parent's).
pub(super) struct Tree {
    pub(super) nodes: Vec<Node>,
    /// Nodes before this one are all fetched.
    scan: usize,
}

impl Tree {
    pub(super) fn new(root: Rect) -> Tree {
        Tree { nodes: vec![Node { rect: root, kids: None }], scan: 0 }
    }

    /// Nodes from the root down to the innermost fetched one containing `pt`. Overlapping
    /// siblings are all searched: the smallest innermost node wins, the deeper and then the
    /// later (drawn on top) one on ties.
    pub(super) fn walk(&self, pt: (i32, i32)) -> Vec<usize> {
        let (mut path, mut best) = (Vec::new(), Vec::new());
        if self.nodes[0].rect.contains(pt.0, pt.1) {
            self.search(0, pt, &mut path, &mut best);
        }
        best
    }

    fn search(&self, at: usize, pt: (i32, i32), path: &mut Vec<usize>, best: &mut Vec<usize>) {
        path.push(at);
        let better = best.last().is_none_or(|&b| {
            let (a, b) = (area(&self.nodes[at].rect), area(&self.nodes[b].rect));
            a < b || a == b && path.len() >= best.len()
        });
        if better {
            best.clone_from(path);
        }
        for &k in self.nodes[at].kids.iter().flatten() {
            if self.nodes[k].rect.contains(pt.0, pt.1) {
                self.search(k, pt, path, best);
            }
        }
        path.pop();
    }

    /// A node containing `pt` whose children are still unknown.
    pub(super) fn frontier(&self, pt: (i32, i32)) -> Option<usize> {
        let mut stack = vec![0];
        while let Some(at) = stack.pop() {
            if !self.nodes[at].rect.contains(pt.0, pt.1) {
                continue;
            }
            match &self.nodes[at].kids {
                None => return Some(at),
                Some(kids) => stack.extend(kids),
            }
        }
        None
    }

    /// Records the children of `at` clipped to it, dropping those outside it; returns the
    /// positions in `rects` that became nodes, in order.
    pub(super) fn add_kids(&mut self, at: usize, rects: &[Rect]) -> Vec<usize> {
        let clip = self.nodes[at].rect;
        let (mut ids, mut kept) = (Vec::new(), Vec::new());
        for (i, r) in rects.iter().enumerate() {
            let r = if r.w <= 0 || r.h <= 0 { Some(clip) } else { r.intersect(&clip) };
            if let Some(r) = r {
                ids.push(self.nodes.len());
                self.nodes.push(Node { rect: r, kids: None });
                kept.push(i);
            }
        }
        self.nodes[at].kids = Some(ids);
        kept
    }

    /// The oldest node whose children are still unknown (breadth-first for background fetching).
    pub(super) fn unexplored(&mut self) -> Option<usize> {
        while self.scan < self.nodes.len() && self.nodes[self.scan].kids.is_some() {
            self.scan += 1;
        }
        (self.scan < self.nodes.len()).then_some(self.scan)
    }
}

/// The rects around `pt` inside `window`, innermost first and ending with `window`: clipped to
/// it, each enclosing the previous one, near-duplicates (every edge within 1 px) merged into
/// the outer one. Empty if `window` doesn't contain `pt`.
pub(super) fn chain(pt: (i32, i32), window: Rect, rects: impl IntoIterator<Item = Rect>) -> Vec<Rect> {
    if !window.contains(pt.0, pt.1) {
        return Vec::new();
    }
    let mut all: Vec<Rect> =
        rects.into_iter().filter_map(|r| r.intersect(&window)).filter(|r| r.contains(pt.0, pt.1)).collect();
    all.sort_by_key(area);
    let mut out: Vec<Rect> = Vec::new();
    for r in all.into_iter().chain([window]) {
        match out.last_mut() {
            Some(last) if same(last, &r) => *last = r,
            Some(last) if !encloses(&r, last) => {}
            _ => out.push(r),
        }
    }
    out
}

fn area(r: &Rect) -> i64 {
    r.w as i64 * r.h as i64
}

fn encloses(outer: &Rect, inner: &Rect) -> bool {
    outer.x <= inner.x && outer.y <= inner.y && outer.right() >= inner.right() && outer.bottom() >= inner.bottom()
}

fn same(a: &Rect, b: &Rect) -> bool {
    [(a.x, b.x), (a.y, b.y), (a.right(), b.right()), (a.bottom(), b.bottom())].iter().all(|(p, q)| (p - q).abs() <= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn chain_is_nested_innermost_first_and_ends_with_window() {
        let win = r(0, 0, 1000, 800);
        let got = chain((50, 50), win, [r(0, 5, 1000, 795), r(40, 40, 20, 20), r(10, 10, 200, 200)]);
        assert_eq!(got, [r(40, 40, 20, 20), r(10, 10, 200, 200), r(0, 5, 1000, 795), win]);
    }

    #[test]
    fn chain_clips_to_window_and_drops_misses() {
        let win = r(100, 100, 500, 400);
        let got = chain((120, 120), win, [r(0, 0, 150, 150), r(300, 300, 50, 50), r(0, 0, 0, 0)]);
        assert_eq!(got, [r(100, 100, 50, 50), win]);
        assert!(chain((10, 10), win, [r(0, 0, 50, 50)]).is_empty());
    }

    #[test]
    fn chain_merges_near_duplicates_into_the_outer() {
        let win = r(0, 0, 800, 600);
        let got = chain((50, 50), win, [r(10, 10, 100, 100), r(9, 10, 101, 100), r(10, 10, 100, 100), r(1, 0, 799, 600)]);
        assert_eq!(got, [r(9, 10, 101, 100), win]);
    }

    #[test]
    fn chain_drops_overlapping_rects_that_dont_nest() {
        let win = r(0, 0, 800, 600);
        let got = chain((50, 50), win, [r(40, 40, 20, 20), r(45, 0, 400, 400), r(0, 0, 300, 300)]);
        assert_eq!(got, [r(40, 40, 20, 20), r(0, 0, 300, 300), win]);
    }

    #[test]
    fn walk_searches_overlapping_siblings() {
        let mut t = Tree::new(r(0, 0, 400, 300));
        t.add_kids(0, &[r(0, 0, 400, 300), r(0, 0, 400, 300)]);
        assert_eq!(t.frontier((15, 15)), Some(2));
        t.add_kids(2, &[]);
        assert_eq!(t.frontier((15, 15)), Some(1));
        t.add_kids(1, &[r(10, 10, 20, 20), r(10, 10, 20, 20), r(200, 0, 10, 10)]);
        for n in 3..6 {
            t.add_kids(n, &[]);
        }
        assert_eq!(t.frontier((15, 15)), None);
        assert_eq!(t.walk((15, 15)), [0, 1, 4]);
        assert_eq!(t.walk((100, 100)), [0, 2]);
    }

    #[test]
    fn elements_without_bounds_take_their_parents() {
        let mut t = Tree::new(r(0, 0, 400, 300));
        t.add_kids(0, &[r(0, 0, 0, 0), r(50, 50, 10, 0)]);
        assert_eq!(t.nodes[1].rect, r(0, 0, 400, 300));
        assert_eq!(t.nodes[2].rect, r(0, 0, 400, 300));
        t.add_kids(1, &[r(20, 20, 30, 30)]);
        assert_eq!(t.walk((25, 25)), [0, 1, 3]);
    }

    #[test]
    fn tree_walks_clips_and_tracks_frontier() {
        let mut t = Tree::new(r(0, 0, 400, 300));
        assert_eq!(t.frontier((10, 10)), Some(0));
        let kept = t.add_kids(0, &[r(-50, -50, 100, 100), r(500, 0, 10, 10), r(200, 0, 300, 300)]);
        assert_eq!(kept, [0, 2]);
        assert_eq!(t.nodes[1].rect, r(0, 0, 50, 50));
        assert_eq!(t.nodes[2].rect, r(200, 0, 200, 300));
        assert_eq!(t.walk((10, 10)), [0, 1]);
        assert_eq!(t.frontier((10, 10)), Some(1));
        assert_eq!(t.walk((100, 100)), [0]);
        assert_eq!(t.frontier((100, 100)), None);
        assert_eq!(t.unexplored(), Some(1));
        t.add_kids(1, &[]);
        assert_eq!(t.frontier((10, 10)), None);
        assert_eq!(t.unexplored(), Some(2));
        assert!(t.walk((1000, 10)).is_empty());
    }
}
