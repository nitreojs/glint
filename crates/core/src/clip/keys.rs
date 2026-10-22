use serde::{Deserialize, Serialize};

use super::math::Range;
use crate::record::keys::{Key, KeyLabel, Mods, Payload, Position, Style};

/// How long an added key is shown before its release.
pub const TAP: i64 = 800_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySize {
    Small,
    #[default]
    Normal,
    Large,
}

impl KeySize {
    pub fn factor(self) -> f32 {
        match self {
            KeySize::Small => 0.8,
            KeySize::Normal => 1.0,
            KeySize::Large => 1.3,
        }
    }
}

/// The keystroke overlay's look, kept with the export settings: its size and where it sits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyLook {
    pub size: KeySize,
    pub pos: Position,
}

impl KeyLook {
    pub fn style(&self, accent: [u8; 3], scale: f32) -> Style {
        Style::new(accent, scale * self.size.factor()).with(self.pos)
    }
}

/// The editable keystrokes of a recording in the trimmer.
#[derive(Clone, Debug, PartialEq)]
pub struct Keys {
    pub list: Vec<Key>,
    /// Burnt into the export; hidden keys stay out of the output entirely.
    pub shown: bool,
    pub scale: f32,
}

impl Keys {
    pub fn new(payload: &Payload) -> Keys {
        Keys { list: payload.keys.clone(), shown: true, scale: payload.scale }
    }

    /// Moves the picked keys by `by`, clamped so none leaves `0..duration`.
    pub fn shift(&mut self, picked: &[usize], by: i64, duration: i64) -> bool {
        let ats = || picked.iter().filter_map(|&i| self.list.get(i)).map(|k| k.at);
        let (Some(lo), Some(hi)) = (ats().min(), ats().max()) else { return false };
        let by = by.clamp(-lo, (duration - 1 - hi).max(-lo));
        if by == 0 {
            return false;
        }
        for &i in picked {
            if let Some(k) = self.list.get_mut(i) {
                k.at += by;
                k.hold = k.hold.map(|h| h + by);
            }
        }
        true
    }

    pub fn delete(&mut self, picked: &[usize]) -> bool {
        let before = self.list.len();
        let mut i = 0;
        self.list.retain(|_| {
            i += 1;
            !picked.contains(&(i - 1))
        });
        self.list.len() != before
    }

    pub fn add(&mut self, t: i64, main: KeyLabel) -> usize {
        self.list.push(Key { at: t, hold: Some(t + TAP), mods: Mods::default(), main });
        self.list.len() - 1
    }

    pub fn between(&self, a: i64, b: i64) -> Vec<usize> {
        let (lo, hi) = (a.min(b), a.max(b));
        (0..self.list.len()).filter(|&i| (lo..=hi).contains(&self.list[i].at)).collect()
    }

    /// The keys burnt into the export: the shown ones whose pills reach into a kept range.
    pub fn burnt(&self, kept: &[Range], style: &Style) -> Vec<Key> {
        if !self.shown {
            return Vec::new();
        }
        let show = |k: &Key| {
            let (start, end) = visible(k, style);
            kept.iter().any(|r| start < r.end && end > r.start)
        };
        self.list.iter().filter(|k| show(k)).cloned().collect()
    }
}

