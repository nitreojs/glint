//! Keystroke overlay: the keys pressed during a recording as editable data, and the stack of
//! keycap pills they draw as a function of time (the keyboard analogue of [`super::clicks`]).
//!
//! Times are in 100 ns units on the video timeline (zero = the first frame, paused spans
//! removed). A key's label is resolved on the capture side (modifiers, dead-key composition);
//! this module only stores and lays it out, so it stays free of the Windows keyboard.

use std::path::Path;

use emath::{Rect, pos2, vec2};
use serde::{Deserialize, Serialize};

use super::mp4;

#[cfg(test)]
mod tests;

/// moov/udta box holding a recording's keystrokes as JSON ([`Payload`]).
pub const BOX: [u8; 4] = *b"glky";
/// [`Payload::version`] this build writes and the newest it reads.
pub const VERSION: u32 = 1;

const MS: i64 = 10_000;

/// Modifiers held with a key. Shift is only set for chords and special keys; a bare printable
/// bakes it into the glyph, so [`label`] never shows a redundant `Shift`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    /// A real chord: Ctrl, Alt or Win is held (Shift alone is not, it only shifts a character).
    pub fn chord(&self) -> bool {
        self.ctrl || self.alt || self.win
    }

    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }
}

/// A non-printable key with its own name.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Named {
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    PrintScreen,
}

impl Named {
    /// How it reads on a pill.
    pub fn text(self) -> &'static str {
        match self {
            Named::Enter => "Enter",
            Named::Esc => "Esc",
            Named::Tab => "Tab",
            Named::Backspace => "Backspace",
            Named::Delete => "Delete",
            Named::Space => "Space",
            Named::Left => "\u{2190}",
            Named::Right => "\u{2192}",
            Named::Up => "\u{2191}",
            Named::Down => "\u{2193}",
            Named::Home => "Home",
            Named::End => "End",
            Named::PageUp => "PgUp",
            Named::PageDown => "PgDn",
            Named::Insert => "Ins",
            Named::PrintScreen => "PrtSc",
        }
    }
}

/// The main (non-modifier) part of a keystroke.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KeyLabel {
    /// A named special key, e.g. [`Named::Enter`] or an arrow.
    Named(Named),
    /// A function key `F1`..=`F24`.
    Func(u8),
    /// One printable grapheme, resolved on the capture side (`shortcuts` never records these).
    Text(String),
}

impl KeyLabel {
    fn text(&self) -> String {
        match self {
            KeyLabel::Named(n) => n.text().to_string(),
            KeyLabel::Func(n) => format!("F{n}"),
            KeyLabel::Text(s) => s.clone(),
        }
    }
}

/// One keystroke on the video timeline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// When the key went down, 100 ns on the video timeline.
    pub at: i64,
    /// Release time, for the held pill; `None` if still down when the recording ended.
    pub hold: Option<i64>,
    pub mods: Mods,
    pub main: KeyLabel,
}

/// What `Ctrl+Shift+S`, `Alt+Tab`, `Win+E`, `Esc`, `A` or `\u{2190}` read as.
pub fn label(key: &Key) -> String {
    let mut out = String::new();
    for (on, name) in [(key.mods.ctrl, "Ctrl"), (key.mods.alt, "Alt"), (key.mods.shift, "Shift"), (key.mods.win, "Win")] {
        if on {
            out.push_str(name);
            out.push('+');
        }
    }
    out.push_str(&key.main.text());
    out
}

/// What the `glky` box holds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Payload {
    pub version: u32,
    /// DPI / 96 of the monitor the region was recorded on; [`Style::new`] takes it.
    pub scale: f32,
    pub keys: Vec<Key>,
}

/// A key going down or up as a hook (and its resolver) reports it: `at` on the capture clock,
/// `mods`/`main` resolved for a down event, `vk` the physical key that identifies auto-repeat
/// and pairs a release with its press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub vk: u16,
    pub down: bool,
    pub at: i64,
    pub mods: Mods,
    pub main: KeyLabel,
}

