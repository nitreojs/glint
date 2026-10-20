use std::path::PathBuf;

use serde::Deserialize;

use crate::recorder::Codec;
use crate::util::{app_dir, videos_dir};

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    pub output_dir: String,
    pub fps: u32,
    pub codec: CodecName,
    pub quality: u32,
    pub effort: u32,
    pub draw_cursor: bool,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CodecName {
    H264,
    Hevc,
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
            output_dir: String::new(),
            fps: 0,
            codec: CodecName::H264,
            quality: 22,
            effort: 50,
            draw_cursor: true,
        }
    }
}

const TEMPLATE: &str = r#"# hzrec settings. After editing, pick "Reload settings" in the tray menu.

# Starts region selection; pressing it again stops the recording.
# Modifiers: Ctrl, Alt, Shift, Win. Keys: A-Z, 0-9, F1-F24, PrintScreen, Pause, ScrollLock,
# Insert, Delete, Home, End, PageUp, PageDown, Space, Tab, Enter, Backspace, Up, Down, Left,
# Right, Num0-Num9.
hotkey = "Ctrl+Alt+PrintScreen"

# Folder for recordings. Empty = <Videos>\hzrec
output_dir = ""

# Frame rate cap. 0 = the native refresh rate of the monitor being recorded.
fps = 0

# "h264" plays everywhere; "hevc" is ~30% smaller at the same quality (Telegram, Discord,
# browsers with hardware decoding, VLC/mpv).
codec = "h264"

# Constant quality (QP, 0-51): lower = sharper and bigger. 18 is near-lossless,
# 22 is the default, 26-28 gives small files.
quality = 22

# Encoder effort 0-100: higher compresses a bit better but loads the GPU more. If the
# "saved" notification reports dropped frames, lower this.
effort = 50

draw_cursor = true
"#;

impl Config {
    pub fn path() -> PathBuf {
        app_dir().join("config.toml")
    }

    /// Loads the settings file, creating it with defaults on first run.
    pub fn load() -> Result<Config, String> {
        let path = Self::path();
        if !path.exists() {
            std::fs::write(&path, TEMPLATE).map_err(|e| format!("can't create {}: {e}", path.display()))?;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        parse_hotkey(&cfg.hotkey)?;
        if cfg.quality > 51 {
            return Err(format!("quality must be 0-51, got {}", cfg.quality));
        }
        Ok(cfg)
    }

    pub fn output_dir(&self) -> PathBuf {
        if self.output_dir.trim().is_empty() {
            videos_dir().join("hzrec")
        } else {
            PathBuf::from(self.output_dir.trim())
        }
    }
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
    Some(match p {
        "printscreen" | "prtsc" | "print" | "snapshot" => 0x2C,
        "pause" | "break" => 0x13,
        "scrolllock" => 0x91,
        "insert" | "ins" => 0x2D,
        "delete" | "del" => 0x2E,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "space" => 0x20,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "backspace" => 0x08,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        _ => return None,
    })
}
