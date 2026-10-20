use std::path::PathBuf;

use serde::Deserialize;

use crate::platform::format::date_time;
use crate::platform::paths::{app_dir, videos_dir};
use crate::record::Codec;

pub(crate) mod hotkeys;

use hotkeys::parse_hotkey;

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub hotkey: String,
    pub screenshot_hotkey: String,
    pub fullscreen_hotkey: String,
    pub pause_hotkey: String,
    pub launch_at_startup: bool,
    pub freeze_screen: bool,
    pub ui_elements: UiElements,
    pub notify_success: bool,
    pub output_dir: String,
    /// File name pattern without the extension; see [`Config::file_stem`].
    pub file_name: String,
    pub fps: u32,
    pub codec: CodecName,
    pub quality: u32,
    pub effort: u32,
    pub draw_cursor: bool,
    pub audio_desktop: bool,
    pub audio_mic: bool,
    pub countdown: u32,
    pub open_trimmer: bool,
    pub screenshot_style: ShotStyle,
    pub screenshot_format: ShotFormat,
    /// JPG quality, 50-100.
    pub screenshot_quality: u32,
    pub screenshot_copy: bool,
    pub screenshot_save: bool,
    pub screenshot_upload: bool,
    pub upload_destination: String,
    pub litterbox_expiry: Expiry,
    pub upload_copy_link: bool,
    pub recording_upload: bool,
}

