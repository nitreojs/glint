use glint_core::config::Config;
use glint_core::config::audio::{MAX_TRACKS, track_name};

mod about;
pub(super) mod audio;
pub(super) mod general;
pub(super) mod output;
mod recording;
mod screenshots;
pub(super) mod updates;
pub(super) mod uploads;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Section {
    General,
    Recording,
    Audio,
    Screenshots,
    Uploads,
    Output,
    Updates,
    About,
}

pub(super) const SECTIONS: [Section; 8] = [
    Section::General,
    Section::Recording,
    Section::Audio,
    Section::Screenshots,
    Section::Uploads,
    Section::Output,
    Section::Updates,
    Section::About,
];

impl Section {
    pub(super) fn name(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Recording => "recording",
            Section::Audio => "audio",
            Section::Screenshots => "screenshots",
            Section::Uploads => "uploads",
            Section::Output => "output",
            Section::Updates => "updates",
            Section::About => "about",
        }
    }

    /// How many of this section's settings differ between `a` and `b`.
    pub(super) fn changes(self, a: &Config, b: &Config) -> usize {
        let n = |same: bool| usize::from(!same);
        match self {
            Section::General => {
                n(a.hotkey == b.hotkey)
                    + n(a.screenshot_hotkey == b.screenshot_hotkey)
                    + n(a.fullscreen_hotkey == b.fullscreen_hotkey)
                    + n(a.monitor_hotkey == b.monitor_hotkey)
                    + n(a.pause_hotkey == b.pause_hotkey)
                    + n(a.launch_at_startup == b.launch_at_startup)
                    + n(a.freeze_screen == b.freeze_screen)
                    + n(a.ui_elements == b.ui_elements)
                    + n(a.notify_success == b.notify_success)
                    + n(a.accent_rgb() == b.accent_rgb() && a.accent_custom_rgb() == b.accent_custom_rgb())
            }
            Section::Recording => {
                n(a.fps == b.fps)
                    + n(a.codec == b.codec)
                    + n(a.quality == b.quality)
                    + n(a.effort == b.effort)
                    + n(a.countdown == b.countdown)
                    + n(a.indicator_inside == b.indicator_inside)
                    + n(a.draw_cursor == b.draw_cursor)
                    + n(a.open_trimmer == b.open_trimmer)
                    + n(a.click_highlights == b.click_highlights)
                    + n(a.clicks == b.clicks)
                    + n(a.keystrokes == b.keystrokes)
                    + n(a.keystroke_position == b.keystroke_position)
                    + n(a.keystrokes_mode == b.keystrokes_mode)
            }
            Section::Audio => {
                let (x, y) = (&a.audio_sources, &b.audio_sources);
                let named = |n| track_name(&a.audio_track_names, n) != track_name(&b.audio_track_names, n);
                x.iter().zip(y).filter(|(s, t)| s != t).count() + x.len().abs_diff(y.len()) + (1..=MAX_TRACKS).filter(|&n| named(n)).count()
            }
            Section::Screenshots => {
                n(a.screenshot_format == b.screenshot_format)
                    + n(a.screenshot_style == b.screenshot_style)
                    + n(a.screenshot_quality == b.screenshot_quality)
                    + n(a.screenshot_cursor == b.screenshot_cursor)
                    + n(a.screenshot_copy == b.screenshot_copy)
                    + n(a.screenshot_save == b.screenshot_save)
                    + n(a.screenshot_upload == b.screenshot_upload)
            }
            Section::Uploads => {
                n(a.upload_destination == b.upload_destination)
                    + n(a.litterbox_expiry == b.litterbox_expiry)
                    + n(a.upload_copy_link == b.upload_copy_link)
                    + n(a.recording_upload == b.recording_upload)
                    + n(a.upload_proxy == b.upload_proxy)
            }
            Section::Output => n(a.output_dir == b.output_dir) + n(a.file_name == b.file_name),
            Section::Updates => n(a.update_mode == b.update_mode) + n(a.update_beta == b.update_beta),
            Section::About => 0,
        }
    }

    /// Puts this section's settings back to their defaults.
    pub(super) fn reset(self, cfg: &mut Config) {
        let d = Config::default();
        match self {
            Section::General => {
                cfg.hotkey = d.hotkey;
                cfg.screenshot_hotkey = d.screenshot_hotkey;
                cfg.fullscreen_hotkey = d.fullscreen_hotkey;
                cfg.monitor_hotkey = d.monitor_hotkey;
                cfg.pause_hotkey = d.pause_hotkey;
                cfg.launch_at_startup = d.launch_at_startup;
                cfg.freeze_screen = d.freeze_screen;
                cfg.ui_elements = d.ui_elements;
                cfg.notify_success = d.notify_success;
                cfg.accent = d.accent;
                cfg.accent_custom = d.accent_custom;
            }
            Section::Recording => {
                cfg.fps = d.fps;
                cfg.codec = d.codec;
                cfg.quality = d.quality;
                cfg.effort = d.effort;
                cfg.countdown = d.countdown;
                cfg.indicator_inside = d.indicator_inside;
                cfg.draw_cursor = d.draw_cursor;
                cfg.open_trimmer = d.open_trimmer;
                cfg.click_highlights = d.click_highlights;
                cfg.clicks = d.clicks;
                cfg.keystrokes = d.keystrokes;
                cfg.keystroke_position = d.keystroke_position;
                cfg.keystrokes_mode = d.keystrokes_mode;
            }
            Section::Audio => {
                cfg.audio_sources = d.audio_sources;
                cfg.audio_track_names = d.audio_track_names;
            }
            Section::Screenshots => {
                cfg.screenshot_format = d.screenshot_format;
                cfg.screenshot_style = d.screenshot_style;
                cfg.screenshot_quality = d.screenshot_quality;
                cfg.screenshot_cursor = d.screenshot_cursor;
                cfg.screenshot_copy = d.screenshot_copy;
                cfg.screenshot_save = d.screenshot_save;
                cfg.screenshot_upload = d.screenshot_upload;
            }
            Section::Uploads => {
                cfg.upload_destination = d.upload_destination;
                cfg.litterbox_expiry = d.litterbox_expiry;
                cfg.upload_copy_link = d.upload_copy_link;
                cfg.recording_upload = d.recording_upload;
                cfg.upload_proxy = d.upload_proxy;
            }
            Section::Output => {
                cfg.output_dir = d.output_dir;
                cfg.file_name = d.file_name;
            }
            Section::Updates => {
                cfg.update_mode = d.update_mode;
                cfg.update_beta = d.update_beta;
            }
            Section::About => {}
        }
    }
}
