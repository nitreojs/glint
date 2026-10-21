use super::rect::Rect;

/// Sides of a selection whose outline is drawn just inside it instead of around it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sides {
    pub l: bool,
    pub t: bool,
    pub r: bool,
    pub b: bool,
}

/// The sides of `r` on the edge of its monitors: a `width` px outline outside them would land on
/// another monitor or on none, so it goes inside `r` there.
pub fn inward(r: Rect, monitors: &[Rect], width: i32) -> Sides {
    let (tw, th) = (width.min(r.w), width.min(r.h));
    let strips = [
        (Rect { w: tw, ..r }, (-width, 0)),
        (Rect { h: th, ..r }, (0, -width)),
        (Rect { x: r.right() - tw, w: tw, ..r }, (width, 0)),
        (Rect { y: r.bottom() - th, h: th, ..r }, (0, width)),
    ];
    let [l, t, rr, b] = strips.map(|(strip, (dx, dy))| {
        let mut covered = 0i64;
        let stays = monitors.iter().filter_map(|m| strip.intersect(m).map(|part| (m, part))).all(|(m, part)| {
            covered += part.w as i64 * part.h as i64;
            let out = part.offset(dx, dy);
            m.intersect(&out) == Some(out)
        });
        !stays || covered < strip.w as i64 * strip.h as i64
    });
    Sides { l, t, r: rr, b }
}

/// Where an outline `width` px thick starts: `r` grown by `width` on every side not drawn inward.
pub fn outer(r: Rect, sides: Sides, width: i32) -> Rect {
    let grow = |inside: bool| if inside { 0 } else { width };
    Rect::from_points(r.x - grow(sides.l), r.y - grow(sides.t), r.right() + grow(sides.r), r.bottom() + grow(sides.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: [Rect; 3] = [
        Rect { x: 0, y: 0, w: 3440, h: 1440 },
        Rect { x: -1920, y: 540, w: 1920, h: 1080 },
        Rect { x: 365, y: -1440, w: 2560, h: 1440 },
    ];

    fn sides(l: bool, t: bool, r: bool, b: bool) -> Sides {
        Sides { l, t, r, b }
    }

    fn at(r: Rect) -> Sides {
        inward(r, &MONITORS, 2)
    }

    #[test]
    fn outlines_stay_on_the_selections_monitors() {
        assert_eq!(at(Rect { x: 1000, y: 300, w: 500, h: 400 }), Sides::default(), "mid-monitor: all around");
        for m in MONITORS {
            assert_eq!(at(m), sides(true, true, true, true), "a whole monitor keeps it inside");
        }
        assert_eq!(at(Rect { x: 1000, y: 0, w: 500, h: 300 }), sides(false, true, false, false), "the monitor above");
        assert_eq!(at(Rect { x: 100, y: 0, w: 500, h: 300 }), sides(false, true, false, false), "half over a gap");
        assert_eq!(at(Rect { x: 0, y: 600, w: 300, h: 200 }), sides(true, false, false, false), "the monitor beside");
        assert_eq!(at(Rect { x: 0, y: 100, w: 300, h: 200 }), sides(true, false, false, false), "nothing beside");
        assert_eq!(at(Rect { x: 3000, y: 1200, w: 440, h: 240 }), sides(false, false, true, true), "the far corner");
        assert_eq!(at(Rect { x: 1, y: 1, w: 300, h: 200 }), sides(true, true, false, false), "1 px is too close");
        assert_eq!(at(Rect { x: 2, y: 2, w: 300, h: 200 }), Sides::default(), "2 px is enough");
    }

    #[test]
    fn selections_across_monitors() {
        assert_eq!(at(Rect { x: 1000, y: -200, w: 500, h: 400 }), Sides::default(), "down across the boundary");
        assert_eq!(at(Rect { x: 300, y: -200, w: 500, h: 400 }), sides(true, true, false, false), "parts over a gap");
        assert_eq!(at(Rect { x: -100, y: 600, w: 300, h: 200 }), Sides::default(), "sideways across the boundary");
        assert_eq!(at(Rect { x: -100, y: 500, w: 300, h: 200 }), sides(true, true, false, false), "parts over a gap");
        let both = Rect { x: -1920, y: -1440, w: 5360, h: 3060 };
        assert_eq!(at(both), sides(true, true, true, true));
    }

    #[test]
    fn outer_grows_only_outward_sides() {
        let r = Rect { x: 10, y: 20, w: 30, h: 40 };
        assert_eq!(outer(r, Sides::default(), 2), r.inflate(2));
        assert_eq!(outer(r, sides(true, false, false, true), 2), Rect { x: 10, y: 18, w: 32, h: 42 });
        assert_eq!(outer(r, sides(true, true, true, true), 2), r);
    }
}
