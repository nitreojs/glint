//! `%APPDATA%\afterglow\config.toml`: written with comments on first run, read at start and on
//! "reload settings".

use std::path::PathBuf;

use glint_core::config::CodecName;
use glint_core::config::audio::{AudioSource, SourceKind};
use glint_core::config::hotkeys::parse_hotkey;
use glint_core::paths::app_dir_for;
use serde::Deserialize;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

pub const APP: &str = "afterglow";

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Buffer from launch; off waits for "resume" in the tray menu.
    pub buffer: bool,
    /// Seconds a save covers.
    pub length: u32,
    pub save_hotkey: String,
    /// Empty: Videos\afterglow.
    pub output_dir: String,
    /// 0: the monitor's refresh rate.
    pub fps: u32,
    pub codec: CodecName,
    /// Constant QP, 0-51: lower is sharper and bigger.
    pub quality: u32,
    /// The buffer never holds more than this; it keeps less time instead.
    pub max_memory_mb: u32,
    pub audio_sources: Vec<AudioSource>,
    pub launch_at_startup: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            buffer: true,
            length: 60,
            save_hotkey: "Alt+F10".into(),
            output_dir: String::new(),
            fps: 60,
            codec: CodecName::H264,
            quality: 23,
            max_memory_mb: 1024,
            audio_sources: vec![AudioSource { tracks: vec![1], ..AudioSource::new(SourceKind::Desktop, 0) }],
            launch_at_startup: false,
        }
    }
}

pub fn path() -> PathBuf {
    app_dir_for(APP).join("config.toml")
}

impl Config {
    /// The saved settings; a missing file is written with the defaults first.
    pub fn load() -> Result<Config, String> {
        let path = path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Config::default();
                std::fs::write(&path, cfg.render()).map_err(|e| format!("{}: {e}", path.display()))?;
                return Ok(cfg);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("config.toml: {e}"))?;
        cfg.check()?;
        Ok(cfg)
    }

    fn check(&self) -> Result<(), String> {
        parse_hotkey(&self.save_hotkey).map_err(|e| format!("save_hotkey: {e}"))?;
        if !(5..=1800).contains(&self.length) {
            return Err(format!("length must be 5-1800 seconds, got {}", self.length));
        }
        if self.quality > 51 {
            return Err(format!("quality must be 0-51, got {}", self.quality));
        }
        if self.fps > 240 {
            return Err(format!("fps must be 0-240, got {}", self.fps));
        }
        if self.max_memory_mb < 64 {
            return Err(format!("max_memory_mb must be at least 64, got {}", self.max_memory_mb));
        }
        Ok(())
    }

    /// Where saved clips go.
    pub fn output_dir(&self) -> PathBuf {
        if !self.output_dir.trim().is_empty() {
            return PathBuf::from(self.output_dir.trim());
        }
        videos().unwrap_or_else(|| app_dir_for(APP)).join(APP)
    }

    /// The defaults file, with what each setting does.
    fn render(&self) -> String {
        format!(
            r#"# afterglow keeps the last stretch of your screen and sound, and saves it on a hotkey.
# Edit this file, then pick "reload settings" in the tray menu.

# Buffer from launch; false waits for "resume" in the tray menu.
buffer = {buffer}

# Seconds a save covers (5-1800).
length = {length}

save_hotkey = "{hotkey}"

# Where clips are saved; empty is Videos\afterglow.
output_dir = ""

# Frames per second (0: the monitor's refresh rate), "h264" or "hevc", and constant quality
# (QP, 0-51: lower is sharper and bigger).
fps = {fps}
codec = "h264"
quality = {quality}

# The buffer never holds more than this many MB; it keeps less time instead.
max_memory_mb = {memory}

# Start afterglow when you sign in to Windows.
launch_at_startup = {autostart}

# What's heard, as in glint: "desktop" (what your speakers play), "app" (one app, by exe name)
# or "mic".
[[audio_sources]]
kind = "desktop"
enabled = true
volume = 100
tracks = [1]
"#,
            buffer = self.buffer,
            length = self.length,
            hotkey = self.save_hotkey,
            fps = self.fps,
            quality = self.quality,
            memory = self.max_memory_mb,
            autostart = self.launch_at_startup,
        )
    }
}

fn videos() -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as _));
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_written_defaults_read_back_as_the_defaults() {
        let cfg: Config = toml::from_str(&Config::default().render()).unwrap();
        assert_eq!(cfg, Config::default());
        cfg.check().unwrap();
    }

    #[test]
    fn bad_values_are_named() {
        let bad = Config { length: 2, ..Config::default() };
        assert!(bad.check().unwrap_err().contains("length"));
        let bad = Config { save_hotkey: "Alt+Nope".into(), ..Config::default() };
        assert!(bad.check().unwrap_err().contains("save_hotkey"));
    }
}
