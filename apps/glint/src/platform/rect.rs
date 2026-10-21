use glint_core::geom::rect::Rect;
use windows::Win32::Foundation::RECT;

/// The four strips between `outer` and `inner`.
pub(crate) fn ring(outer: Rect, inner: Rect) -> [RECT; 4] {
    [
        RECT { left: outer.x, top: outer.y, right: outer.right(), bottom: inner.y },
        RECT { left: outer.x, top: inner.bottom(), right: outer.right(), bottom: outer.bottom() },
        RECT { left: outer.x, top: inner.y, right: inner.x, bottom: inner.bottom() },
        RECT { left: inner.right(), top: inner.y, right: outer.right(), bottom: inner.bottom() },
    ]
}