/// The span a key's pill is on screen: from its press until the hold and fade are over.
pub fn visible(k: &Key, style: &Style) -> (i64, i64) {
    let base = k.hold.unwrap_or(k.at);
    (k.at, base + style.hold + style.fade)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::edit::{Edit, Undo};
    use crate::clip::math::SECOND;
    use crate::record::keys::Named;

    const S: i64 = SECOND;

    fn text(s: &str) -> KeyLabel {
        KeyLabel::Text(s.into())
    }

    fn key(at: i64, hold: Option<i64>, main: KeyLabel) -> Key {
        Key { at, hold, mods: Mods::default(), main }
    }

    fn keys() -> Keys {
        let list = vec![
            key(S, Some(S + S / 10), text("a")),
            key(3 * S, Some(5 * S), KeyLabel::Named(Named::Enter)),
            key(8 * S, None, KeyLabel::Func(5)),
        ];
        Keys::new(&Payload { version: 1, scale: 1.5, keys: list })
    }

    #[test]
    fn keys_move_delete_and_get_added_with_undo() {
        let mut e = Edit::new(10 * S, 0);
        e.keys = Some(keys());
        let fresh = e.clone();
        let mut u = Undo::default();
        let mut step = |e: &mut Edit, f: &dyn Fn(&mut Keys) -> bool| {
            let before = e.clone();
            let changed = f(e.keys.as_mut().unwrap());
            u.record(before, e);
            changed
        };

        assert!(step(&mut e, &|k| k.shift(&[0, 1], S / 2, 10 * S)));
        let k = &e.keys.as_ref().unwrap().list;
        assert_eq!((k[0].at, k[1].at, k[1].hold), (S + S / 2, 3 * S + S / 2, Some(5 * S + S / 2)));
        assert!(step(&mut e, &|k| k.shift(&[2], 5 * S, 10 * S)), "stops at the end");
        assert_eq!(e.keys.as_ref().unwrap().list[2].at, 10 * S - 1);
        assert!(step(&mut e, &|k| k.shift(&[0, 2], -20 * S, 10 * S)), "all stop at the start");
        assert_eq!(e.keys.as_ref().unwrap().list[0].at, 0);
        assert!(!step(&mut e, &|k| k.shift(&[0], -S, 10 * S)));
        assert!(!step(&mut e, &|k| k.shift(&[7], S, 10 * S)));

        assert!(step(&mut e, &|k| k.delete(&[0, 2])));
        assert_eq!(e.keys.as_ref().unwrap().list.len(), 1);
        assert!(!step(&mut e, &|k| k.delete(&[5])));
        assert!(step(&mut e, &|k| k.add(2 * S, text("z")) == 1));
        assert_eq!(e.keys.as_ref().unwrap().list[1], key(2 * S, Some(2 * S + TAP), text("z")));
        assert_eq!(e.keys.as_ref().unwrap().between(3 * S, 0), [1], "only the added key is in 0..=3s");

        while u.undo(&mut e) {}
        assert_eq!(e, fresh);
    }

    #[test]
    fn the_export_burns_only_shown_keys_that_show_in_the_kept_ranges() {
        let style = KeyLook::default().style([200, 30, 30], 1.0);
        let mut k = keys();
        let r = |a: i64, b: i64| Range { start: a, end: b };
        let ats = |v: Vec<Key>| v.iter().map(|k| k.at).collect::<Vec<_>>();
        assert_eq!(k.burnt(&[r(0, 10 * S)], &style).len(), 3);
        assert_eq!(ats(k.burnt(&[r(6 * S, 7 * S + S / 2)], &style)), [3 * S], "a held pill still fading past the cut");
        assert_eq!(ats(k.burnt(&[r(8 * S, 10 * S)], &style)), [8 * S], "down to the end");
        assert_eq!(ats(k.burnt(&[r(S + S / 2, 2 * S)], &style)), [S], "the 'a' pill still showing");
        k.shown = false;
        assert!(k.burnt(&[r(0, 10 * S)], &style).is_empty());
        assert_eq!(visible(&k.list[0], &style), (S, S + S / 10 + style.hold + style.fade));
        assert_eq!(visible(&k.list[2], &style), (8 * S, 8 * S + style.hold + style.fade));
    }

    #[test]
    fn looks_scale_and_place_the_style() {
        let small = KeyLook { size: KeySize::Small, pos: Position::Left }.style([1, 2, 3], 2.0);
        assert_eq!(small.pos, Position::Left);
        assert!((small.height - 30.0 * 2.0 * 0.8).abs() < 1e-4);
        let large = KeyLook { size: KeySize::Large, pos: Position::Right }.style([1, 2, 3], 1.0);
        assert!((large.font - 15.0 * 1.3).abs() < 1e-4 && large.pos == Position::Right);
        let json = serde_json::to_string(&KeyLook { size: KeySize::Large, pos: Position::Right }).unwrap();
        assert_eq!(json, r#"{"size":"large","pos":"right"}"#);
        assert_eq!(serde_json::from_str::<KeyLook>("{}").unwrap(), KeyLook::default());
    }
}
