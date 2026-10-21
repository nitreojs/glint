//! Windows backend of [`glint_core::record::pipeline`].
//!
//! * Capture: Desktop Duplication of every monitor the region overlaps, on its own D3D11
//!   device. Frames arrive at the monitors' refresh rates; the region is cropped (and, across
//!   monitors, composed) on the GPU into a small ring of shared textures. Nothing else
//!   touches this device, so the encoder can never stall capture.
//! * Encoder: a second device shared with Media Foundation. It copies frames out of the ring,
//!   draws the cursor and feeds the hardware encoder MFT through a sink writer, which writes
//!   the MP4.
//! * Audio: WASAPI loopback, per-app process loopback and microphones, written as AAC streams.

use std::path::PathBuf;

use glint_core::config::audio::AudioSource;
use glint_core::geom::rect::Rect;
use glint_core::record::Codec;

#[cfg(windows)]
mod apps;
#[cfg(windows)]
mod audio;
#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod cursor;
#[cfg(windows)]
mod encoder;
#[cfg(windows)]
mod rotate;
#[cfg(windows)]
mod win;

#[cfg(windows)]
pub use apps::{App, running_apps};
#[cfg(windows)]
pub use audio::{Microphone, microphones};

#[derive(Clone, Debug)]
pub struct Params {
    /// Every output the region overlaps, all on one adapter.
    pub sources: Vec<Source>,
    /// Capture region in desktop pixels; width and height must be even. Parts no source
    /// covers stay black.
    pub region: Rect,
    /// Stream frame rate: the fastest source's exact refresh rate, or a lower cap. Captures
    /// that land in the same 1/fps slot are merged, so a cap simply keeps the latest image.
    pub fps: f64,
    pub codec: Codec,
    /// Constant quantizer: lower = better quality and bigger files.
    pub qp: u32,
    /// Encoder quality-vs-speed knob, 0 (fastest) ..= 100 (best).
    pub effort: u32,
    pub draw_cursor: bool,
    pub audio: Vec<AudioSource>,
    pub track_names: Vec<String>,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub struct Source {
    pub adapter_idx: u32,
    pub output_idx: u32,
    /// The output's desktop rectangle when the region was picked.
    pub desktop: Rect,
}

#[cfg(windows)]
pub fn start(
    p: Params,
    on_done: impl FnOnce(Result<glint_core::record::pipeline::Outcome, String>) + Send + 'static,
) -> Result<glint_core::record::pipeline::Starting, String> {
    glint_core::record::pipeline::start(win::Windows(p), on_done)
}

pub trait Ctx<T> {
    fn ctx(self, what: &str) -> Result<T, String>;
}
impl<T> Ctx<T> for windows::core::Result<T> {
    fn ctx(self, what: &str) -> Result<T, String> {
        self.map_err(|e| format!("{what}: {} (0x{:08X})", e.message(), e.code().0 as u32))
    }
}
