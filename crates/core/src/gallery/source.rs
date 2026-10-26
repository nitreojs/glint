//! Where glint and afterglow keep their captures, read from each app's own files (its history and
//! the output folder its settings name), so either app's gallery lists both apps' captures.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::{DEFAULT_ACCENT, parse_hex};
use crate::naming::{AFTERGLOW, GLINT, Names};

/// Which app made a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Maker {
    Glint,
    Afterglow,
}

/// afterglow's accent.
pub const AMBER: [u8; 3] = [0xF5, 0x9E, 0x0B];
/// glint looks at least this deep below its folder: `{app}/` folders and one more.
const GLINT_DEPTH: usize = 3;

impl Maker {
    pub const ALL: [Maker; 2] = [Maker::Glint, Maker::Afterglow];

    pub fn name(self) -> &'static str {
        match self {
            Maker::Glint => "glint",
            Maker::Afterglow => "afterglow",
        }
    }

    fn names(self) -> Names {
        match self {
            Maker::Glint => GLINT,
            Maker::Afterglow => AFTERGLOW,
        }
    }

    /// The app's own folder (`%APPDATA%\<app>`) under `appdata`.
    pub fn dir(self, appdata: &Path) -> PathBuf {
        appdata.join(self.name())
    }

    /// The app's history file under `appdata`.
    pub fn history(self, appdata: &Path) -> PathBuf {
        self.dir(appdata).join("history.jsonl")
    }

    /// The app has run here: it left its settings or a history.
    pub fn installed(self, appdata: &Path) -> bool {
        let dir = self.dir(appdata);
        dir.join("config.toml").is_file() || dir.join("history.jsonl").is_file()
    }
}

/// One app's captures: its history, and its output folder looked at `depth` folders deep (1: only
/// the folder itself).
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub maker: Maker,
    pub history: PathBuf,
    pub root: PathBuf,
    pub depth: usize,
    /// Which folder below `root` (0 the first) is named after the app a capture shows.
    pub app_folder: Option<usize>,
    pub accent: [u8; 3],
}

/// What the gallery needs from an app's settings; the rest is ignored, so a file from a newer
/// version still gives its folder.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Settings {
    output_dir: String,
    file_name: String,
    accent: String,
}

impl Source {
    /// `maker`'s captures, from its files under `appdata` (`%APPDATA%`); `default_root` is where
    /// they go while its settings name no folder.
    pub fn read(maker: Maker, appdata: &Path, default_root: impl FnOnce() -> PathBuf) -> Source {
        let text = std::fs::read_to_string(maker.dir(appdata).join("config.toml")).unwrap_or_default();
        let settings: Settings = toml::from_str(&text).unwrap_or_default();
        let root = match settings.output_dir.trim() {
            "" => default_root(),
            dir => PathBuf::from(dir),
        };
        let names = maker.names();
        let depth = names.depth(&settings.file_name);
        let (depth, accent) = match maker {
            Maker::Glint => (depth.max(GLINT_DEPTH), parse_hex(&settings.accent).or_else(|| parse_hex(DEFAULT_ACCENT)).unwrap_or_default()),
            Maker::Afterglow => (depth, AMBER),
        };
        Source { maker, history: maker.history(appdata), root, depth, app_folder: names.app_folder(&settings.file_name), accent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_app_is_read_from_its_own_settings() {
        let appdata = std::env::temp_dir().join(format!("glint-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&appdata);
        assert!(!Maker::Glint.installed(&appdata) && !Maker::Afterglow.installed(&appdata));
        let fallback = |name: &str| appdata.join("defaults").join(name);
        // never run: defaults all through
        let glint = Source::read(Maker::Glint, &appdata, || fallback("glint"));
        assert_eq!(glint.root, fallback("glint"));
        assert_eq!((glint.depth, glint.app_folder, glint.accent), (3, None, [0xE5, 0x40, 0x2F]));
        assert_eq!(glint.history, appdata.join("glint").join("history.jsonl"));

        for (app, text) in [
            ("glint", "output_dir = 'F:\\glint videos'\nfile_name = '{kind}/{app}/{app}_{date}'\naccent = '#7E3F67'\nhotkey = 'x'\n"),
            ("afterglow", "output_dir = '  '\nfile_name = '{app}/{date}_{time}'\nlength = 60\n"),
        ] {
            std::fs::create_dir_all(appdata.join(app)).unwrap();
            std::fs::write(appdata.join(app).join("config.toml"), text).unwrap();
        }
        let glint = Source::read(Maker::Glint, &appdata, || fallback("glint"));
        assert_eq!(glint.root, PathBuf::from(r"F:\glint videos"));
        assert_eq!((glint.depth, glint.app_folder, glint.accent), (3, Some(1), [0x7E, 0x3F, 0x67]));
        let after = Source::read(Maker::Afterglow, &appdata, || fallback("afterglow"));
        assert_eq!(after.root, fallback("afterglow"), "a blank folder is the default one");
        assert_eq!((after.depth, after.app_folder, after.accent), (2, Some(0), AMBER));
        assert!(Maker::Glint.installed(&appdata) && Maker::Afterglow.installed(&appdata));

        // a file it can't read still lists the default folder
        std::fs::write(appdata.join("afterglow").join("config.toml"), "not = [toml").unwrap();
        let broken = Source::read(Maker::Afterglow, &appdata, || fallback("afterglow"));
        assert_eq!((broken.root, broken.depth), (fallback("afterglow"), 1));
        let _ = std::fs::remove_dir_all(&appdata);
    }
}
