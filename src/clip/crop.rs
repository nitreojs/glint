#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

pub(super) const MIN: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Aspect {
    Free,
    Original,
    Wide,
    Square,
    Tall,
}

impl Aspect {
    pub(super) const ALL: [(Aspect, &'static str); 5] = [
        (Aspect::Free, "free"),
        (Aspect::Original, "original"),
        (Aspect::Wide, "16:9"),
        (Aspect::Square, "1:1"),
        (Aspect::Tall, "9:16"),
    ];

    pub(super) fn ratio(self, src: (u32, u32)) -> Option<f32> {
        match self {
            Aspect::Free => None,
            Aspect::Original => Some(src.0.max(1) as f32 / src.1.max(1) as f32),
            Aspect::Wide => Some(16.0 / 9.0),
            Aspect::Square => Some(1.0),
            Aspect::Tall => Some(9.0 / 16.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Grip {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Grip {
    pub(super) const MOVE: Grip = Grip { left: false, right: false, top: false, bottom: false };

    pub(super) fn all() -> [Grip; 8] {
        let g = |left, right, top, bottom| Grip { left, right, top, bottom };
        [
            g(true, false, true, false),
            g(false, false, true, false),
            g(false, true, true, false),
            g(false, true, false, false),
            g(false, true, false, true),
            g(false, false, false, true),
            g(true, false, false, true),
            g(true, false, false, false),
        ]
    }

    fn corner(self) -> bool {
        (self.left || self.right) && (self.top || self.bottom)
    }
}

#[derive(Clone, Copy)]
struct Edges {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Crop {
    pub(super) fn full(src: (u32, u32)) -> Crop {
        Crop { x: 0, y: 0, w: src.0, h: src.1 }
    }

    pub(super) fn is_full(self, src: (u32, u32)) -> bool {
        self.x == 0 && self.y == 0 && self.w >= src.0 && self.h >= src.1
    }

    fn edges(self) -> Edges {
        Edges { x0: self.x as f32, y0: self.y as f32, x1: (self.x + self.w) as f32, y1: (self.y + self.h) as f32 }
    }

    fn from_edges(e: Edges, src: (u32, u32)) -> Crop {
        let (sw, sh) = (src.0 as f32, src.1 as f32);
        let x = e.x0.round().clamp(0.0, sw) as u32;
        let y = e.y0.round().clamp(0.0, sh) as u32;
        let w = ((e.x1 - e.x0).round().max(1.0) as u32).min(src.0 - x.min(src.0)).max(1);
        let h = ((e.y1 - e.y0).round().max(1.0) as u32).min(src.1 - y.min(src.1)).max(1);
        Crop { x, y, w, h }
    }

    pub(super) fn center(self) -> (f32, f32) {
        (self.x as f32 + self.w as f32 / 2.0, self.y as f32 + self.h as f32 / 2.0)
    }

    pub(super) fn moved(self, d: (f32, f32), src: (u32, u32)) -> Crop {
        let x = (self.x as f32 + d.0).round().clamp(0.0, (src.0 - self.w.min(src.0)) as f32) as u32;
        let y = (self.y as f32 + d.1).round().clamp(0.0, (src.1 - self.h.min(src.1)) as f32) as u32;
        Crop { x, y, ..self }
    }

    /// `self` dragged by `grip` over `d` source pixels; with a ratio the box keeps it, corners pivot on the
    /// opposite corner and edges stretch around the box's centre line.
    pub(super) fn dragged(self, grip: Grip, d: (f32, f32), ratio: Option<f32>, src: (u32, u32)) -> Crop {
        if grip == Grip::MOVE {
            return self.moved(d, src);
        }
        let (sw, sh) = (src.0 as f32, src.1 as f32);
        let min = (MIN as f32).min(sw).min(sh);
        let mut e = self.edges();
        let Some(r) = ratio else {
            if grip.left {
                e.x0 = (e.x0 + d.0).clamp(0.0, e.x1 - min);
            }
            if grip.right {
                e.x1 = (e.x1 + d.0).clamp(e.x0 + min, sw);
            }
            if grip.top {
                e.y0 = (e.y0 + d.1).clamp(0.0, e.y1 - min);
            }
            if grip.bottom {
                e.y1 = (e.y1 + d.1).clamp(e.y0 + min, sh);
            }
            return Crop::from_edges(e, src);
        };
        let min_w = min.max(min * r);
        if grip.corner() {
            let ax = if grip.left { e.x1 } else { e.x0 };
            let ay = if grip.top { e.y1 } else { e.y0 };
            let px = if grip.left { e.x0 + d.0 } else { e.x1 + d.0 };
            let py = if grip.top { e.y0 + d.1 } else { e.y1 + d.1 };
            let room_w = if grip.left { ax } else { sw - ax };
            let room_h = if grip.top { ay } else { sh - ay };
            let w = (px - ax).abs().max((py - ay).abs() * r).max(min_w).min(room_w).min(room_h * r);
            let h = w / r;
            (e.x0, e.x1) = if grip.left { (ax - w, ax) } else { (ax, ax + w) };
            (e.y0, e.y1) = if grip.top { (ay - h, ay) } else { (ay, ay + h) };
            return Crop::from_edges(e, src);
        }
        let (cx, cy) = self.center();
        if grip.left || grip.right {
            let ax = if grip.left { e.x1 } else { e.x0 };
            let px = if grip.left { e.x0 + d.0 } else { e.x1 + d.0 };
            let room = if grip.left { ax } else { sw - ax };
            let w = (px - ax).abs().max(min_w).min(room).min(sh * r);
            let h = w / r;
            (e.x0, e.x1) = if grip.left { (ax - w, ax) } else { (ax, ax + w) };
            let y0 = (cy - h / 2.0).clamp(0.0, sh - h);
            (e.y0, e.y1) = (y0, y0 + h);
        } else {
            let ay = if grip.top { e.y1 } else { e.y0 };
            let py = if grip.top { e.y0 + d.1 } else { e.y1 + d.1 };
            let room = if grip.top { ay } else { sh - ay };
            let h = (py - ay).abs().max(min_w / r).min(room).min(sw / r);
            let w = h * r;
            (e.y0, e.y1) = if grip.top { (ay - h, ay) } else { (ay, ay + h) };
            let x0 = (cx - w / 2.0).clamp(0.0, sw - w);
            (e.x0, e.x1) = (x0, x0 + w);
        }
        Crop::from_edges(e, src)
    }

    /// The box reshaped to `ratio` with about the same area, centred where it was, shrunk to fit the frame.
    pub(super) fn with_aspect(self, ratio: f32, src: (u32, u32)) -> Crop {
        let (sw, sh) = (src.0 as f32, src.1 as f32);
        let area = self.w as f32 * self.h as f32;
        let mut w = (area * ratio).sqrt();
        let mut h = w / ratio;
        let s = (sw / w).min(sh / h).min(1.0);
        (w, h) = (w * s, h * s);
        let (cx, cy) = self.center();
        let x0 = (cx - w / 2.0).clamp(0.0, sw - w);
        let y0 = (cy - h / 2.0).clamp(0.0, sh - h);
        Crop::from_edges(Edges { x0, y0, x1: x0 + w, y1: y0 + h }, src)
    }

    /// Even offsets and sizes inside the frame, as video encoders and nv12 need.
    pub(super) fn even(self, src: (u32, u32)) -> Crop {
        let x = self.x.min(src.0.saturating_sub(2)) & !1;
        let y = self.y.min(src.1.saturating_sub(2)) & !1;
        let w = (self.w.min(src.0 - x) & !1).max(2);
        let h = (self.h.min(src.1 - y) & !1).max(2);
        Crop { x, y, w, h }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: (u32, u32) = (1920, 1080);

    fn c(x: u32, y: u32, w: u32, h: u32) -> Crop {
        Crop { x, y, w, h }
    }

    #[test]
    fn aspect_presets_keep_the_centre_and_fit_the_frame() {
        let sq = Crop::full(SRC).with_aspect(1.0, SRC);
        assert_eq!(sq, c(420, 0, 1080, 1080));
        let box_ = c(560, 340, 800, 450);
        let tall = box_.with_aspect(9.0 / 16.0, SRC);
        assert_eq!(tall.center(), box_.center());
        assert!((tall.w as f32 / tall.h as f32 - 9.0 / 16.0).abs() < 0.01);
        assert!(tall.y + tall.h <= SRC.1);
        let corner = c(1800, 1000, 120, 80).with_aspect(16.0 / 9.0, SRC);
        assert!(corner.x + corner.w == 1920 && corner.y + corner.h <= 1080);
        assert!((corner.w as f32 / corner.h as f32 - 16.0 / 9.0).abs() < 0.05);
        assert_eq!(Aspect::Original.ratio((3440, 1440)), Some(3440.0 / 1440.0));
        assert_eq!(Aspect::Free.ratio(SRC), None);
    }

    #[test]
    fn sizes_and_offsets_come_out_even_and_inside() {
        assert_eq!(c(101, 51, 641, 361).even(SRC), c(100, 50, 640, 360));
        assert_eq!(c(1919, 1079, 5, 5).even(SRC), c(1918, 1078, 2, 2));
        assert_eq!(c(0, 0, 1920, 1080).even(SRC), Crop::full(SRC));
        assert_eq!(c(3, 3, 1919, 1079).even((1919, 1079)), c(2, 2, 1916, 1076));
    }

    #[test]
    fn moving_and_resizing_stay_in_the_frame() {
        let b = c(100, 100, 400, 300);
        assert_eq!(b.moved((-500.0, 2000.0), SRC), c(0, 780, 400, 300));
        let left = Grip { left: true, right: false, top: false, bottom: false };
        assert_eq!(b.dragged(left, (-300.0, 0.0), None, SRC), c(0, 100, 500, 300));
        assert_eq!(b.dragged(left, (1000.0, 0.0), None, SRC), c(468, 100, MIN, 300));
        let br = Grip { left: false, right: true, top: false, bottom: true };
        assert_eq!(b.dragged(br, (5000.0, 5000.0), None, SRC), c(100, 100, 1820, 980));
        let locked = b.dragged(br, (400.0, 0.0), Some(16.0 / 9.0), SRC);
        assert_eq!((locked.x, locked.y, locked.w, locked.h), (100, 100, 800, 450));
        let huge = b.dragged(br, (5000.0, 5000.0), Some(1.0), SRC);
        assert_eq!(huge, c(100, 100, 980, 980));
        let top = Grip { left: false, right: false, top: true, bottom: false };
        let t = c(800, 400, 320, 180).dragged(top, (0.0, -180.0), Some(16.0 / 9.0), SRC);
        assert_eq!((t.y + t.h, t.h, t.w), (580, 360, 640));
        assert_eq!(t.center().0, 960.0);
        assert!(Crop::full(SRC).is_full(SRC) && !b.is_full(SRC));
    }
}
