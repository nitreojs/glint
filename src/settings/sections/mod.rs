use crate::config::Config;

mod about;
mod general;
mod output;
mod recording;
mod screenshots;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Section {
    General,
    Recording,
    Screenshots,
    Output,
    About,
}

pub(super) const SECTIONS: [Section; 5] =
    [Section::General, Section::Recording, Section::Screenshots, Section::Output, Section::About];

impl Section {
    pub(super) fn name(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Recording => "recording",
            Section::Screenshots => "screenshots",
            Section::Output => "output",
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
                    + n(a.launch_at_startup == b.launch_at_startup)
                    + n(a.freeze_screen == b.freeze_screen)
            }
            Section::Recording => {
                n(a.fps == b.fps)
                    + n(a.codec == b.codec)
                    + n(a.quality == b.quality)
                    + n(a.effort == b.effort)
                    + n(a.draw_cursor == b.draw_cursor)
            }
            Section::Screenshots => {
                n(a.screenshot_format == b.screenshot_format)
                    + n(a.screenshot_quality == b.screenshot_quality)
                    + n(a.screenshot_copy == b.screenshot_copy)
                    + n(a.screenshot_save == b.screenshot_save)
            }
            Section::Output => n(a.output_dir == b.output_dir) + n(a.file_name == b.file_name),
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
                cfg.launch_at_startup = d.launch_at_startup;
                cfg.freeze_screen = d.freeze_screen;
            }
            Section::Recording => {
                cfg.fps = d.fps;
                cfg.codec = d.codec;
                cfg.quality = d.quality;
                cfg.effort = d.effort;
                cfg.draw_cursor = d.draw_cursor;
            }
            Section::Screenshots => {
                cfg.screenshot_format = d.screenshot_format;
                cfg.screenshot_quality = d.screenshot_quality;
                cfg.screenshot_copy = d.screenshot_copy;
                cfg.screenshot_save = d.screenshot_save;
            }
            Section::Output => {
                cfg.output_dir = d.output_dir;
                cfg.file_name = d.file_name;
            }
            Section::About => {}
        }
    }
}
