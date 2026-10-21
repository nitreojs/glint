use super::rect::Rect;

/// Candidate edges, sorted and deduplicated; right and bottom edges are exclusive like [`Rect::right`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    pub xs: Vec<i32>,
    pub ys: Vec<i32>,
}

pub fn edges(rects: &[Rect]) -> Edges {
    let mut xs: Vec<i32> = rects.iter().flat_map(|r| [r.x, r.right()]).collect();
    let mut ys: Vec<i32> = rects.iter().flat_map(|r| [r.y, r.bottom()]).collect();
    for v in [&mut xs, &mut ys] {
        v.sort_unstable();
        v.dedup();
    }
    Edges { xs, ys }
}

/// The candidate nearest to `value` if it's within `threshold`, else `value`. `candidates` must be sorted.
pub fn snap_edge(value: i32, candidates: &[i32], threshold: i32) -> i32 {
    let i = candidates.partition_point(|&c| c < value);
    let below = i.checked_sub(1).and_then(|i| candidates.get(i));
    [below, candidates.get(i)]
        .into_iter()
        .flatten()
        .copied()
        .filter(|c| (c - value).abs() <= threshold)
        .min_by_key(|c| (c - value).abs())
        .unwrap_or(value)
}

pub fn threshold(scale: f32) -> i32 {
    (8.0 * scale).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_collect_both_sides_once() {
        let a = Rect { x: 10, y: 20, w: 100, h: 50 };
        let b = Rect { x: 110, y: 20, w: 30, h: 10 };
        assert_eq!(edges(&[a, b]), Edges { xs: vec![10, 110, 140], ys: vec![20, 30, 70] });
    }

    #[test]
    fn values_snap_to_the_nearest_edge_within_the_threshold() {
        let xs = [0, 100, 110, 400];
        assert_eq!(snap_edge(96, &xs, 8), 100);
        assert_eq!(snap_edge(106, &xs, 8), 110);
        assert_eq!(snap_edge(104, &xs, 8), 100);
        assert_eq!(snap_edge(392, &xs, 8), 400);
        // Beyond the threshold it lets go.
        assert_eq!(snap_edge(391, &xs, 8), 391);
        assert_eq!(snap_edge(-20, &xs, 8), -20);
        assert_eq!(snap_edge(5, &[], 8), 5);
    }

    #[test]
    fn the_threshold_follows_the_dpi() {
        assert_eq!((threshold(1.0), threshold(1.5), threshold(2.0)), (8, 12, 16));
    }
}
