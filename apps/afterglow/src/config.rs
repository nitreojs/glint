//! `%APPDATA%\afterglow\config.toml`: written with comments on first run and by the settings
//! window, read at start and whenever the settings window saves.

use std::path::PathBuf;

use glint_core::config::CodecName;
use glint_core::config::audio::{AudioSource, MAX_TRACKS, SourceKind};
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
    /// Average Mbit/s, which fixes the buffer's size; 0 records at the constant `quality` instead.
    pub bitrate: u32,
    /// Constant QP, 0-51: lower is sharper and bigger.
    pub quality: u32,
    /// The buffer never holds more than this; it keeps less time instead.
    pub max_memory_mb: u32,
    pub audio_sources: Vec<AudioSource>,
    /// What players call each track; empty names it after its sources.
    pub audio_track_names: Vec<String>,
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
            // hevc needs a decoder windows doesn't ship to be played or trimmed: opt in
            codec: CodecName::H264,
            bitrate: 15,
            quality: 23,
            max_memory_mb: 1024,
            audio_sources: vec![AudioSource { tracks: vec![1], ..AudioSource::new(SourceKind::Desktop, 0) }],
            audio_track_names: vec![String::new(); MAX_TRACKS as usize],
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
        let mut cfg: Config = toml::from_str(&text).map_err(|e| format!("config.toml: {e}"))?;
        cfg.check()?;
        cfg.audio_track_names.resize(MAX_TRACKS as usize, String::new());
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
        if self.bitrate > 200 {
            return Err(format!("bitrate must be 0-200 Mbit/s, got {}", self.bitrate));
        }
        if self.quality > 51 {
            return Err(format!("quality must be 0-51, got {}", self.quality));
        }
        if self.fps > 240 {
            return Err(format!("fps must be 0-240, got {}", self.fps));
        }
        if self.audio_track_names.len() > MAX_TRACKS as usize {
            return Err(format!("audio_track_names has {} names, at most {MAX_TRACKS} fit", self.audio_track_names.len()));
        }
        if self.max_memory_mb < 64 {
            return Err(format!("max_memory_mb must be at least 64, got {}", self.max_memory_mb));
        }
        Ok(())
    }

    /// About how many bytes `secs` of recording take, when that's known (a bitrate is set).
    pub fn bytes_for(&self, secs: u32) -> Option<u64> {
        // the sound: 192 kbit/s per track
        let tracks = self.audio_sources.iter().filter(|s| s.enabled).flat_map(|s| s.tracks.iter()).max().copied();
        let audio = tracks.unwrap_or(0) as u64 * 24_000;
        (self.bitrate > 0).then(|| (self.bitrate as u64 * 125_000 + audio) * secs as u64)
    }

    /// Where saved clips go.
    pub fn output_dir(&self) -> PathBuf {
        if !self.output_dir.trim().is_empty() {
            return PathBuf::from(self.output_dir.trim());
        }
        default_output_dir()
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

# Frames per second (0: the monitor's refresh rate) and "h264" or "hevc" (about 30 % smaller).
fps = {fps}
codec = "{codec}"

# Average Mbit/s (a minute takes about 7.5 MB per Mbit/s); 0 uses the constant quality below
# instead (QP, 0-51: lower is sharper and bigger), whose size depends on what's on screen.
bitrate = {bitrate}
quality = {quality}

# The buffer never holds more than this many MB; it keeps less time instead.
max_memory_mb = {memory}

# Start afterglow when you sign in to Windows.
launch_at_startup = {autostart}

# What players call tracks 1-6; an empty name is made from the track's sources.
audio_track_names = [{names}]

# What's heard, as in glint: "desktop" (what your speakers play, minus an `exclude`d app), "app"
# (one app by exe name) or "mic" (a device id, or the default microphone without one).
{sources}"#,
            buffer = self.buffer,
            length = self.length,
            hotkey = q(&self.save_hotkey),
            output_dir = q(&self.output_dir),
            monitor = self.monitor,
            fps = self.fps,
            bitrate = self.bitrate,
            quality = self.quality,
            memory = self.max_memory_mb,
            autostart = self.launch_at_startup,
            names = self.audio_track_names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", "),
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
            bitrate: 0,
            quality: 18,
            max_memory_mb: 4096,
            launch_at_startup: true,
            buffer: false,
            ..Config::default()
        };
        edited.audio_sources.push(AudioSource { device: "{0.0.1.00000000}.{mic}".into(), ..AudioSource::new(SourceKind::Mic, 1) });
        edited.audio_track_names[1] = "voice \"chat\"".into();
        edited.audio_sources.push(AudioSource { app: "game.exe".into(), ..AudioSource::new(SourceKind::App, 2) });
        let back: Config = toml::from_str(&edited.render()).unwrap();
        assert_eq!(back, edited);
    }

    #[test]
    fn a_bitrate_says_how_big_a_save_is() {
        let cfg = Config { bitrate: 16, ..Config::default() };
        assert_eq!(cfg.bytes_for(60), Some((2_000_000 + 24_000) * 60), "16 Mbit/s and one sound track");
        assert_eq!(Config { bitrate: 0, ..cfg }.bytes_for(60), None, "constant quality can't tell");
    }

    #[test]
    fn bad_values_are_named() {
        let bad = Config { length: 2, ..Config::default() };
        assert!(bad.check().unwrap_err().contains("length"));
        let bad = Config { save_hotkey: "Alt+Nope".into(), ..Config::default() };
        assert!(bad.check().unwrap_err().contains("save_hotkey"));
    }
}
