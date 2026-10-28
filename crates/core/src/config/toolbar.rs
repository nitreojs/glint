//! The screenshot editor's toolbar: which tools it shows, in what order, where separators split
//! them into groups, and each tool's keys.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer};

/// The editor's tools by the names config.toml uses, in the default toolbar order.
pub const TOOLS: [&str; 12] =
    ["pen", "highlighter", "arrow", "rectangle", "blur", "pixelate", "eraser", "text", "step", "cursor", "grab", "clipboard"];
/// What config.toml writes for a separator.
pub const SEPARATOR: &str = "|";
/// Picks the region in draw-first shots, so no tool can have it.
pub const RESERVED: u8 = b'Q';

const fn keys(digit: Option<u8>, letter: u8) -> Keys {
    Keys { digit, letter: Some(letter) }
}

const DEFAULT_KEYS: [Keys; TOOLS.len()] = [
    keys(Some(b'1'), b'P'),
    keys(Some(b'2'), b'H'),
    keys(Some(b'3'), b'A'),
    keys(Some(b'4'), b'R'),
    keys(Some(b'5'), b'B'),
    keys(Some(b'6'), b'X'),
    keys(Some(b'9'), b'E'),
    keys(Some(b'7'), b'T'),
    keys(Some(b'8'), b'N'),
    keys(Some(b'0'), b'M'),
    keys(None, b'G'),
    keys(None, b'V'),
];

/// A place on the toolbar: a tool (an index into [`TOOLS`]) or a separator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    Tool(usize),
    Separator,
}

/// The toolbar as it comes out of the box: every tool, no separators.
pub fn default_slots() -> Vec<Slot> {
    (0..TOOLS.len()).map(Slot::Tool).collect()
}

/// `slots` as the toolbar shows them: each tool once, no separator first, last or twice in a row.
pub fn tidy(slots: impl IntoIterator<Item = Slot>) -> Vec<Slot> {
    let mut out: Vec<Slot> = Vec::new();
    for slot in slots {
        match slot {
            Slot::Tool(t) if t >= TOOLS.len() || out.contains(&slot) => {}
            Slot::Separator if matches!(out.last(), None | Some(Slot::Separator)) => {}
            _ => out.push(slot),
        }
    }
    if out.last() == Some(&Slot::Separator) {
        out.pop();
    }
    out
}

/// The tools `slots` leaves out, in [`TOOLS`] order.
pub fn hidden(slots: &[Slot]) -> impl Iterator<Item = usize> + '_ {
    (0..TOOLS.len()).filter(|t| !slots.contains(&Slot::Tool(*t)))
}

/// Reads `toolbar`: unknown names are skipped, and so is a tool named twice.
pub(super) fn de_slots<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Slot>, D::Error> {
    let names = Vec::<String>::deserialize(d)?;
    Ok(tidy(names.iter().filter_map(|n| {
        let n = n.trim().to_ascii_lowercase();
        if n == SEPARATOR { Some(Slot::Separator) } else { TOOLS.iter().position(|t| *t == n).map(Slot::Tool) }
    })))
}

/// `toolbar = [...]`, commented out while it's the default (so new tools show up by themselves).
pub(super) fn render_slots(slots: &[Slot]) -> String {
    let names: Vec<String> = slots
        .iter()
        .map(|s| match s {
            Slot::Tool(t) => format!("\"{}\"", TOOLS[*t]),
            Slot::Separator => format!("\"{SEPARATOR}\""),
        })
        .collect();
    let line = format!("toolbar = [{}]", names.join(", "));
    if slots == default_slots() { format!("# {line}") } else { line }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Digit,
    Letter,
}

/// What kind of tool key `key` (an uppercase letter or a digit) can be; none for Q.
pub fn kind(key: u8) -> Option<KeyKind> {
    match key {
        b'0'..=b'9' => Some(KeyKind::Digit),
        RESERVED => None,
        b'A'..=b'Z' => Some(KeyKind::Letter),
        _ => None,
    }
}

/// A tool's keys: a digit and an (uppercase) letter, either optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    pub digit: Option<u8>,
    pub letter: Option<u8>,
}

impl Keys {
    pub fn get(&self, kind: KeyKind) -> Option<u8> {
        match kind {
            KeyKind::Digit => self.digit,
            KeyKind::Letter => self.letter,
        }
    }

    fn slot(&mut self, kind: KeyKind) -> &mut Option<u8> {
        match kind {
            KeyKind::Digit => &mut self.digit,
            KeyKind::Letter => &mut self.letter,
        }
    }

    /// "1 P", "G", or "" for none.
    pub fn text(&self) -> String {
        [self.digit, self.letter].into_iter().flatten().map(|k| (k as char).to_string()).collect::<Vec<_>>().join(" ")
    }

    /// The first digit and the first letter in `text` ("1 P", "p", "7"); anything else is skipped.
    fn parse(text: &str) -> Keys {
        let mut keys = Keys::default();
        for key in text.split_whitespace().filter(|w| w.len() == 1).map(|w| w.as_bytes()[0].to_ascii_uppercase()) {
            if let Some(k) = kind(key) {
                keys.slot(k).get_or_insert(key);
            }
        }
        keys
    }
}

