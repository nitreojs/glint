use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::naming::{GLINT, pattern_problem};
use crate::paths::app_dir;
use crate::record::Codec;
use crate::record::keys::Position;

pub mod audio;
pub mod color;
pub mod hotkeys;

use audio::AudioSource;
use hotkeys::parse_hotkey;

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    pub screenshot_hotkey: String,
    pub fullscreen_hotkey: String,
    pub monitor_hotkey: String,
    pub pause_hotkey: String,
    pub launch_at_startup: bool,
    pub freeze_screen: bool,
    pub ui_elements: UiElements,
    pub notify_success: bool,
    pub output_dir: String,
    /// File name pattern without the extension; see [`crate::naming::expand`].
    pub file_name: String,
    pub fps: u32,
    pub codec: CodecName,
    pub quality: u32,
    pub effort: u32,
    pub draw_cursor: bool,
    pub click_highlights: bool,
    pub clicks: ClickMode,
    pub keystrokes: Keystrokes,
    pub keystroke_position: Position,
    pub keystrokes_mode: ClickMode,
    pub audio_sources: Vec<AudioSource>,
    pub audio_track_names: Vec<String>,
    pub countdown: u32,
    pub indicator_inside: IndicatorInside,
    pub open_trimmer: bool,
    pub screenshot_style: ShotStyle,
    pub screenshot_format: ShotFormat,
    /// Screenshots keep the mouse cursor (the editor can move or delete it).
    pub screenshot_cursor: bool,
    /// JPG quality, 50-100.
    pub screenshot_quality: u32,
    pub screenshot_copy: bool,
    pub screenshot_save: bool,
    pub screenshot_upload: bool,
    pub upload_destination: String,
    pub litterbox_expiry: Expiry,
    pub upload_copy_link: bool,
    pub recording_upload: bool,
    pub upload_proxy: String,
    /// `#RRGGBB`, see [`parse_hex`].
    pub accent: String,
    pub accent_custom: Option<String>,
    pub update_mode: UpdateMode,
    pub update_beta: bool,
}

pub const DEFAULT_ACCENT: &str = "#E5402F";
/// Characters Windows doesn't allow in file names.
pub(crate) const BAD_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

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

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShotStyle {
    Select,
    Sharex,
}

/// When hovering picks UI elements inside windows instead of whole windows.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UiElements {
    Shift,
    Always,
    Off,
}

impl UiElements {
    pub fn used(self, shift: bool) -> bool {
        match self {
            UiElements::Shift => shift,
            UiElements::Always => true,
            UiElements::Off => false,
        }
    }
}

/// The recording frame and pill where they would overlap the recorded area.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndicatorInside {
    /// Left out while recording.
    Hide,
    /// Shown but excluded from capture, which pauses NVIDIA Instant Replay and similar apps.
    Show,
}

/// What happens to click highlights.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClickMode {
    /// The video stays clean; the clicks are saved in the file for the trimmer.
    Editable,
    /// Drawn into the video while recording.
    BurnIn,
}

/// How much of the keyboard the keystroke overlay records.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Keystrokes {
    /// No overlay, no keyboard hook.
    Off,
    /// Only chords (Ctrl/Alt/Win) and standalone special keys; never typed text.
    Shortcuts,
    /// Also printable text, skipping password fields.
    All,
}

/// What glint does about new releases on GitHub.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateMode {
    /// Downloads in the background, installs when glint restarts.
    Auto,
    /// Only tells you; clicking the notification installs.
    Notify,
    Off,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "12h")]
    HalfDay,
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "72h")]
    ThreeDays,
}

