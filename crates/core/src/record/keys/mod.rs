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
    /// How long a row stays fully shown after its last key's (release or press) time.
    pub hold: i64,
    /// How long it then takes to fade out.
    pub fade: i64,
    /// How long a row slides and fades in.
    pub rise: i64,
    /// A gap longer than this ends a typing burst, so the next key starts a new row.
    pub burst: i64,
    pub height: f32,
    /// Horizontal padding around a cap's text.
    pub pad: f32,
    /// Smallest cap width, so a single letter stays squarish.
    pub min_cap: f32,
    /// Gap between caps within a row.
    pub cap_gap: f32,
    /// Gap between stacked rows.
    pub row_gap: f32,
    pub radius: f32,
    pub font: f32,
    /// The raised bottom edge of a cap, in pixels (the keycap's depth).
    pub depth: f32,
    /// Distance a row slides up as it appears.
    pub slide: f32,
    /// Margin from the region edges, as a fraction of the region height.
    pub margin: f32,
    /// Up to this many rows stack, the newest at the bottom.
    pub max: usize,
    /// A typed-text cap keeps at most this many trailing characters.
    pub keep: usize,
    pub pos: Position,
    /// Straight-alpha RGBA of the keycap: the face, the raised bottom edge, the top highlight,
    /// the thin outline, the text, and the drop shadow.
    pub fill: [f32; 4],
    pub edge: [f32; 4],
    pub top: [f32; 4],
    pub border: [f32; 4],
    pub text: [f32; 4],
    pub shadow: [f32; 4],
}

impl Style {
    /// `scale`: the region's monitor DPI / 96. `accent` is unused: keycaps are intentionally
    /// neutral (dark cap, light text) so they don't read as UI buttons.
    pub fn new(_accent: [u8; 3], scale: f32) -> Style {
        Style {
            hold: 1500 * MS,
            fade: 200 * MS,
            rise: 110 * MS,
            burst: 1000 * MS,
            height: 30.0 * scale,
            pad: 9.0 * scale,
            min_cap: 26.0 * scale,
            cap_gap: 4.0 * scale,
            row_gap: 6.0 * scale,
            radius: 6.0 * scale,
            font: 15.0 * scale,
            depth: 3.0 * scale,
            slide: 10.0 * scale,
            margin: 0.045,
            max: 3,
            keep: 30,
            pos: Position::Centre,
            fill: [0.113, 0.113, 0.121, 0.95],
            edge: [0.05, 0.05, 0.06, 0.95],
            top: [0.42, 0.42, 0.45, 0.6],
            border: [0.6, 0.6, 0.62, 0.22],
            text: [0.97, 0.97, 0.98, 1.0],
            shadow: [0.0, 0.0, 0.0, 0.38],
        }
    }

    pub fn with(mut self, pos: Position) -> Style {
        self.pos = pos;
        self
    }
}

/// One keycap to draw: its label, its rectangle in region pixels, the alpha the fade and
/// slide-in leave, and the keycap colours (`depth` is the raised-bottom-edge height). A typed
/// word is one wide cap; a chord is one cap per key in a row.
#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub text: String,
    pub rect: Rect,
    pub alpha: f32,
    pub radius: f32,
    pub font: f32,
    pub depth: f32,
    pub fill: [f32; 4],
    pub edge: [f32; 4],
    pub top: [f32; 4],
    pub border: [f32; 4],
    pub text_color: [f32; 4],
    pub shadow: [f32; 4],
}

fn ease_out(p: f32) -> f32 {
    1.0 - (1.0 - p).powi(3)
}

/// The caps a single key shows as its own row: its modifiers then its main key.
fn caps_of(k: &Key) -> Vec<String> {
    let mut caps = Vec::new();
    for (on, name) in [(k.mods.ctrl, "Ctrl"), (k.mods.alt, "Alt"), (k.mods.shift, "Shift"), (k.mods.win, "Win")] {
        if on {
            caps.push(name.to_string());
        }
    }
    caps.push(k.main.text());
    caps
}

enum Cells {
    /// A typing burst: each edit pushes a grapheme or (with `None`) backspaces one; `tail` is a
    /// special key (Enter, Esc, …) that closed the burst, shown as caps after the text.
    Text { edits: Vec<(i64, Option<String>)>, tail: Option<(i64, Vec<String>)> },
    /// A shortcut or standalone special key: its caps, shown together from `at`.
    Caps { at: i64, caps: Vec<String> },
}

struct Row {
    /// The first key's time, for the slide-in.
    appear: i64,
    /// The last key's (release or press) time, where the hold countdown starts.
    base: i64,
    cells: Cells,
}

