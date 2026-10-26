//! `%APPDATA%\afterglow\config.toml`: written with comments on first run and by the settings
//! window, read at start and whenever the settings window saves.

use std::path::PathBuf;

use glint_core::config::CodecName;
use glint_core::geom::rect::Rect;
use glint_core::config::audio::{AudioSource, MAX_TRACKS, SourceKind};
use glint_core::config::hotkeys::parse_hotkey;
use glint_core::naming::{Names, pattern_problem};
use glint_core::paths::app_dir_for;
use serde::Deserialize;

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
    /// File name pattern without the extension, as in glint; see [`Names::expand`].
    pub file_name: String,
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
    /// Where on the recorded monitor notes show.
    pub note_position: Place,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            buffer: true,
            length: 60,
            save_hotkey: "Alt+F10".into(),
            output_dir: String::new(),
            file_name: NAMES.default.into(),
            monitor: 0,
            fps: 60,
            // hevc needs a decoder windows doesn't ship to be played or trimmed: opt in
            codec: CodecName::H264,
            // constant quality at glint's "balanced": the memory cap keeps busy scenes in check
            bitrate: 0,
            quality: 23,
            max_memory_mb: 1024,
            audio_sources: vec![AudioSource { tracks: vec![1], ..AudioSource::new(SourceKind::Desktop, 0) }],
            audio_track_names: vec![String::new(); MAX_TRACKS as usize],
            launch_at_startup: false,
            note_position: Place::TopRight,
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
        if let Some(problem) = pattern_problem(&self.file_name) {
            return Err(format!("file_name: {problem}"));
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

    /// Bytes a second of recording takes on a `w` × `h` monitor refreshing at `hz`: on average and
    /// in a busy stretch. Exact with a bitrate; with constant quality, [`busy_game_mbps`]'s estimate.
    pub fn rate(&self, (w, h): (u32, u32), hz: f64) -> (f64, f64) {
        // the sound: 192 kbit/s per track
        let tracks = self.audio_sources.iter().filter(|s| s.enabled).flat_map(|s| s.tracks.iter()).max().copied();
        let audio = tracks.unwrap_or(0) as f64 * 192_000.0;
        if self.bitrate > 0 {
            let bytes = (self.bitrate as f64 * 1e6 + audio) / 8.0;
            return (bytes, bytes);
        }
        let fps = if self.fps == 0 { hz } else { (self.fps as f64).min(hz) };
        let video = busy_game_mbps(w as f64 * h as f64, fps, self.codec, self.quality) * 1e6;
        ((video + audio) / 8.0, (video * BUSY_STRETCH + audio) / 8.0)
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
            for (key, value) in [("app", &s.app), ("exclude", &s.exclude), ("name", &s.name), ("device", &s.device)] {
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

# Clip names, as in glint: {{date}} {{time}} {{app}} (the app in front when you save) {{title}} and
# {{kind}} are filled in, / makes folders. "{{app}}/{{app}}_{{date}}_{{time}}" sorts clips by game.
file_name = {file_name}

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

# Where notes show on the recorded monitor: top_left, top, top_right, bottom_left, bottom or
# bottom_right.
note_position = "{place}"

# What players call tracks 1-6; an empty name is made from the track's sources.
audio_track_names = [{names}]

# What's heard, as in glint: "desktop" (what your speakers play, minus an `exclude`d app), "app"
# (one app by exe name) or "mic" (a device id, or the default microphone without one); `name` is
# what the app is called, else its exe.
{sources}"#,
            buffer = self.buffer,
            length = self.length,
            hotkey = q(&self.save_hotkey),
            output_dir = q(&self.output_dir),
            file_name = q(&self.file_name),
            monitor = self.monitor,
            fps = self.fps,
            bitrate = self.bitrate,
            quality = self.quality,
            memory = self.max_memory_mb,
            autostart = self.launch_at_startup,
            place = self.note_position.name(),
            names = self.audio_track_names.iter().map(|n| q(n)).collect::<Vec<_>>().join(", "),
        )
    }
}

/// How much busier than its average the stretch a buffer holds can be: the 512 MB cap held 42 s
/// of 3440 × 1440 game footage at qp 18 (about 102 Mbit/s) where the whole clip averaged 78.
const BUSY_STRETCH: f64 = 1.3;

/// About how many Mbit/s busy game footage takes at constant quality `qp`: fitted to h.264
/// recordings of 3440 × 1440 games captured at 144 fps (78 Mbit/s on average at qp 18, about
/// ×0.89 a qp step up), scaled linearly by the pixels and sub-linearly by the frame rate (60 fps
/// costs about 0.85× of 144); hevc takes about 0.6× of h.264.
fn busy_game_mbps(pixels: f64, fps: f64, codec: CodecName, qp: u32) -> f64 {
    let pixels = pixels / (3440.0 * 1440.0);
    let fps = (fps.max(1.0) / 144.0).powf(0.186);
    let qp = 0.89f64.powi(qp as i32 - 18);
    let codec = match codec {
        CodecName::H264 => 1.0,
        CodecName::Hevc => 0.6,
    };
    78.0 * pixels * fps * qp * codec
}