/// Collects keystrokes from downs and ups in arrival order, collapsing auto-repeat.
#[derive(Default)]
pub struct Log {
    keys: Vec<Key>,
    /// Physical keys currently held: their `vk` and the index of their [`Key`].
    held: Vec<(u16, usize)>,
}

impl Log {
    pub fn down(&mut self, vk: u16, at: i64, mods: Mods, main: KeyLabel) {
        if self.held.iter().any(|&(v, _)| v == vk) {
            return; // auto-repeat: the OS resends the key while it is held
        }
        self.held.push((vk, self.keys.len()));
        self.keys.push(Key { at, hold: None, mods, main });
    }

    /// A release without a press (held before the hook was listening) is ignored.
    pub fn up(&mut self, vk: u16, at: i64) {
        if let Some(p) = self.held.iter().position(|&(v, _)| v == vk) {
            let (_, i) = self.held.remove(p);
            self.keys[i].hold = Some(at.max(self.keys[i].at));
        }
    }

    pub fn keys(&self) -> &[Key] {
        &self.keys
    }

    /// The keys relative to `start` (the first frame's time); ones pressed before it are dropped.
    pub fn finish(self, start: i64) -> Vec<Key> {
        self.keys
            .into_iter()
            .filter(|k| k.at >= start)
            .map(|k| Key { at: k.at - start, hold: k.hold.map(|h| h - start), ..k })
            .collect()
    }
}

/// Where a stack of pills sits along the bottom edge.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    Left,
    #[default]
    Centre,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// How long a pill stays fully shown after its (release or press) time.
    pub hold: i64,
    /// How long it then takes to fade out.
    pub fade: i64,
    /// How long it slides and fades in.
    pub rise: i64,
    pub height: f32,
    /// Horizontal padding around the text.
    pub pad: f32,
    /// Vertical gap between stacked pills.
    pub gap: f32,
    pub radius: f32,
    pub font: f32,
    /// Distance the pill slides up as it appears.
    pub slide: f32,
    /// Margin from the region edges, as a fraction of the region height.
    pub margin: f32,
    /// Up to this many pills stack, the newest at the bottom.
    pub max: usize,
    pub pos: Position,
    /// Straight-alpha RGBA: the translucent fill, the text, the accent hairline.
    pub fill: [f32; 4],
    pub text: [f32; 4],
    pub accent: [f32; 4],
}

impl Style {
    /// `scale`: the region's monitor DPI / 96.
    pub fn new(accent: [u8; 3], scale: f32) -> Style {
        let [r, g, b] = accent.map(|c| c as f32 / 255.0);
        Style {
            hold: 1500 * MS,
            fade: 200 * MS,
            rise: 120 * MS,
            height: 30.0 * scale,
            pad: 11.0 * scale,
            gap: 6.0 * scale,
            radius: 8.0 * scale,
            font: 15.0 * scale,
            slide: 10.0 * scale,
            margin: 0.045,
            max: 3,
            pos: Position::Centre,
            fill: [0.09, 0.10, 0.12, 0.82],
            text: [0.96, 0.96, 0.97, 1.0],
            accent: [r, g, b, 1.0],
        }
    }

    pub fn with(mut self, pos: Position) -> Style {
        self.pos = pos;
        self
    }
}

/// A keycap to draw: its text (with `\u{00d7}N` already appended for a burst), its rectangle in
/// region pixels, the alpha the fade and the slide-in leave, and the colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub text: String,
    pub rect: Rect,
    pub alpha: f32,
    pub radius: f32,
    pub font: f32,
    pub fill: [f32; 4],
    pub text_color: [f32; 4],
    pub accent: [f32; 4],
}

fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p).powi(3)
}

struct Run {
    label: String,
    /// When the pill first appeared (the first key of the burst), for the slide-in.
    appear: i64,
    /// The latest (release or press) time of the burst, where the hold countdown starts.
    base: i64,
    count: u32,
}

/// What a burnt-in frame needs to draw its keycap stack: the keystrokes, the frame's time on
/// the video timeline, and the style. The renderer measures text and lays the pills out via
/// [`state_at`].
pub struct KeyFrame<'a> {
    pub keys: &'a [Key],
    pub t: i64,
    pub style: &'a Style,
}