impl Expiry {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Expiry::Hour => "1h",
            Expiry::HalfDay => "12h",
            Expiry::Day => "24h",
            Expiry::ThreeDays => "72h",
        }
    }
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
            fullscreen_hotkey: "PrintScreen".into(),
            monitor_hotkey: "Alt+PrintScreen".into(),
            pause_hotkey: "Pause".into(),
            launch_at_startup: false,
            freeze_screen: true,
            ui_elements: UiElements::Shift,
            notify_success: true,
            output_dir: String::new(),
            file_name: GLINT.default.into(),
            fps: 0,
            codec: CodecName::H264,
            quality: 22,
            effort: 50,
            draw_cursor: true,
            click_highlights: false,
            clicks: ClickMode::Editable,
            keystrokes: Keystrokes::Off,
            keystroke_position: Position::Centre,
            keystrokes_mode: ClickMode::Editable,
            audio_sources: audio::default_sources(),
            audio_track_names: vec![String::new(); audio::MAX_TRACKS as usize],
            countdown: 0,
            indicator_inside: IndicatorInside::Hide,
            open_trimmer: false,
            screenshot_style: ShotStyle::Select,
            screenshot_format: ShotFormat::Png,
            screenshot_cursor: false,
            screenshot_quality: 90,
            screenshot_copy: true,
            screenshot_save: true,
            screenshot_upload: false,
            upload_destination: "catbox".into(),
            litterbox_expiry: Expiry::Day,
            upload_copy_link: true,
            recording_upload: false,
            upload_proxy: "system".into(),
            accent: DEFAULT_ACCENT.into(),
            accent_custom: None,
            update_mode: UpdateMode::Auto,
            update_beta: false,
        }
    }
}

impl Config {
    pub(crate) fn path() -> PathBuf {
        app_dir().join("config.toml")
    }

    fn parse(text: &str) -> Result<Config, toml::de::Error> {
        let mut table: toml::Table = toml::from_str(text)?;
        let has_monitor_hotkey = table.contains_key("monitor_hotkey");
        let mut cfg: Config = if audio::is_legacy(&table) {
            audio::migrate(&mut table);
            toml::Value::Table(table).try_into()?
        } else {
            toml::from_str(text)?
        };
        if !has_monitor_hotkey {
            cfg.monitor_hotkey = cfg.free_monitor_hotkey();
        }
        Ok(cfg)
    }

    /// The monitor hotkey for settings from before it existed: the default, unless another
    /// hotkey already uses that combo.
    fn free_monitor_hotkey(&self) -> String {
        let taken: Vec<_> = [&self.hotkey, &self.screenshot_hotkey, &self.fullscreen_hotkey, &self.pause_hotkey]
            .into_iter()
            .filter_map(|h| parse_hotkey(h).ok())
            .collect();
        ["Alt+PrintScreen", "Shift+PrintScreen", "Ctrl+Shift+PrintScreen", "Ctrl+Alt+Shift+PrintScreen"]
            .into_iter()
            .find(|c| parse_hotkey(c).is_ok_and(|c| !taken.contains(&c)))
            .unwrap_or("Ctrl+Alt+Shift+F12")
            .to_string()
    }

