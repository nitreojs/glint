//! Drawing helpers shared by the overlay and the recording indicator: 32-bit DIB surfaces,
//! Direct2D/DirectWrite rendering (anti-aliased pills and text) into DIBs or straight into
//! DWM glass windows, and vsync-paced fades.

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Dwm::DwmFlush;

mod canvas;
mod dib;
mod painter;

pub use canvas::Canvas;
pub use dib::Dib;
pub use painter::{Painter, make_glass};

pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

pub fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Runs `step(progress)` once per display refresh for `ms` milliseconds (progress 0..=1,
/// eased), blocking the calling thread.
pub fn animate(ms: f32, mut step: impl FnMut(f32)) {
    let start = std::time::Instant::now();
    loop {
        let t = (start.elapsed().as_secs_f32() * 1000.0 / ms).min(1.0);
        step(ease_out(t));
        if t >= 1.0 {
            break;
        }
        if unsafe { DwmFlush() }.is_err() {
            std::thread::sleep(std::time::Duration::from_millis(7));
        }
    }
}
