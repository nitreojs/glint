use super::rect::Rect;

/// A desktop-attached DXGI output (monitor).
#[derive(Clone, Debug)]
pub struct Output {
    pub adapter_idx: u32,
    pub output_idx: u32,
    /// Desktop coordinates in physical pixels.
    pub rect: Rect,
    pub refresh_hz: f64,
}

impl Output {
    /// Native frame rate rounded to an integer (143.98 Hz -> 144).
    pub fn native_fps(&self) -> u32 {
        (self.refresh_hz.round() as u32).max(1)
    }
}