    /// Loads the settings file, creating it with defaults on first run.
    pub fn load() -> Result<Config, String> {
        let path = Self::path();
        if !path.exists() {
            Config::default().save()?;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let cfg = Config::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.check_hotkeys()?;
        if cfg.quality > 51 {
            return Err(format!("quality must be 0-51, got {}", cfg.quality));
        }
        if cfg.countdown > 10 {
            return Err(format!("countdown must be 0-10 seconds, got {}", cfg.countdown));
        }
        if !(50..=100).contains(&cfg.screenshot_quality) {
            return Err(format!("screenshot_quality must be 50-100, got {}", cfg.screenshot_quality));
        }
        if let Some(problem) = pattern_problem(&cfg.file_name) {
            return Err(format!("file_name {}: {problem}", toml::Value::String(cfg.file_name.clone())));
        }
        if parse_hex(&cfg.accent).is_none() {
            return Err(format!("accent must be a colour like \"#E5402F\", got {}", toml::Value::String(cfg.accent.clone())));
        }
        for (i, s) in cfg.audio_sources.iter().enumerate() {
            if let Some(problem) = s.problem() {
                return Err(format!("audio source {}: {problem}", i + 1));
            }
        }
        if cfg.audio_track_names.len() > audio::MAX_TRACKS as usize {
            return Err(format!("audio_track_names has {} names, at most {} fit", cfg.audio_track_names.len(), audio::MAX_TRACKS));
        }
        Ok(cfg)
    }

    /// Every hotkey parses, and no two are the same combo.
    fn check_hotkeys(&self) -> Result<(), String> {
        let hotkeys = [
            ("hotkey", &self.hotkey),
            ("screenshot_hotkey", &self.screenshot_hotkey),
            ("fullscreen_hotkey", &self.fullscreen_hotkey),
            ("monitor_hotkey", &self.monitor_hotkey),
            ("pause_hotkey", &self.pause_hotkey),
        ];
        let mut combos = Vec::with_capacity(hotkeys.len());
        for (name, combo) in hotkeys {
            let parsed = parse_hotkey(combo).map_err(|e| format!("{name}: {e}"))?;
            if let Some((other, _)) = combos.iter().find(|(_, c)| *c == parsed) {
                return Err(format!("{other} and {name} must differ"));
            }
            combos.push((name, parsed));
        }
        Ok(())
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
        let accent_custom = match &self.accent_custom {
            Some(c) => format!("accent_custom = {}", q(c)),
            None => "# accent_custom = \"#RRGGBB\"".into(),
        };
        let codec = match self.codec {
            CodecName::H264 => "h264",
            CodecName::Hevc => "hevc",
        };
        let format = match self.screenshot_format {
            ShotFormat::Png => "png",
            ShotFormat::Jpg => "jpg",
        };
        let style = match self.screenshot_style {
            ShotStyle::Select => "select",
            ShotStyle::Sharex => "sharex",
        };
        let ui_elements = match self.ui_elements {
            UiElements::Shift => "shift",
            UiElements::Always => "always",
            UiElements::Off => "off",
        };
        let indicator_inside = match self.indicator_inside {
            IndicatorInside::Hide => "hide",
            IndicatorInside::Show => "show",
        };
        let clicks = match self.clicks {
            ClickMode::Editable => "editable",
            ClickMode::BurnIn => "burn-in",
        };
        let keystrokes = match self.keystrokes {
            Keystrokes::Off => "off",
            Keystrokes::Shortcuts => "shortcuts",
            Keystrokes::All => "all",
        };
        let keystroke_position = match self.keystroke_position {
            Position::Left => "left",
            Position::Centre => "centre",
            Position::Right => "right",
        };
        let keystrokes_mode = match self.keystrokes_mode {
            ClickMode::Editable => "editable",
            ClickMode::BurnIn => "burn-in",
        };
        let update_mode = match self.update_mode {
            UpdateMode::Auto => "auto",
            UpdateMode::Notify => "notify",
            UpdateMode::Off => "off",
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

# Screenshots every monitor at once, straight to the clipboard / folder, without selecting.
fullscreen_hotkey = {fullscreen_hotkey}

# Screenshots the monitor under the mouse, straight to the clipboard / folder, without selecting.
monitor_hotkey = {monitor_hotkey}

# Pauses and resumes a running recording (only held while recording).
pause_hotkey = {pause_hotkey}

# Start glint (in the tray) when you sign in to Windows.
launch_at_startup = {launch_at_startup}

# While selecting a region, show a still image of the screen (true) or the live screen,
# dimmed (false).
freeze_screen = {freeze_screen}

# While selecting, highlight buttons, panels and other UI elements inside windows instead of
# whole windows (the mouse wheel steps out to bigger parts): "shift" while Shift is held,
# "always", or "off".
ui_elements = "{ui_elements}"

# Tray notifications after a screenshot or recording is saved (true), or only when
# something goes wrong (false).
notify_success = {notify_success}

# Folder for recordings and screenshots. Empty = <Documents>\glint
output_dir = {output_dir}

# File names, without the extension. {{date}} becomes 2026-10-20 and {{time}} 14-32-07
# (local time), {{app}} the captured window's app ("screen" for whole monitors), {{title}} its
# title, {{kind}} "screenshots" or "recordings"; a / makes folders, e.g. "{{app}}/{{app}}_{{date}}_{{time}}"
# (captures that aren't one app's window go to "other"). A number is appended if the file
# already exists.
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

# A ring ripples out from every mouse click in recordings (only mouse buttons are watched,
# never the keyboard). clicks: "editable" keeps the video clean and saves the clicks in the
# file, so the trimmer can move or remove them; "burn-in" draws them into the video while recording.
click_highlights = {click_highlights}
clicks = "{clicks}"

# On-screen keystroke overlay for recordings, shown only while recording your own screen.
# keystrokes: "off"; "shortcuts" shows only chords (Ctrl/Alt/Win) and special keys, never
# typed text; "all" also shows printable text and skips password fields. keystroke_position:
# "left", "centre" or "right" along the bottom. keystrokes_mode: "editable" saves them in the
# file for the trimmer; "burn-in" draws them into the video while recording.
keystrokes = "{keystrokes}"
keystroke_position = "{keystroke_position}"
keystrokes_mode = "{keystrokes_mode}"

# Sound to record. Each source has a kind: "desktop" (what the default output device plays;
# exclude = "name.exe" leaves one app out of it), "app" (app = "name.exe": only that app and
# the processes it starts), or "mic" (device = an endpoint id, or none for the default
# microphone). volume is 0-200 (%); tracks lists which of the video's audio tracks (1-6) it
# goes to, and a track that gets several sources holds their mix. An app on a track that also
# has a desktop source (not excluding it) isn't added again: desktop already holds it. name is
# what the app (or the app a desktop leaves out) is called; without one it's named after its exe.
{audio_sources}
# Names players show for tracks 1-6; "" names a track after its sources ("mix", "desktop", ...).
{audio_track_names}

# Seconds to count down after selecting a region before recording starts (0 = none).
countdown = {countdown}

# The recording frame and pill where they'd overlap the recorded area (e.g. a whole monitor
# on a single-monitor setup): "hide" leaves them out (the tray icon and hotkeys still control
# the recording), "show" keeps them out of the video, but NVIDIA Instant Replay and some
# capture apps pause while they're visible.
indicator_inside = "{indicator_inside}"

# Open the trimmer (trim, cut, crop, mute tracks, save a smaller video or a gif) after every recording is saved.
open_trimmer = {open_trimmer}

# Screenshots: "select" picks a region first, then opens the editor; "sharex" opens the
# editor on the whole frozen screen first (pick a region with Q). Recordings always select first.
screenshot_style = "{style}"

# Screenshots keep the mouse cursor: the editor shows it as a mark you can move, resize or
# delete; all-screens and monitor shots have it drawn in.
screenshot_cursor = {screenshot_cursor}

# Screenshots: "png" (lossless) or "jpg" (smaller; screenshot_quality 50-100 applies).
screenshot_format = "{format}"
screenshot_quality = {screenshot_quality}
# What happens when a screenshot is finished: copy it, save it to the folder, upload it
# (any combination).
screenshot_copy = {screenshot_copy}
screenshot_save = {screenshot_save}
screenshot_upload = {screenshot_upload}

# Where uploads go: "catbox" (permanent, 200 MB), "litterbox" (temporary, 1 GB), "uguu"
# (temporary, 128 MB), "pomf" (pomf.lain.la, 1 GB), or the name of a custom uploader
# imported in settings (a ShareX .sxcu file in %APPDATA%\glint\uploaders).
upload_destination = {upload_destination}
# How long litterbox keeps files: "1h", "12h", "24h" or "72h".
litterbox_expiry = "{expiry}"
# Copy the link to the clipboard when an upload finishes.
upload_copy_link = {upload_copy_link}
# Upload every recording after it's saved.
recording_upload = {recording_upload}
# Proxy for uploads: "system" (Windows' proxy settings, the ones browsers use), "none", or
# "http://host:port". SOCKS proxies can't be used: Windows' HTTP stack only speaks HTTP proxies.
upload_proxy = {upload_proxy}

# Accent colour of the tray icon and the settings / trimmer windows: a #RRGGBB hex colour.
accent = {accent}
# The colour last picked with the custom swatch, remembered while a preset is in use.
{accent_custom}

# Updates from github.com/nitreojs/glint, checked at start and every 6 hours, signature-checked
# before they're used: "auto" downloads them in the background and installs on the next start
# (or when you click the notification), "notify" only tells you, "off" never checks.
update_mode = "{update_mode}"
# Also take beta versions (GitHub pre-releases).
update_beta = {update_beta}
"#,
            hotkey = q(&self.hotkey),
            screenshot_hotkey = q(&self.screenshot_hotkey),
            fullscreen_hotkey = q(&self.fullscreen_hotkey),
            monitor_hotkey = q(&self.monitor_hotkey),
            pause_hotkey = q(&self.pause_hotkey),
            launch_at_startup = self.launch_at_startup,
            freeze_screen = self.freeze_screen,
            notify_success = self.notify_success,
            output_dir = q(&self.output_dir),
            file_name = q(&self.file_name),
            fps = self.fps,
            quality = self.quality,
            effort = self.effort,
            draw_cursor = self.draw_cursor,
            click_highlights = self.click_highlights,
            keystrokes = keystrokes,
            keystroke_position = keystroke_position,
            keystrokes_mode = keystrokes_mode,
            audio_sources = audio::render_sources(&self.audio_sources),
            audio_track_names = audio::render_track_names(&self.audio_track_names),
            countdown = self.countdown,
            open_trimmer = self.open_trimmer,
            screenshot_quality = self.screenshot_quality,
            screenshot_cursor = self.screenshot_cursor,
            screenshot_copy = self.screenshot_copy,
            screenshot_save = self.screenshot_save,
            screenshot_upload = self.screenshot_upload,
            upload_destination = q(&self.upload_destination),
            expiry = self.litterbox_expiry.name(),
            upload_copy_link = self.upload_copy_link,
            recording_upload = self.recording_upload,
            upload_proxy = q(&self.upload_proxy),
            accent = q(&self.accent),
            update_beta = self.update_beta,
        )
    }

    pub fn output_dir_or(&self, documents: impl FnOnce() -> PathBuf) -> PathBuf {
        if self.output_dir.trim().is_empty() {
            documents().join("glint")
        } else {
            PathBuf::from(self.output_dir.trim())
        }
    }

    /// The accent as RGB; the default red if the setting isn't a valid colour.
    pub fn accent_rgb(&self) -> [u8; 3] {
        parse_hex(&self.accent).or_else(|| parse_hex(DEFAULT_ACCENT)).unwrap_or_default()
    }

    pub fn accent_custom_rgb(&self) -> Option<[u8; 3]> {
        self.accent_custom.as_deref().and_then(parse_hex)
    }
}

/// Up to 0.1 the default folder was `<Videos>\glint`. On the first start after that changed,
/// settings that use the default while `old` (that folder) holds files of its own keep it:
/// returns the folder to write into `output_dir`, so nothing seems to move.
pub fn pinned_output_dir(output_dir: &str, old: &Path, old_files: &[&str]) -> Option<String> {
    let captures = old_files.iter().any(|f| !f.eq_ignore_ascii_case("desktop.ini") && !f.eq_ignore_ascii_case("thumbs.db"));
    (output_dir.trim().is_empty() && captures).then(|| old.display().to_string())
}

/// `#RRGGBB` or `#RGB`, the `#` optional, surrounding spaces ignored.
pub fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let hex = text.trim();
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    match hex.len() {
        6 => Some([(v >> 16) as u8, (v >> 8) as u8, v as u8]),
        3 => Some([(v >> 8) as u8 * 17, (v >> 4 & 15) as u8 * 17, (v & 15) as u8 * 17]),
        _ => None,
    }
}