/// The keycap stack at time `t`: the live pills, newest at the bottom, up to [`Style::max`].
/// `region` is the drawn area in pixels; `measure` gives a label's width at [`Style::font`].
pub fn state_at(keys: &[Key], t: i64, style: &Style, region: (f32, f32), measure: &dyn Fn(&str) -> f32) -> Vec<Pill> {
    let base = |k: &Key| k.hold.unwrap_or(k.at);
    let mut order: Vec<&Key> = keys.iter().collect();
    order.sort_by_key(|k| k.at);

    // Collapse a burst of the same label (while the previous one is still within its hold) into
    // one pill with a repeat count.
    let mut runs: Vec<Run> = Vec::new();
    for k in order {
        let text = label(k);
        match runs.last_mut() {
            Some(r) if r.label == text && k.at <= r.base + style.hold => {
                r.base = base(k);
                r.count += 1;
            }
            _ => runs.push(Run { label: text, appear: k.at, base: base(k), count: 1 }),
        }
    }

    let end = |r: &Run| r.base + style.hold + style.fade;
    let mut live: Vec<&Run> = runs.iter().filter(|r| (r.appear..end(r)).contains(&t)).collect();
    live.sort_by_key(|r| std::cmp::Reverse(r.appear)); // newest first → bottom slot
    live.truncate(style.max);

    let margin = style.margin * region.1;
    live.iter()
        .enumerate()
        .map(|(slot, r)| {
            let text = if r.count > 1 { format!("{} \u{00d7}{}", r.label, r.count) } else { r.label.clone() };
            let w = measure(&text) + 2.0 * style.pad;
            let fs = r.base + style.hold;
            let fade = if t < fs { 1.0 } else { 1.0 - (t - fs) as f32 / style.fade as f32 };
            let reveal = ease_out(((t - r.appear) as f32 / style.rise as f32).clamp(0.0, 1.0));
            let left = match style.pos {
                Position::Left => margin,
                Position::Centre => (region.0 - w) / 2.0,
                Position::Right => region.0 - margin - w,
            };
            let bottom = region.1 - margin - slot as f32 * (style.height + style.gap) + (1.0 - reveal) * style.slide;
            let rect = Rect::from_min_size(pos2(left, bottom - style.height), vec2(w, style.height));
            Pill {
                text,
                rect,
                alpha: (fade * reveal).clamp(0.0, 1.0),
                radius: style.radius,
                font: style.font,
                fill: style.fill,
                text_color: style.text,
                accent: style.accent,
            }
        })
        .collect()
}

/// Whether the keycap stack changes right after `t` (a pill sliding in or fading out), so a
/// burnt-in video needs fresh frames even when nothing else moves.
pub fn moving(keys: &[Key], t: i64, style: &Style) -> bool {
    keys.iter().any(|k| {
        let base = k.hold.unwrap_or(k.at);
        let rising = (k.at..k.at + style.rise).contains(&t);
        let fs = base + style.hold;
        let fading = (fs..fs + style.fade).contains(&t);
        rising || fading
    })
}

/// Stores `payload` in the finalized MP4 at `path` (moov/udta/glky).
pub fn save(path: &Path, payload: &Payload) -> Result<(), String> {
    let json = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    mp4::set_movie_box(path, &BOX, &json)
}

/// The keystrokes saved in the MP4 at `path`; `None` when it has none (the overlay was off or
/// burnt into the video).
pub fn load(path: &Path) -> Result<Option<Payload>, String> {
    let Some(json) = mp4::movie_box(path, &BOX)? else { return Ok(None) };
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let v: Versioned = serde_json::from_slice(&json).map_err(|e| format!("keystrokes: {e}"))?;
    if v.version > VERSION {
        return Err(format!("keystrokes were saved by a newer glint (format {})", v.version));
    }
    serde_json::from_slice(&json).map(Some).map_err(|e| format!("keystrokes: {e}"))
}