pub(crate) const DEFAULT_FILE_NAME: &str = "glint_{date}_{time}";
/// Characters Windows doesn't allow in file names.
pub(crate) const BAD_NAME_CHARS: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CodecName {
    H264,
    Hevc,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ShotFormat {
    Png,
    Jpg,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ShotStyle {
    Select,
    Sharex,
}

/// When hovering picks UI elements inside windows instead of whole windows.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum UiElements {
    Shift,
    Always,
    Off,
}

impl UiElements {
    pub(crate) fn used(self, shift: bool) -> bool {
        match self {
            UiElements::Shift => shift,
            UiElements::Always => true,
            UiElements::Off => false,
        }
    }
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Expiry {
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
    pub(crate) fn codec(self) -> Codec {
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
            pause_hotkey: "Pause".into(),
            launch_at_startup: false,
            freeze_screen: true,
            ui_elements: UiElements::Shift,
            notify_success: true,
            output_dir: String::new(),
            file_name: DEFAULT_FILE_NAME.into(),
            fps: 0,
            codec: CodecName::H264,
            quality: 22,
            effort: 50,
            draw_cursor: true,
            audio_desktop: false,
            audio_mic: false,
            countdown: 0,
            open_trimmer: false,
            screenshot_style: ShotStyle::Select,
            screenshot_format: ShotFormat::Png,
            screenshot_quality: 90,
            screenshot_copy: true,
            screenshot_save: true,
            screenshot_upload: false,
            upload_destination: "catbox".into(),
            litterbox_expiry: Expiry::Day,
            upload_copy_link: true,
            recording_upload: false,
        }
    }
}

impl Config {
    pub(crate) fn path() -> PathBuf {
        app_dir().join("config.toml")
    }

    /// Loads the settings file, creating it with defaults on first run.
    pub(crate) fn load() -> Result<Config, String> {
        let path = Self::path();
        if !path.exists() {
            Config::default().save()?;
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let hotkeys = [
            ("hotkey", &cfg.hotkey),
            ("screenshot_hotkey", &cfg.screenshot_hotkey),
            ("fullscreen_hotkey", &cfg.fullscreen_hotkey),
            ("pause_hotkey", &cfg.pause_hotkey),
        ];
        let mut combos = Vec::with_capacity(hotkeys.len());
        for (name, combo) in hotkeys {
            let parsed = parse_hotkey(combo).map_err(|e| format!("{name}: {e}"))?;
            if let Some((other, _)) = combos.iter().find(|(_, c)| *c == parsed) {
                return Err(format!("{other} and {name} must differ"));
            }
            combos.push((name, parsed));
        }
        if cfg.quality > 51 {
            return Err(format!("quality must be 0-51, got {}", cfg.quality));
        }
        if cfg.countdown > 10 {
            return Err(format!("countdown must be 0-10 seconds, got {}", cfg.countdown));
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
    pub(crate) fn save(&self) -> Result<(), String> {
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
        let style = match self.screenshot_style {
            ShotStyle::Select => "select",
            ShotStyle::Sharex => "sharex",
        };
        let ui_elements = match self.ui_elements {
            UiElements::Shift => "shift",
            UiElements::Always => "always",
            UiElements::Off => "off",
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

# Record the sound playing on the default output device, and/or the default microphone.
# Both on = mixed into one track.
audio_desktop = {audio_desktop}
audio_mic = {audio_mic}

# Seconds to count down after selecting a region before recording starts (0 = none).
countdown = {countdown}

# Open the trimmer (trim, export a gif) after every recording is saved.
open_trimmer = {open_trimmer}

# Screenshots: "select" picks a region first, then opens the editor; "sharex" opens the
# editor on the whole frozen screen first (pick a region with Q). Recordings always select first.
screenshot_style = "{style}"

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
"#,
            hotkey = q(&self.hotkey),
            screenshot_hotkey = q(&self.screenshot_hotkey),
            fullscreen_hotkey = q(&self.fullscreen_hotkey),
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
            audio_desktop = self.audio_desktop,
            audio_mic = self.audio_mic,
            countdown = self.countdown,
            open_trimmer = self.open_trimmer,
            screenshot_quality = self.screenshot_quality,
            screenshot_copy = self.screenshot_copy,
            screenshot_save = self.screenshot_save,
            screenshot_upload = self.screenshot_upload,
            upload_destination = q(&self.upload_destination),
            expiry = self.litterbox_expiry.name(),
            upload_copy_link = self.upload_copy_link,
            recording_upload = self.recording_upload,
        )
    }

    pub(crate) fn output_dir(&self) -> PathBuf {
        if self.output_dir.trim().is_empty() {
            videos_dir().join("glint")
        } else {
            PathBuf::from(self.output_dir.trim())
        }
    }

    /// File name (without extension) for a capture made at `date` / `time`.
    pub(crate) fn file_stem(&self, date: &str, time: &str) -> String {
        let pattern = match self.file_name.trim() {
            "" => DEFAULT_FILE_NAME,
            p => p,
        };
        pattern.replace("{date}", date).replace("{time}", time).trim_end_matches(['.', ' ']).to_string()
    }

    /// A new file in the output folder named by the settings' pattern, never overwriting.
    pub(crate) fn unique_path(&self, ext: &str) -> PathBuf {
        let dir = self.output_dir();
        let (date, time) = date_time();
        let stem = self.file_stem(&date, &time);
        let mut path = dir.join(format!("{stem}.{ext}"));
        for n in 2.. {
            if !path.exists() {
                break;
            }
            path = dir.join(format!("{stem}_{n}.{ext}"));
        }
        path
    }
}

/// Why `pattern` can't name files, if it can't.
pub(crate) fn file_name_problem(pattern: &str) -> Option<&'static str> {
    pattern.contains(BAD_NAME_CHARS).then_some("file names can't contain \\ / : * ? \" < > |")
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
            pause_hotkey: "Ctrl+Pause".into(),
            launch_at_startup: true,
            freeze_screen: false,
            ui_elements: UiElements::Always,
            notify_success: false,
            output_dir: r#"D:\clips\"quoted" dir\"#.into(),
            file_name: "clip {date} at {time}".into(),
            fps: 60,
            codec: CodecName::Hevc,
            quality: 18,
            effort: 75,
            draw_cursor: false,
            audio_desktop: true,
            audio_mic: true,
            countdown: 5,
            open_trimmer: true,
            screenshot_style: ShotStyle::Sharex,
            screenshot_format: ShotFormat::Jpg,
            screenshot_quality: 75,
            screenshot_copy: false,
            screenshot_save: true,
            screenshot_upload: true,
            upload_destination: "my \"host\".sxcu".into(),
            litterbox_expiry: Expiry::Hour,
            upload_copy_link: false,
            recording_upload: true,
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
}
