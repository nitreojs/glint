use glint_core::geom::rect::Rect;
use windows::Win32::Foundation::RECT;

pub trait WinRect {
    fn to_win(self) -> RECT;
    fn from_win(r: &RECT) -> Self;
}

impl WinRect for Rect {
    fn to_win(self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
    fn from_win(r: &RECT) -> Rect {
        Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    }
}