/// The canonical `#RRGGBB` spelling.
pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_file_parses_back_to_the_same_settings() {
        let cfg = Config {
            hotkey: "Shift+Pause".into(),
            screenshot_hotkey: "Win+Shift+S".into(),
            fullscreen_hotkey: "Alt+F12".into(),
            monitor_hotkey: "Shift+F12".into(),
            pause_hotkey: "Ctrl+Pause".into(),
            launch_at_startup: true,
            freeze_screen: false,
            ui_elements: UiElements::Always,
            notify_success: false,
            output_dir: r#"D:\clips\"quoted" dir\"#.into(),
            file_name: "{app}/clip {date} at {time}".into(),
            fps: 60,
            codec: CodecName::Hevc,
            quality: 18,
            effort: 75,
            draw_cursor: false,
            click_highlights: true,
            clicks: ClickMode::BurnIn,
            keystrokes: Keystrokes::All,
            keystroke_position: Position::Right,
            keystrokes_mode: ClickMode::BurnIn,
            audio_sources: vec![
                AudioSource { exclude: "game.exe".into(), ..AudioSource::new(audio::SourceKind::Desktop, 0) },
                AudioSource { app: "discord.exe".into(), name: "Discord".into(), volume: 80, ..AudioSource::new(audio::SourceKind::App, 1) },
                AudioSource { enabled: false, tracks: vec![4], ..AudioSource::new(audio::SourceKind::Mic, 2) },
            ],
            audio_track_names: ["all", "", "", "voice", "", ""].map(String::from).to_vec(),
            countdown: 5,
            indicator_inside: IndicatorInside::Show,
            open_trimmer: true,
            screenshot_style: ShotStyle::Sharex,
            screenshot_format: ShotFormat::Jpg,
            screenshot_cursor: true,
            screenshot_quality: 75,
            screenshot_copy: false,
            screenshot_save: true,
            screenshot_upload: true,
            upload_destination: "my \"host\".sxcu".into(),
            litterbox_expiry: Expiry::Hour,
            upload_copy_link: false,
            recording_upload: true,
            upload_proxy: "http://127.0.0.1:10809".into(),
            accent: "#2F7BE5".into(),
            accent_custom: Some("#7FE0C0".into()),
            update_mode: UpdateMode::Notify,
            update_beta: true,
        };
        let back: Config = toml::from_str(&cfg.render()).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(toml::from_str::<Config>(&Config::default().render()).unwrap(), Config::default());
    }

    #[test]
    fn hotkeys_must_all_differ() {
        assert_eq!(Config::default().check_hotkeys(), Ok(()));
        assert_eq!(Config::default().monitor_hotkey, "Alt+PrintScreen");
        let clash = Config { monitor_hotkey: "printscreen".into(), ..Config::default() };
        assert_eq!(clash.check_hotkeys(), Err("fullscreen_hotkey and monitor_hotkey must differ".into()));
        let clash = Config { pause_hotkey: "alt+prtsc".into(), ..Config::default() };
        assert_eq!(clash.check_hotkeys(), Err("monitor_hotkey and pause_hotkey must differ".into()));
        let bad = Config { monitor_hotkey: "Alt".into(), ..Config::default() };
        assert!(bad.check_hotkeys().unwrap_err().starts_with("monitor_hotkey: "));
        let old = Config::parse("hotkey = \"Ctrl+F9\"").unwrap();
        assert_eq!(old.monitor_hotkey, "Alt+PrintScreen");
        let old = Config::parse("fullscreen_hotkey = \"Alt+PrtSc\"\nhotkey = \"Shift+PrintScreen\"").unwrap();
        assert_eq!((old.monitor_hotkey.as_str(), old.check_hotkeys()), ("Ctrl+Shift+PrintScreen", Ok(())));
        let chosen = Config::parse("fullscreen_hotkey = \"Alt+PrtSc\"\nmonitor_hotkey = \"Alt+PrintScreen\"").unwrap();
        assert!(chosen.check_hotkeys().is_err());
    }

    #[test]
    fn the_old_default_folder_is_kept_only_when_it_has_captures() {
        let old = Path::new(r"C:\Users\a\Videos\glint");
        let pinned = pinned_output_dir("", old, &["glint_2026-10-20_14-32-07.mp4", "desktop.ini"]);
        assert_eq!(pinned.as_deref(), Some(r"C:\Users\a\Videos\glint"));
        assert_eq!(pinned_output_dir("  ", old, &["a.png"]).as_deref(), Some(r"C:\Users\a\Videos\glint"));
        assert_eq!(pinned_output_dir("", old, &[]), None);
        assert_eq!(pinned_output_dir("", old, &["desktop.ini", "Thumbs.db"]), None);
        assert_eq!(pinned_output_dir(r"D:\clips", old, &["a.png"]), None);
        let documents = Config::default().output_dir_or(|| PathBuf::from("docs"));
        assert_eq!(documents, Path::new("docs").join("glint"));
    }

    #[test]
    fn old_audio_keys_still_load() {
        let old = Config::parse("fps = 60\naudio_desktop = true\naudio_mic = true\ncountdown = 3").unwrap();
        assert_eq!((old.fps, old.countdown), (60, 3));
        let kinds: Vec<_> = old.audio_sources.iter().map(|s| (s.kind, s.enabled)).collect();
        assert_eq!(kinds, [(audio::SourceKind::Desktop, true), (audio::SourceKind::Mic, true)]);
        let off = Config::parse("audio_desktop = false").unwrap();
        assert_eq!(off.audio_sources, Config::default().audio_sources);
        assert!(Config::parse("audio_desktop = true\nnope = 1").is_err());
        assert_eq!(Config::parse(&Config::default().render()).unwrap(), Config::default());
    }

    #[test]
    fn ui_elements_follow_the_mode_and_shift() {
        let used = |mode: UiElements| [mode.used(false), mode.used(true)];
        assert_eq!(used(UiElements::Shift), [false, true]);
        assert_eq!(used(UiElements::Always), [true, true]);
        assert_eq!(used(UiElements::Off), [false, false]);
        let parsed = |v: &str| toml::from_str::<Config>(&format!("ui_elements = \"{v}\"")).map(|c| c.ui_elements);
        assert_eq!(parsed("always").unwrap(), UiElements::Always);
        assert_eq!(parsed("off").unwrap(), UiElements::Off);
        assert!(parsed("ctrl").is_err());
        assert_eq!(Config::default().ui_elements, UiElements::Shift);
    }

    #[test]
    fn indicator_inside_round_trips() {
        for inside in [IndicatorInside::Hide, IndicatorInside::Show] {
            let cfg = Config { indicator_inside: inside, ..Config::default() };
            assert_eq!(toml::from_str::<Config>(&cfg.render()).unwrap().indicator_inside, inside);
        }
        let parsed = |v: &str| toml::from_str::<Config>(&format!("indicator_inside = \"{v}\"")).map(|c| c.indicator_inside);
        assert_eq!(parsed("show").unwrap(), IndicatorInside::Show);
        assert!(parsed("excluded").is_err());
        assert_eq!(Config::default().indicator_inside, IndicatorInside::Hide);
    }

    #[test]
    fn update_settings_round_trip_with_their_comments() {
        for mode in [UpdateMode::Auto, UpdateMode::Notify, UpdateMode::Off] {
            for beta in [false, true] {
                let cfg = Config { update_mode: mode, update_beta: beta, ..Config::default() };
                let back = Config::parse(&cfg.render()).unwrap();
                assert_eq!((back.update_mode, back.update_beta), (mode, beta));
            }
        }
        let text = Config::default().render();
        assert!(text.contains("\n# (or when you click the notification), \"notify\" only tells you, \"off\" never checks.\nupdate_mode = \"auto\"\n"));
        assert!(text.contains("\n# Also take beta versions (GitHub pre-releases).\nupdate_beta = false\n"));
        let parsed = |v: &str| Config::parse(&format!("update_mode = \"{v}\"")).map(|c| c.update_mode);
        assert_eq!(parsed("notify").unwrap(), UpdateMode::Notify);
        assert!(parsed("manual").is_err());
        assert_eq!((Config::default().update_mode, Config::default().update_beta), (UpdateMode::Auto, false));
    }

    #[test]
    fn accent_colours_parse_and_are_validated() {
        assert_eq!(parse_hex("#E5402F"), Some([0xE5, 0x40, 0x2F]));
        assert_eq!(parse_hex(" e5402f "), Some([0xE5, 0x40, 0x2F]));
        assert_eq!(parse_hex("#f0a"), Some([0xFF, 0x00, 0xAA]));
        for bad in ["", "#", "#E5402", "#E5402F0", "#GG0000", "E5 40 2F", "#+5402F", "#ё00"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        assert_eq!(hex([0xE5, 0x40, 0x2F]), DEFAULT_ACCENT);
        assert_eq!(hex(parse_hex("#abc").unwrap()), "#AABBCC");
        assert_eq!(Config { accent: "nope".into(), ..Config::default() }.accent_rgb(), [0xE5, 0x40, 0x2F]);
        let parsed = |v: &str| toml::from_str::<Config>(&format!("accent = \"{v}\"")).map(|c| c.accent_rgb());
        assert_eq!(parsed("#1F9E8F").unwrap(), [0x1F, 0x9E, 0x8F]);
    }

    #[test]
    fn click_settings_round_trip_with_their_comment() {
        for on in [false, true] {
            for mode in [ClickMode::Editable, ClickMode::BurnIn] {
                let cfg = Config { click_highlights: on, clicks: mode, ..Config::default() };
                let back = Config::parse(&cfg.render()).unwrap();
                assert_eq!((back.click_highlights, back.clicks), (on, mode));
            }
        }
        let text = Config::default().render();
        assert!(text.contains("while recording.\nclick_highlights = false\nclicks = \"editable\"\n"), "{text}");
        let parsed = |v: &str| Config::parse(&format!("clicks = \"{v}\"")).map(|c| c.clicks);
        assert_eq!(parsed("burn-in").unwrap(), ClickMode::BurnIn);
        assert!(parsed("burn_in").is_err());
        assert_eq!((Config::default().click_highlights, Config::default().clicks), (false, ClickMode::Editable));
    }
}
