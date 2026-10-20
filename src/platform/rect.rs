use windows::Win32::Foundation::RECT;

/// Axis-aligned rectangle in physical pixels (exclusive right/bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub(crate) fn from_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Rect { x: x0.min(x1), y: y0.min(y1), w: (x1 - x0).abs(), h: (y1 - y0).abs() }
    }
    pub(crate) fn right(&self) -> i32 {
        self.x + self.w
    }
    pub(crate) fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub(crate) fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub(crate) fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }
    pub(crate) fn inflate(&self, d: i32) -> Rect {
        Rect { x: self.x - d, y: self.y - d, w: self.w + 2 * d, h: self.h + 2 * d }
    }
    pub(crate) fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
    pub(crate) fn to_win(self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
    pub(crate) fn from_win(r: &RECT) -> Rect {
        Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    }
}

/// The four strips between `outer` and `inner`.
pub(crate) fn ring(outer: Rect, inner: Rect) -> [RECT; 4] {
    [
        RECT { left: outer.x, top: outer.y, right: outer.right(), bottom: inner.y },
        RECT { left: outer.x, top: inner.bottom(), right: outer.right(), bottom: outer.bottom() },
        RECT { left: outer.x, top: inner.y, right: inner.x, bottom: inner.bottom() },
        RECT { left: inner.right(), top: inner.y, right: outer.right(), bottom: inner.bottom() },
    ]
}
