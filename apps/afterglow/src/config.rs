//! `%APPDATA%\afterglow\config.toml`: written with comments on first run and by the settings
//! window, read at start and whenever the settings window saves.

use std::path::PathBuf;

use glint_core::config::CodecName;
use glint_core::config::audio::{AudioSource, SourceKind};
use glint_core::config::hotkeys::parse_hotkey;
use glint_core::paths::app_dir_for;
use serde::Deserialize;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

pub const APP: &str = "afterglow";
/// Lengths the settings offer, in seconds.
pub const LENGTHS: [u32; 5] = [30, 60, 120, 300, 600];
/// Memory caps the settings offer, in MB.
pub const MEMORY_CAPS: [u32; 4] = [512, 1024, 2048, 4096];

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
    /// 0: the primary monitor; n: the nth monitor (1 = the first one Windows lists).
    pub monitor: u32,
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
            monitor: 0,
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
                cfg.save()?;
                return Ok(cfg);
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("config.toml: {e}"))?;
        cfg.check()?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<(), String> {
        self.check()?;
        let path = path();
        std::fs::write(&path, self.render()).map_err(|e| format!("{}: {e}", path.display()))
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
        default_output_dir()
    }

    /// Whether a source of `kind` is on.
    pub fn hears(&self, kind: SourceKind) -> bool {
        self.audio_sources.iter().any(|s| s.kind == kind && s.enabled)
    }

    /// Turns the default `kind` source (desktop, or the default microphone) on or off. Both on,
    /// each gets its own track besides the mixed first one.
    pub fn set_hears(&mut self, kind: SourceKind, on: bool) {
        let default = |s: &AudioSource| s.kind == kind && (kind != SourceKind::Mic || s.device.is_empty());
        match self.audio_sources.iter_mut().find(|s| default(s)) {
            Some(s) => s.enabled = on,
            None if on => self.audio_sources.push(AudioSource::new(kind, 0)),
            None => {}
        }
        let both = self.hears(SourceKind::Desktop) && self.hears(SourceKind::Mic);
        for s in self.audio_sources.iter_mut().filter(|s| s.kind != SourceKind::App) {
            s.tracks = match (both, s.kind) {
                (true, SourceKind::Desktop) => vec![1, 2],
                (true, _) => vec![1, 3],
                (false, _) => vec![1],
            };
        }
    }

    /// The file, with what each setting does.
    fn render(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let codec = match self.codec {
            CodecName::H264 => "h264",
            CodecName::Hevc => "hevc",
        };
        let mut sources = String::new();
        for s in &self.audio_sources {
            let tracks: Vec<String> = s.tracks.iter().map(u32::to_string).collect();
            sources += &format!(
                "\n[[audio_sources]]\nkind = \"{}\"\nenabled = {}\nvolume = {}\ntracks = [{}]\n",
                s.kind.name(),
                s.enabled,
                s.volume,
                tracks.join(", ")
            );
            for (key, value) in [("app", &s.app), ("exclude", &s.exclude), ("device", &s.device)] {
                if !value.is_empty() {
                    sources += &format!("{key} = {}\n", q(value));
                }
            }
        }
        format!(
            r#"# afterglow keeps the last stretch of your screen and sound, and saves it on a hotkey.
# The settings window (tray menu → settings) edits this file.

# Buffer from launch; false waits for "resume" in the tray menu.
buffer = {buffer}

# Seconds a save covers (5-1800).
length = {length}

save_hotkey = {hotkey}

# Where clips are saved; empty is Videos\afterglow.
output_dir = {output_dir}

# 0 is the primary monitor; 1, 2, … the monitors in the order Windows lists them.
monitor = {monitor}

# Frames per second (0: the monitor's refresh rate), "h264" or "hevc", and constant quality
# (QP, 0-51: lower is sharper and bigger).
fps = {fps}
codec = "{codec}"
quality = {quality}

# The buffer never holds more than this many MB; it keeps less time instead.
max_memory_mb = {memory}

# Start afterglow when you sign in to Windows.
launch_at_startup = {autostart}

# What's heard, as in glint: "desktop" (what your speakers play), "app" (one app, by exe name)
# or "mic" (a device id, or the default microphone without one).
{sources}"#,
            buffer = self.buffer,
            length = self.length,
            hotkey = q(&self.save_hotkey),
            output_dir = q(&self.output_dir),
            monitor = self.monitor,
            fps = self.fps,
            quality = self.quality,
            memory = self.max_memory_mb,
            autostart = self.launch_at_startup,
        )
    }
}

pub fn default_output_dir() -> PathBuf {
    videos().unwrap_or_else(|| app_dir_for(APP)).join(APP)
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
    fn saved_settings_read_back_the_same() {
        let cfg: Config = toml::from_str(&Config::default().render()).unwrap();
        assert_eq!(cfg, Config::default());
        let mut edited = Config {
            length: 300,
            save_hotkey: "Ctrl+Shift+F9".into(),
            output_dir: r#"D:\clips "best""#.into(),
            monitor: 2,
            fps: 0,
            codec: CodecName::Hevc,
            quality: 18,
            max_memory_mb: 4096,
            launch_at_startup: true,
            buffer: false,
            ..Config::default()
        };
        edited.set_hears(SourceKind::Mic, true);
        edited.audio_sources.push(AudioSource { app: "game.exe".into(), ..AudioSource::new(SourceKind::App, 2) });
        let back: Config = toml::from_str(&edited.render()).unwrap();
        assert_eq!(back, edited);
    }

    #[test]
    fn desktop_and_mic_share_the_first_track_and_keep_their_own() {
        let mut cfg = Config::default();
        assert!(cfg.hears(SourceKind::Desktop) && !cfg.hears(SourceKind::Mic));
        cfg.set_hears(SourceKind::Mic, true);
        let tracks: Vec<_> = cfg.audio_sources.iter().map(|s| (s.kind, s.tracks.clone())).collect();
        assert_eq!(tracks, [(SourceKind::Desktop, vec![1, 2]), (SourceKind::Mic, vec![1, 3])]);
        cfg.set_hears(SourceKind::Desktop, false);
        assert!(!cfg.hears(SourceKind::Desktop) && cfg.hears(SourceKind::Mic));
        assert!(cfg.audio_sources.iter().all(|s| s.tracks == [1]), "alone, the mic is the first track");
        cfg.audio_sources.push(AudioSource { app: "game.exe".into(), ..AudioSource::new(SourceKind::App, 2) });
        cfg.set_hears(SourceKind::Desktop, true);
        assert_eq!(cfg.audio_sources[2].tracks, [1, 4], "app sources keep the tracks they were given");
    }

    #[test]
    fn bad_values_are_named() {
        let bad = Config { length: 2, ..Config::default() };
        assert!(bad.check().unwrap_err().contains("length"));
        let bad = Config { save_hotkey: "Alt+Nope".into(), ..Config::default() };
        assert!(bad.check().unwrap_err().contains("save_hotkey"));
    }
}