/// Groups the keystrokes into rows: a run of typed printables (and spaces, with backspace
/// editing it) batches into one text row; a chord or a standalone special is its own row.
fn rows_of(keys: &[Key], style: &Style) -> Vec<Row> {
    let base = |k: &Key| k.hold.unwrap_or(k.at);
    let mut order: Vec<&Key> = keys.iter().collect();
    order.sort_by_key(|k| k.at);
    let mut rows: Vec<Row> = Vec::new();
    let mut open: Option<usize> = None; // the live text row, if any
    let mut last = i64::MIN; // the last key added to the open row
    for k in order {
        let printable = matches!(k.main, KeyLabel::Text(_)) && !k.mods.chord();
        let space = matches!(k.main, KeyLabel::Named(Named::Space)) && !k.mods.chord();
        let back = matches!(k.main, KeyLabel::Named(Named::Backspace)) && !k.mods.chord();
        let joins = open.is_some() && k.at - last <= style.burst;
        if k.mods.chord() {
            open = None;
            rows.push(Row { appear: k.at, base: base(k), cells: Cells::Caps { at: k.at, caps: caps_of(k) } });
        } else if printable || space {
            let ch = if space { " ".to_string() } else { k.main.text() };
            if let Some(i) = open.filter(|_| joins) {
                if let Cells::Text { edits, .. } = &mut rows[i].cells {
                    edits.push((k.at, Some(ch)));
                }
                rows[i].base = base(k);
            } else {
                rows.push(Row { appear: k.at, base: base(k), cells: Cells::Text { edits: vec![(k.at, Some(ch))], tail: None } });
                open = Some(rows.len() - 1);
            }
            last = k.at;
        } else if let (true, Some(i)) = (back, open.filter(|_| joins)) {
            if let Cells::Text { edits, .. } = &mut rows[i].cells {
                edits.push((k.at, None));
            }
            rows[i].base = base(k);
            last = k.at;
        } else if let Some(i) = open.filter(|_| joins) {
            // a special key ends the burst, shown as its own cap(s) after the text.
            if let Cells::Text { tail, .. } = &mut rows[i].cells {
                *tail = Some((k.at, caps_of(k)));
            }
            rows[i].base = base(k);
            open = None;
        } else {
            open = None;
            rows.push(Row { appear: k.at, base: base(k), cells: Cells::Caps { at: k.at, caps: caps_of(k) } });
        }
    }
    rows
}

/// The caps a row shows at time `t` (its typed text replayed up to `t`, then any tail).
fn caps_at(row: &Row, t: i64, style: &Style) -> Vec<String> {
    match &row.cells {
        Cells::Caps { at, caps } => {
            if *at <= t {
                caps.clone()
            } else {
                Vec::new()
            }
        }
        Cells::Text { edits, tail } => {
            let mut chars: Vec<String> = Vec::new();
            for (at, edit) in edits {
                if *at > t {
                    break;
                }
                match edit {
                    Some(c) => chars.push(c.clone()),
                    None => {
                        chars.pop();
                    }
                }
            }
            if chars.len() > style.keep {
                chars.drain(0..chars.len() - style.keep);
            }
            let mut caps = Vec::new();
            if !chars.is_empty() {
                caps.push(chars.concat());
            }
            if let Some((at, tail)) = tail
                && *at <= t
            {
                caps.extend(tail.clone());
            }
            caps
        }
    }
}

/// What a burnt-in frame needs to draw its keycap rows: the keystrokes, the frame's time on
/// the video timeline, and the style. The renderer measures text and lays the caps out via
/// [`state_at`].
pub struct KeyFrame<'a> {
    pub keys: &'a [Key],
    pub t: i64,
    pub style: &'a Style,
}

/// The keycaps at time `t`: rows stacked newest at the bottom (up to [`Style::max`]), the caps
/// within a row laid out left to right. `region` is the drawn area in pixels; `measure` gives a
/// label's width at [`Style::font`].
pub fn state_at(keys: &[Key], t: i64, style: &Style, region: (f32, f32), measure: &dyn Fn(&str) -> f32) -> Vec<Pill> {
    let rows = rows_of(keys, style);
    let end = |r: &Row| r.base + style.hold + style.fade;
    let mut live: Vec<(&Row, Vec<String>)> = rows
        .iter()
        .filter(|r| (r.appear..end(r)).contains(&t))
        .map(|r| (r, caps_at(r, t, style)))
        .filter(|(_, caps)| !caps.is_empty())
        .collect();
    live.sort_by_key(|(r, _)| std::cmp::Reverse(r.appear)); // newest first → bottom slot
    live.truncate(style.max);

    let margin = style.margin * region.1;
    let mut out = Vec::new();
    for (slot, (row, caps)) in live.iter().enumerate() {
        let fs = row.base + style.hold;
        let fade = if t < fs { 1.0 } else { 1.0 - (t - fs) as f32 / style.fade as f32 };
        let reveal = ease_out(((t - row.appear) as f32 / style.rise as f32).clamp(0.0, 1.0));
        let alpha = (fade * reveal).clamp(0.0, 1.0);
        let widths: Vec<f32> = caps.iter().map(|c| (measure(c) + 2.0 * style.pad).max(style.min_cap)).collect();
        let row_w = widths.iter().sum::<f32>() + style.cap_gap * (caps.len().saturating_sub(1)) as f32;
        let mut x = match style.pos {
            Position::Left => margin,
            Position::Centre => (region.0 - row_w) / 2.0,
            Position::Right => region.0 - margin - row_w,
        };
        let bottom = region.1 - margin - slot as f32 * (style.height + style.row_gap) + (1.0 - reveal) * style.slide;
        for (c, w) in caps.iter().zip(&widths) {
            let rect = Rect::from_min_size(pos2(x, bottom - style.height), vec2(*w, style.height));
            out.push(Pill {
                text: c.clone(),
                rect,
                alpha,
                radius: style.radius,
                font: style.font,
                depth: style.depth,
                fill: style.fill,
                edge: style.edge,
                top: style.top,
                border: style.border,
                text_color: style.text,
                shadow: style.shadow,
            });
            x += w + style.cap_gap;
        }
    }
    out
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
