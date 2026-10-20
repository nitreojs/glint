pub(crate) const MOD_ALT: u32 = 0x1;
pub(crate) const MOD_CONTROL: u32 = 0x2;
pub(crate) const MOD_SHIFT: u32 = 0x4;
pub(crate) const MOD_WIN: u32 = 0x8;

/// Parses "Ctrl+Alt+PrintScreen" into (RegisterHotKey modifiers, virtual key).
pub(crate) fn parse_hotkey(s: &str) -> Result<(u32, u32), String> {
    let mut mods = 0;
    let mut key = None;
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        let p = part.to_ascii_lowercase();
        let m = match p.as_str() {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "windows" | "super" => MOD_WIN,
            _ => 0,
        };
        if m != 0 {
            mods |= m;
            continue;
        }
        if key.is_some() {
            return Err(format!("hotkey \"{s}\" has more than one non-modifier key"));
        }
        key = Some(vk_from_name(&p).ok_or_else(|| format!("unknown key \"{part}\" in hotkey \"{s}\""))?);
    }
    let vk = key.ok_or_else(|| format!("hotkey \"{s}\" has no key, only modifiers"))?;
    Ok((mods, vk))
}

/// Keys with a name of their own (the first spelling is canonical), by virtual-key code.
const NAMED_KEYS: &[(&[&str], u32)] = &[
    (&["PrintScreen", "PrtSc", "Print", "Snapshot"], 0x2C),
    (&["Pause", "Break"], 0x13),
    (&["ScrollLock"], 0x91),
    (&["Insert", "Ins"], 0x2D),
    (&["Delete", "Del"], 0x2E),
    (&["Home"], 0x24),
    (&["End"], 0x23),
    (&["PageUp", "PgUp"], 0x21),
    (&["PageDown", "PgDn"], 0x22),
    (&["Space"], 0x20),
    (&["Tab"], 0x09),
    (&["Enter", "Return"], 0x0D),
    (&["Backspace"], 0x08),
    (&["Up"], 0x26),
    (&["Down"], 0x28),
    (&["Left"], 0x25),
    (&["Right"], 0x27),
];

fn vk_from_name(p: &str) -> Option<u32> {
    let b = p.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
        return Some(b[0].to_ascii_uppercase() as u32);
    }
    if let Some(n) = p.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        return (1..=24).contains(&n).then(|| 0x70 + n - 1);
    }
    if let Some(n) = p.strip_prefix("num").and_then(|n| n.parse::<u32>().ok()) {
        return (n <= 9).then(|| 0x60 + n);
    }
    NAMED_KEYS.iter().find(|(names, _)| names.iter().any(|n| n.eq_ignore_ascii_case(p))).map(|&(_, vk)| vk)
}

/// Canonical name of a virtual key, if hotkeys support it.
pub(crate) fn key_name(vk: u32) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(vk as u8).to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x6F)),
        0x60..=0x69 => Some(format!("Num{}", vk - 0x60)),
        _ => NAMED_KEYS.iter().find(|&&(_, v)| v == vk).map(|(names, _)| names[0].to_string()),
    }
}

/// Formats modifiers + key the way [`parse_hotkey`] reads them, e.g. "Ctrl+Alt+PrintScreen".
pub(crate) fn hotkey_string(mods: u32, vk: u32) -> Option<String> {
    let mut s = String::new();
    for (bit, name) in [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")] {
        if mods & bit != 0 {
            s += name;
            s += "+";
        }
    }
    Some(s + &key_name(vk)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_round_trip_to_canonical_spelling() {
        for (input, canonical) in [
            ("Ctrl+Alt+PrintScreen", "Ctrl+Alt+PrintScreen"),
            ("alt + ctrl + prtsc", "Ctrl+Alt+PrintScreen"),
            ("shift+win+f12", "Shift+Win+F12"),
            ("Ctrl+num7", "Ctrl+Num7"),
            ("Pause", "Pause"),
            ("ctrl+x", "Ctrl+X"),
        ] {
            let (mods, vk) = parse_hotkey(input).unwrap();
            assert_eq!(hotkey_string(mods, vk).as_deref(), Some(canonical), "{input}");
        }
    }

    #[test]
    fn bad_hotkeys_are_rejected() {
        for bad in ["Ctrl+Alt", "Ctrl+Bogus", "A+B", "F25", ""] {
            assert!(parse_hotkey(bad).is_err(), "{bad}");
        }
        assert_eq!(hotkey_string(MOD_CONTROL, 0xFF), None); // keys without a name can't be saved
    }
}