/// A corner, or the middle of the top or bottom edge, of a monitor.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    TopLeft,
    Top,
    TopRight,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Place {
    pub const ALL: [Place; 6] = [Place::TopLeft, Place::Top, Place::TopRight, Place::BottomLeft, Place::Bottom, Place::BottomRight];

    pub fn name(self) -> &'static str {
        match self {
            Place::TopLeft => "top_left",
            Place::Top => "top",
            Place::TopRight => "top_right",
            Place::BottomLeft => "bottom_left",
            Place::Bottom => "bottom",
            Place::BottomRight => "bottom_right",
        }
    }

    /// Where a `w` × `h` box goes in `area`, `margin` in from its edges.
    pub fn at(self, area: Rect, (w, h): (i32, i32), margin: i32) -> Rect {
        let x = match self {
            Place::TopLeft | Place::BottomLeft => area.x + margin,
            Place::Top | Place::Bottom => area.x + (area.w - w) / 2,
            Place::TopRight | Place::BottomRight => area.right() - w - margin,
        };
        let y = match self {
            Place::TopLeft | Place::Top | Place::TopRight => area.y + margin,
            _ => area.bottom() - h - margin,
        };
        Rect { x, y, w, h }
    }
}

/// afterglow's file naming: `afterglow_<date>_<time>` until a pattern is set.
pub const NAMES: Names = glint_core::naming::AFTERGLOW;

pub fn default_output_dir() -> PathBuf {
    glint_shell::explorer::videos().unwrap_or_else(|| app_dir_for(APP)).join(APP)
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
            file_name: "{app}/{app} {title}_{date}".into(),
            monitor: 2,
            fps: 0,
            codec: CodecName::Hevc,
            bitrate: 0,
            quality: 18,
            max_memory_mb: 4096,
            launch_at_startup: true,
            note_position: Place::Bottom,
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
    fn a_bitrate_says_exactly_how_big_a_save_is() {
        let cfg = Config { bitrate: 16, ..Config::default() };
        let per_sec = (16_000_000.0 + 192_000.0) / 8.0;
        assert_eq!(cfg.rate((1920, 1080), 60.0), (per_sec, per_sec), "16 Mbit/s and one sound track");
    }

    #[test]
    fn constant_quality_follows_the_measurements() {
        let near = |a: f64, b: f64| (a / b - 1.0).abs() < 0.05;
        let mbps = |qp, fps, codec| busy_game_mbps(3440.0 * 1440.0, fps, codec, qp);
        assert!(near(mbps(18, 144.0, CodecName::H264), 78.0));
        for (qp, measured) in [(20, 80.0 / 105.0), (21, 0.73), (22, 0.61), (24, 0.49)] {
            assert!(near(mbps(qp, 144.0, CodecName::H264) / 78.0, measured), "qp {qp}");
        }
        assert!(near(mbps(18, 60.0, CodecName::H264) / 78.0, 0.85), "60 fps");
        assert!(near(mbps(18, 144.0, CodecName::Hevc) / 78.0, 0.6), "hevc");
        assert!(near(busy_game_mbps(1720.0 * 1440.0, 144.0, CodecName::H264, 18), 39.0), "half the pixels");

        let cfg = Config { bitrate: 0, quality: 18, fps: 0, audio_sources: vec![], ..Config::default() };
        let (avg, busy) = cfg.rate((3440, 1440), 144.0);
        assert!(near(avg, 78e6 / 8.0) && busy > avg);
        assert!(near(512.0 * 1048576.0 / busy, 42.0), "the 512 MB cap held 42 s in a busy stretch");
        assert_eq!(Config { fps: 240, ..cfg.clone() }.rate((3440, 1440), 144.0), (avg, busy), "frames come at most at the refresh rate");
    }

    #[test]
    fn notes_sit_in_their_corner_or_middle() {
        let area = Rect { x: -1920, y: 0, w: 1920, h: 1040 };
        let at = |p: Place| p.at(area, (200, 50), 24);
        assert_eq!(at(Place::TopLeft), Rect { x: -1896, y: 24, w: 200, h: 50 });
        assert_eq!(at(Place::Top), Rect { x: -1060, y: 24, w: 200, h: 50 });
        assert_eq!(at(Place::TopRight), Rect { x: -224, y: 24, w: 200, h: 50 });
        assert_eq!(at(Place::Bottom), Rect { x: -1060, y: 966, w: 200, h: 50 });
        assert_eq!(at(Place::BottomRight), Rect { x: -224, y: 966, w: 200, h: 50 });
        for p in Place::ALL {
            let r = at(p);
            assert!(r.x >= area.x && r.right() <= area.right() && r.y >= area.y && r.bottom() <= area.bottom(), "{p:?}");
        }
    }

    #[test]
    fn bad_values_are_named() {
        let bad = Config { length: 2, ..Config::default() };
        assert!(bad.check().unwrap_err().contains("length"));
        let bad = Config { save_hotkey: "Alt+Nope".into(), ..Config::default() };
        assert!(bad.check().unwrap_err().contains("save_hotkey"));
        let bad = Config { file_name: "clips/../a:b".into(), ..Config::default() };
        assert!(bad.check().unwrap_err().contains("file_name"));
    }
}
