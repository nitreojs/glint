//! The tray menu of glint and afterglow: our own popup in the apps' look instead of the system's
//! `TrackPopupMenu` — a dark rounded glass panel in Plex Sans with glyphs, hotkey hints,
//! separators, greyed items, switches, submenus and a status header in the app's accent.
//!
//! It behaves like a real menu: it opens at the cursor and flips to stay on the monitor's work
//! area, at that monitor's DPI; it fades and slides in and out and its highlight glides between
//! rows; arrows, Enter, Esc and first letters drive it; a click outside, losing focus, Esc or a
//! pick closes it; it never shows in the taskbar or alt-tab. [`track`] is modal like
//! `TrackPopupMenu`: other windows' messages keep flowing while it's open, and it returns the
//! picked command.

mod draw;
mod layout;
mod nav;
mod popup;

pub use popup::track;

/// What a menu shows. Labels are shown as given (lowercase, by the apps' convention).
pub struct Menu {
    /// The app's colour: the header's mark, accented glyphs, switches that are on.
    pub accent: [u8; 3],
    pub header: Option<Header>,
    pub items: Vec<Item>,
}

/// The line above the items: the app's name and what it's doing.
pub struct Header {
    pub title: String,
    pub status: String,
}

/// A small line drawing left of a label.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Glyph {
    /// A filled dot: start recording.
    Record,
    Stop,
    Pause,
    /// A triangle: resume.
    Play,
    /// Four corner brackets: pick a region.
    Region,
    /// Two screens: all of them.
    Screens,
    /// A screen on a stand: this one.
    Monitor,
    /// A counter-clockwise arrow round a dot: save what was just on screen.
    Replay,
    /// Scissors.
    Trim,
    /// A clock.
    Recent,
    Folder,
    /// Two sliders.
    Settings,
    /// A page of lines.
    Log,
    /// A clockwise arrow: restart into an update.
    Update,
    /// The power symbol.
    Quit,
    /// A frame with a play triangle: a recording.
    Video,
    /// A frame with a hill: a screenshot.
    Image,
    /// Four tiles: every capture.
    Gallery,
    /// An arrow out of a box: open outside.
    Open,
    /// Two overlapping squares.
    Copy,
    /// An arrow up from a line.
    Upload,
    /// An arrow down onto a line: save to a file.
    Save,
    /// A cross.
    Clear,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Kind {
    Command(usize),
    Sub(Vec<Item>),
    Separator,
}

/// A row of a menu.
#[derive(Clone, PartialEq, Debug)]
pub struct Item {
    pub(crate) kind: Kind,
    pub(crate) label: String,
    pub(crate) hint: String,
    pub(crate) glyph: Option<Glyph>,
    pub(crate) accent: bool,
    pub(crate) enabled: bool,
    pub(crate) check: Option<bool>,
}

impl Item {
    fn new(kind: Kind, label: &str) -> Item {
        Item { kind, label: label.to_string(), hint: String::new(), glyph: None, accent: false, enabled: true, check: None }
    }

    /// Picking it makes [`track`] return `id`.
    pub fn command(id: usize, label: &str) -> Item {
        Item::new(Kind::Command(id), label)
    }

    /// Opens `items` beside it; greyed when there's nothing in them.
    pub fn submenu(label: &str, items: Vec<Item>) -> Item {
        let enabled = !items.is_empty();
        Item { enabled, ..Item::new(Kind::Sub(items), label) }
    }

    pub fn separator() -> Item {
        Item { enabled: false, ..Item::new(Kind::Separator, "") }
    }

    /// Right-aligned, muted: a detail, shown as given (`3.7 MB`).
    pub fn hint(mut self, hint: Option<&str>) -> Item {
        self.hint = hint.unwrap_or_default().to_string();
        self
    }

    /// A hotkey as the hint, shown lowercase (`alt+f10`).
    pub fn keys(mut self, keys: Option<&str>) -> Item {
        self.hint = keys.map(str::to_lowercase).unwrap_or_default();
        self
    }

    pub fn glyph(mut self, glyph: Glyph) -> Item {
        self.glyph = Some(glyph);
        self
    }

    /// Draws the glyph in the accent: the menu's main action.
    pub fn accented(mut self) -> Item {
        self.accent = true;
        self
    }

    pub fn enabled(mut self, on: bool) -> Item {
        self.enabled = on && !matches!(self.kind, Kind::Separator);
        self
    }

    /// A switch on the right, on or off; picking the item flips what it stands for.
    pub fn checked(mut self, on: bool) -> Item {
        self.check = Some(on);
        self
    }

    /// The mouse and the keyboard can land on it.
    pub(crate) fn selectable(&self) -> bool {
        self.enabled && !matches!(self.kind, Kind::Separator)
    }

    pub(crate) fn children(&self) -> Option<&[Item]> {
        match &self.kind {
            Kind::Sub(items) if self.enabled => Some(items),
            _ => None,
        }
    }
}
