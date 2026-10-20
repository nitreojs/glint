use std::path::PathBuf;

use serde::Deserialize;

use crate::recorder::Codec;
use crate::util::{app_dir, videos_dir};

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    pub screenshot_hotkey: String,
    pub launch_at_startup: bool,
    pub freeze_screen: bool,
    pub output_dir: String,
    /// File name pattern without the extension; see [`Config::file_stem`].
    pub file_name: String,
    pub fps: u32,
    pub codec: CodecName,
    pub quality: u32,
    pub effort: u32,
    pub draw_cursor: bool,
    pub screenshot_format: ShotFormat,
    /// JPG quality, 50-100.
    pub screenshot_quality: u32,
    pub screenshot_copy: bool,
    pub screenshot_save: bool,
}

pub const DEFAULT_FILE_NAME: &str = "glint_{date}_{time}";
/// Characters Windows doesn't allow in file names.
pub const BAD_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CodecName {
    H264,
    Hevc,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShotFormat {
    Png,
    Jpg,
}

impl CodecName {
    pub fn codec(self) -> Codec {
        match self {
            CodecName::H264 => Codec::H264,
            CodecName::Hevc => Codec::Hevc,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hotkey: "Ctrl+Alt+PrintScreen".into(),
            screenshot_hotkey: "Ctrl+PrintScreen".into(),
            launch_at_startup: false,
            freeze_screen: true,
            output_dir: String::new(),
            file_name: DEFAULT_FILE_NAME.into(),
            fps: 0,
            codec: CodecName::H264,
            quality: 22,
            effort: 50,
            draw_cursor: true,
            screenshot_format: ShotFormat::Png,
            screenshot_quality: 90,
            screenshot_copy: true,
            screenshot_save: true,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        app_dir().join("config.toml")
    }

    /// Loads the settings file, creating it with defaults on first run.
    pub fn load() -> Result<Config, String> {
        let path = Self::path();
        if !path.exists() {
            Config::default().save()?;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let record = parse_hotkey(&cfg.hotkey)?;
        if parse_hotkey(&cfg.screenshot_hotkey).map_err(|e| format!("screenshot_hotkey: {e}"))? == record {
            return Err("hotkey and screenshot_hotkey must differ".into());
        }
        if cfg.quality > 51 {
            return Err(format!("quality must be 0-51, got {}", cfg.quality));
        }
        if !(50..=100).contains(&cfg.screenshot_quality) {
            return Err(format!("screenshot_quality must be 50-100, got {}", cfg.screenshot_quality));
        }
        if let Some(problem) = file_name_problem(&cfg.file_name) {
            return Err(format!("file_name {}: {problem}", toml::Value::String(cfg.file_name.clone())));
        }
        Ok(cfg)
    }

    /// Writes the settings file (with its explanatory comments).
    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.render())
            .and_then(|_| std::fs::rename(&tmp, &path))
            .map_err(|e| format!("can't write {}: {e}", path.display()))
    }

    fn render(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let codec = match self.codec {
            CodecName::H264 => "h264",
            CodecName::Hevc => "hevc",
        };
        let format = match self.screenshot_format {
            ShotFormat::Png => "png",
            ShotFormat::Jpg => "jpg",
        };
        format!(
            r#"# glint settings. "Settings…" in the tray menu edits this file; hand edits apply the
# next time a recording starts.

# Starts region selection; pressing it again stops the recording.
# Modifiers: Ctrl, Alt, Shift, Win. Keys: A-Z, 0-9, F1-F24, PrintScreen, Pause, ScrollLock,
# Insert, Delete, Home, End, PageUp, PageDown, Space, Tab, Enter, Backspace, Up, Down, Left,
# Right, Num0-Num9.
hotkey = {hotkey}

# Starts a screenshot: select a region or window, mark it up, Enter to finish.
screenshot_hotkey = {screenshot_hotkey}

# Start glint (in the tray) when you sign in to Windows.
launch_at_startup = {launch_at_startup}

# While selecting a region, show a still image of the screen (true) or the live screen,
# dimmed (false).
freeze_screen = {freeze_screen}

# Folder for recordings and screenshots. Empty = <Videos>\glint
output_dir = {output_dir}

# File names, without the extension. {{date}} becomes 2026-10-20 and {{time}} 14-32-07
# (local time); a number is appended if the file already exists.
file_name = {file_name}

# Frame rate cap. 0 = the native refresh rate of the monitor being recorded.
fps = {fps}

# "h264" plays everywhere; "hevc" is ~30% smaller at the same quality (Telegram, Discord,
# browsers with hardware decoding, VLC/mpv).
codec = "{codec}"

# Constant quality (QP, 0-51): lower = sharper and bigger. 18 is near-lossless,
# 22 is the default, 26-28 gives small files.
quality = {quality}

# Encoder effort 0-100: higher compresses a bit better but loads the GPU more. If the
# "saved" notification reports dropped frames, lower this.
effort = {effort}

draw_cursor = {draw_cursor}

# Screenshots: "png" (lossless) or "jpg" (smaller; screenshot_quality 50-100 applies).
screenshot_format = "{format}"
screenshot_quality = {screenshot_quality}
# What happens when a screenshot is finished: copy it, save it to the folder, or both.
screenshot_copy = {screenshot_copy}
screenshot_save = {screenshot_save}
"#,
            hotkey = q(&self.hotkey),
            screenshot_hotkey = q(&self.screenshot_hotkey),
            launch_at_startup = self.launch_at_startup,
            freeze_screen = self.freeze_screen,
            output_dir = q(&self.output_dir),
            file_name = q(&self.file_name),
            fps = self.fps,
            quality = self.quality,
            effort = self.effort,
            draw_cursor = self.draw_cursor,
            screenshot_quality = self.screenshot_quality,
            screenshot_copy = self.screenshot_copy,
            screenshot_save = self.screenshot_save,
        )
    }

    pub fn output_dir(&self) -> PathBuf {
        if self.output_dir.trim().is_empty() {
            videos_dir().join("glint")
        } else {
            PathBuf::from(self.output_dir.trim())
        }
    }

    /// File name (without extension) for a capture made at `date` / `time`.
    pub fn file_stem(&self, date: &str, time: &str) -> String {
        let pattern = match self.file_name.trim() {
            "" => DEFAULT_FILE_NAME,
            p => p,
        };
        pattern.replace("{date}", date).replace("{time}", time).trim_end_matches(['.', ' ']).to_string()
    }
}

/// Why `pattern` can't name files, if it can't.
pub fn file_name_problem(pattern: &str) -> Option<&'static str> {
    pattern.contains(BAD_NAME_CHARS).then_some("file names can't contain \\ / : * ? \" < > |")
}

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