/// Every tool's keys, indexed like [`TOOLS`]; no key belongs to two tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolKeys(pub [Keys; TOOLS.len()]);

impl Default for ToolKeys {
    fn default() -> ToolKeys {
        ToolKeys(DEFAULT_KEYS)
    }
}

impl ToolKeys {
    /// The tool `key` (an uppercase letter or a digit) belongs to.
    pub fn owner(&self, key: u8) -> Option<usize> {
        self.0.iter().position(|k| k.digit == Some(key) || k.letter == Some(key))
    }

    /// Gives `tool` the digit or letter `key` (none clears it). A tool that had it gets `tool`'s
    /// old one in exchange, and is returned.
    pub fn set(&mut self, tool: usize, kind: KeyKind, key: Option<u8>) -> Option<usize> {
        let old = self.0[tool].get(kind);
        let other = key.and_then(|k| self.owner(k)).filter(|&o| o != tool);
        if let Some(o) = other {
            *self.0[o].slot(kind) = old;
        }
        *self.0[tool].slot(kind) = key;
        other
    }
}

/// Reads `toolbar_keys`: tools named there take their keys first, then the others keep their
/// defaults where those are still free.
pub(super) fn de_keys<'de, D: Deserializer<'de>>(d: D) -> Result<ToolKeys, D::Error> {
    let table = BTreeMap::<String, String>::deserialize(d)?;
    let mut listed: [Option<Keys>; TOOLS.len()] = [None; TOOLS.len()];
    for (name, text) in &table {
        if let Some(t) = TOOLS.iter().position(|n| n.eq_ignore_ascii_case(name.trim())) {
            listed[t] = Some(Keys::parse(text));
        }
    }
    let mut out = ToolKeys([Keys::default(); TOOLS.len()]);
    let mut taken = Vec::new();
    let order = (0..TOOLS.len()).filter(|&t| listed[t].is_some()).chain((0..TOOLS.len()).filter(|&t| listed[t].is_none()));
    for t in order {
        let wanted = listed[t].unwrap_or(DEFAULT_KEYS[t]);
        for kind in [KeyKind::Digit, KeyKind::Letter] {
            if let Some(k) = wanted.get(kind).filter(|k| !taken.contains(k)) {
                taken.push(k);
                *out.0[t].slot(kind) = Some(k);
            }
        }
    }
    Ok(out)
}

/// `toolbar_keys = { ... }` with the tools whose keys aren't the default ones.
pub(super) fn render_keys(keys: &ToolKeys) -> String {
    let changed: Vec<String> = (0..TOOLS.len())
        .filter(|&t| keys.0[t] != DEFAULT_KEYS[t])
        .map(|t| format!("{} = \"{}\"", TOOLS[t], keys.0[t].text()))
        .collect();
    if changed.is_empty() { "toolbar_keys = {}".into() } else { format!("toolbar_keys = {{ {} }}", changed.join(", ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> usize {
        TOOLS.iter().position(|t| *t == name).unwrap()
    }

    #[test]
    fn tidy_drops_stray_separators_and_repeats() {
        let (s, t) = (Slot::Separator, Slot::Tool);
        assert_eq!(tidy([s, t(0), s, s, t(2), t(0), s, t(99), s]), [t(0), s, t(2)]);
        assert_eq!(tidy([s, s]), []);
        assert_eq!(tidy(default_slots()), default_slots());
        assert_eq!(hidden(&[t(1), s, t(3)]).count(), TOOLS.len() - 2);
    }

    #[test]
    fn keys_parse_loosely() {
        assert_eq!(Keys::parse("1 P"), keys(Some(b'1'), b'P'));
        assert_eq!(Keys::parse(" p  7 x 3 "), keys(Some(b'7'), b'P'), "the first of each");
        assert_eq!(Keys::parse("q 12 ctrl+a é"), Keys::default(), "Q and anything longer than a key are skipped");
        assert_eq!(Keys::default().text(), "");
        assert_eq!(keys(None, b'G').text(), "G");
    }

    #[test]
    fn setting_a_taken_key_swaps() {
        let mut keys = ToolKeys::default();
        let (eraser, pixelate, grab) = (tool("eraser"), tool("pixelate"), tool("grab"));
        assert_eq!(keys.set(eraser, KeyKind::Digit, Some(b'6')), Some(pixelate));
        assert_eq!((keys.0[eraser].digit, keys.0[pixelate].digit), (Some(b'6'), Some(b'9')));
        assert_eq!(keys.set(grab, KeyKind::Digit, Some(b'9')), Some(pixelate), "a tool with no digit takes one");
        assert_eq!((keys.0[grab].digit, keys.0[pixelate].digit), (Some(b'9'), None));
        assert_eq!(keys.set(grab, KeyKind::Letter, Some(b'G')), None, "its own key: nothing moves");
        assert_eq!(keys.set(grab, KeyKind::Letter, None), None);
        assert_eq!(keys.0[grab], Keys { digit: Some(b'9'), letter: None });
        assert_eq!(keys.owner(b'G'), None);
        assert_eq!(keys.owner(b'6'), Some(eraser));
        assert_eq!(kind(b'Q'), None);
    }
}
