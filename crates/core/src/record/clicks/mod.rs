//! Mouse click highlights: the clicks of a recording as editable data, and the ripple each one
//! draws as a function of time.
//!
//! Times are in 100 ns units on the video timeline (zero = the first frame, paused spans
//! removed); positions are in region pixels, continuous (the clicked pixel's centre is
//! `x + 0.5`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::mp4;
use crate::geom::rect::Rect;

#[cfg(test)]
mod tests;

/// moov/udta box holding a recording's clicks as JSON ([`Payload`]).
pub const BOX: [u8; 4] = *b"glck";
/// [`Payload::version`] this build writes and the newest it reads.
pub const VERSION: u32 = 1;

const MS: i64 = 10_000;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Click {
    pub button: Button,
    pub down: i64,
    /// `None`: still held when the recording ended.
    pub up: Option<i64>,
    pub x: f32,
    pub y: f32,
}

/// What the `glck` box holds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Payload {
    pub version: u32,
    /// DPI / 96 of the monitor the region was recorded on; [`Style::new`] takes it.
    pub scale: f32,
    pub clicks: Vec<Click>,
}

/// A button going down or up, as a mouse hook reports it: `at` on the capture clock, the
/// position in region pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub button: Button,
    pub down: bool,
    pub at: i64,
    pub x: f32,
    pub y: f32,
}

/// Where a desktop pixel lands in `region`: the pixel's centre in region pixels, `None` outside.
/// The region image is in desktop orientation (rotated monitors are turned upright while
/// capturing), so this holds across monitors and rotations alike.
pub fn to_region((x, y): (i32, i32), region: Rect) -> Option<(f32, f32)> {
    region.contains(x, y).then(|| ((x - region.x) as f32 + 0.5, (y - region.y) as f32 + 0.5))
}

/// Collects clicks from presses and releases in arrival order.
#[derive(Default)]
pub struct Log {
    clicks: Vec<Click>,
    held: [Option<usize>; 3],
}

impl Log {
    pub fn press(&mut self, button: Button, at: i64, x: f32, y: f32) {
        self.release(button, at);
        self.held[button as usize] = Some(self.clicks.len());
        self.clicks.push(Click { button, down: at, up: None, x, y });
    }

    /// A release without a press (held before the hook was listening) is ignored.
    pub fn release(&mut self, button: Button, at: i64) {
        if let Some(i) = self.held[button as usize].take() {
            let c = &mut self.clicks[i];
            c.up = Some(at.max(c.down));
        }
    }

    pub fn clicks(&self) -> &[Click] {
        &self.clicks
    }

    /// The clicks relative to `start` (the first frame's time); ones pressed before it are dropped.
    pub fn finish(self, start: i64) -> Vec<Click> {
        let shift = |c: Click| Click { down: c.down - start, up: c.up.map(|u| u - start), ..c };
        self.clicks.into_iter().filter(|c| c.down >= start).map(shift).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// Ring radius at the end of the ripple, in region pixels.
    pub radius: f32,
    pub stroke: f32,
    /// Diameter of the dot shown while a button is held.
    pub dot: f32,
    /// How long the ring expands and fades.
    pub ripple: i64,
    /// How long the dot takes to fade after the release.
    pub fade: i64,
    /// Straight-alpha RGBA per button.
    pub left: [f32; 4],
    pub right: [f32; 4],
    pub middle: [f32; 4],
}

impl Style {
    /// `scale`: the region's monitor DPI / 96.
    pub fn new(accent: [u8; 3], scale: f32) -> Style {
        let [r, g, b] = accent.map(|c| c as f32 / 255.0);
        Style {
            radius: 28.0 * scale,
            stroke: 3.0 * scale,
            dot: 8.0 * scale,
            ripple: 450 * MS,
            fade: 120 * MS,
            left: [r, g, b, 1.0],
            right: [0.88, 0.88, 0.88, 1.0],
            middle: [r, g, b, 0.55],
        }
    }

    pub fn color(&self, button: Button) -> [f32; 4] {
        match button {
            Button::Left => self.left,
            Button::Right => self.right,
            Button::Middle => self.middle,
        }
    }
}

/// A circle outline centred on (`x`, `y`): every point whose distance from the centre is
/// within `width / 2` of `radius`. A zero radius is a filled disc `width` across.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub width: f32,
    /// Straight-alpha RGBA, the fade already applied to alpha.
    pub color: [f32; 4],
}

fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p).powi(3)
}

/// What `clicks` show at time `t`: each click's ring while it ripples, then its dot while held
/// and fading out.
pub fn state_at(clicks: &[Click], t: i64, style: &Style) -> Vec<Shape> {
    let mut out = Vec::new();
    shapes_at(clicks, t, style, &mut out);
    out
}

/// [`state_at`] into a reused buffer.
pub fn shapes_at(clicks: &[Click], t: i64, style: &Style, out: &mut Vec<Shape>) {
    out.clear();
    for c in clicks.iter().filter(|c| t >= c.down) {
        let [r, g, b, a] = style.color(c.button);
        let since = t - c.down;
        if since < style.ripple {
            let p = since as f32 / style.ripple as f32;
            let color = [r, g, b, a * (1.0 - p)];
            out.push(Shape { x: c.x, y: c.y, radius: style.radius * ease_out(p), width: style.stroke, color });
        }
        let fading = match c.up.map(|u| t - u.max(c.down)) {
            None => 1.0,
            Some(after) if after < 0 => 1.0,
            Some(after) if after < style.fade => 1.0 - after as f32 / style.fade as f32,
            Some(_) => continue,
        };
        out.push(Shape { x: c.x, y: c.y, radius: 0.0, width: style.dot, color: [r, g, b, a * fading] });
    }
}

/// Whether what `clicks` show changes right after `t` (a ring rippling or a dot fading), so a
/// video that draws them needs new frames even when nothing else moves.
pub fn moving(clicks: &[Click], t: i64, style: &Style) -> bool {
    clicks.iter().any(|c| {
        let rippling = (c.down..c.down + style.ripple).contains(&t);
        let fading = c.up.map(|u| u.max(c.down)).is_some_and(|u| (u..u + style.fade).contains(&t));
        rippling || fading
    })
}

/// Stores `payload` in the finalized MP4 at `path` (moov/udta/glck).
pub fn save(path: &Path, payload: &Payload) -> Result<(), String> {
    let json = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    mp4::set_movie_box(path, &BOX, &json)
}

/// The clicks saved in the MP4 at `path`; `None` when it has none (highlights were off or burnt
/// into the video).
pub fn load(path: &Path) -> Result<Option<Payload>, String> {
    let Some(json) = mp4::movie_box(path, &BOX)? else { return Ok(None) };
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let v: Versioned = serde_json::from_slice(&json).map_err(|e| format!("clicks: {e}"))?;
    if v.version > VERSION {
        return Err(format!("clicks were saved by a newer glint (format {})", v.version));
    }
    serde_json::from_slice(&json).map(Some).map_err(|e| format!("clicks: {e}"))
}