/// Parses "Ctrl+Alt+PrintScreen" into (RegisterHotKey modifiers, virtual key).
pub fn parse_hotkey(s: &str) -> Result<(u32, u32), String> {
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
pub fn key_name(vk: u32) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(vk as u8).to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x6F)),
        0x60..=0x69 => Some(format!("Num{}", vk - 0x60)),
        _ => NAMED_KEYS.iter().find(|&&(_, v)| v == vk).map(|(names, _)| names[0].to_string()),
    }
}

/// Formats modifiers + key the way [`parse_hotkey`] reads them, e.g. "Ctrl+Alt+PrintScreen".
pub fn hotkey_string(mods: u32, vk: u32) -> Option<String> {
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

    #[test]
    fn saved_file_parses_back_to_the_same_settings() {
        let cfg = Config {
            hotkey: "Shift+Pause".into(),
            screenshot_hotkey: "Win+Shift+S".into(),
            launch_at_startup: true,
            freeze_screen: false,
            output_dir: r#"D:\clips\"quoted" dir\"#.into(),
            file_name: "clip {date} at {time}".into(),
            fps: 60,
            codec: CodecName::Hevc,
            quality: 18,
            effort: 75,
            draw_cursor: false,
            screenshot_format: ShotFormat::Jpg,
            screenshot_quality: 75,
            screenshot_copy: false,
            screenshot_save: true,
        };
        let back: Config = toml::from_str(&cfg.render()).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(toml::from_str::<Config>(&Config::default().render()).unwrap(), Config::default());
    }

    #[test]
    fn file_names_fill_in_the_pattern() {
        let named = |pattern: &str| Config { file_name: pattern.into(), ..Config::default() };
        assert_eq!(named(DEFAULT_FILE_NAME).file_stem("2026-10-20", "14-32-07"), "glint_2026-10-20_14-32-07");
        assert_eq!(named("{time} {time}").file_stem("d", "t"), "t t");
        // Empty means the default; trailing dots and spaces aren't valid at the end of a name.
        assert_eq!(named("  ").file_stem("d", "t"), "glint_d_t");
        assert_eq!(named("clip {date}. ").file_stem("d", "t"), "clip d");
        assert!(file_name_problem("a/b").is_some() && file_name_problem("glint {date}").is_none());
    }
}
