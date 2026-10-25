//! afterglow's side of the gallery: the clips it saved (its own history, so clips saved to an
//! earlier folder stay, and the output folder as deep as the file name makes folders). A clip
//! opens in glint's trimmer when glint is there, else in the default player.

use std::path::{Path, PathBuf};

use glint_core::gallery::{Item, Kind};
use glint_core::history::{self, Kind as HistoryKind};
use glint_core::paths::app_dir_for;
use glint_ui::gallery::{self, Extra, Host, Setup, Work};
use windows::Win32::Foundation::HWND;

use crate::app::AMBER;
use crate::config::{APP, Config, NAMES};
use crate::glint;

const PLAYER: u32 = 1;
pub const KINDS: &[Kind] = &[Kind::Recording];

pub fn run(select: Option<PathBuf>) {
    let setup = Setup {
        app: APP,
        accent: AMBER,
        kinds: KINDS,
        history: history_file(),
        folder: || {
            let cfg = Config::load().unwrap_or_default();
            (cfg.output_dir(), depth(&cfg.file_name))
        },
        settings: crate::settings::TITLE,
        empty: "no clips yet",
    };
    gallery::run(setup, Afterglow { glint: glint::find() }, select);
}

/// Brings the gallery forward, or starts it (on `select`, when given).
pub fn open(select: Option<&Path>) {
    gallery::open(APP, select);
}

/// Every clip saved, as glint keeps its captures (`%APPDATA%\afterglow\history.jsonl`).
fn history_file() -> PathBuf {
    app_dir_for(APP).join("history.jsonl")
}

/// Remembers a saved clip and the app it shows.
pub fn remember(path: &Path, app: Option<&str>) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    history::add_to(&history_file(), HistoryKind::Recording, Some(path), size, None, app);
}

/// How many folders deep clips go below the output folder: the file name's own `/` folders, plus
/// the folder itself.
fn depth(file_name: &str) -> usize {
    let pattern = if file_name.trim().is_empty() { NAMES.default } else { file_name };
    pattern.split('/').filter(|s| !s.trim().is_empty()).count().max(1)
}

struct Afterglow {
    glint: Option<PathBuf>,
}

impl Host for Afterglow {
    fn open_label(&self, _item: &Item) -> &'static str {
        if self.glint.is_some() { "trim…" } else { "open" }
    }

    fn open(&mut self, item: &Item) -> Result<(), String> {
        match &self.glint {
            Some(exe) => glint::trim(exe, &item.path),
            None => {
                gallery::open_file(&item.path);
                Ok(())
            }
        }
    }

    fn extras(&self, _item: &Item, _busy: bool) -> Vec<Extra> {
        match self.glint {
            Some(_) => vec![Extra { key: PLAYER, name: "open in player", top: true }],
            None => Vec::new(),
        }
    }

    fn pick(&mut self, key: u32, item: &Item, _owner: HWND) -> Result<Option<Work>, String> {
        if key == PLAYER {
            gallery::open_file(&item.path);
        }
        Ok(None)
    }

    fn refocus(&mut self) {
        self.glint = glint::find();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_go_as_deep_as_the_file_name_makes_folders() {
        assert_eq!(depth(""), 1);
        assert_eq!(depth("afterglow_{date}_{time}"), 1);
        assert_eq!(depth("{app}/{date}_{time}"), 2);
        assert_eq!(depth("clips/ {app} /{date}/{time}"), 4);
        assert_eq!(depth("/{app}//{time}/"), 2);
    }

    #[test]
    fn the_library_lists_saved_clips_newest_first_and_nothing_else() {
        let dir = std::env::temp_dir().join(format!("afterglow-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, old) = (dir.join("clips"), dir.join("old folder"));
        for d in [root.join("Telegram Desktop"), root.join("deep").join("deeper"), old.clone()] {
            std::fs::create_dir_all(d).unwrap();
        }
        // written `hours` ago
        let file = |p: PathBuf, hours: u64| {
            std::fs::write(&p, b"not really a video").unwrap();
            let at = std::time::SystemTime::now() - std::time::Duration::from_secs(hours * 3600);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(at).unwrap();
            p
        };
        let earlier = file(old.join("afterglow_2026-10-01.mp4"), 48);
        let _top = file(root.join("afterglow_2026-10-24.mp4"), 3);
        let _by_app = file(root.join("Telegram Desktop").join("clip.mp4"), 1);
        let _too_deep = file(root.join("deep").join("deeper").join("lost.mp4"), 0);
        let _shot = file(root.join("screenshot.png"), 0);
        let _other = file(root.join("notes.txt"), 0);
        // saved before the folder moved, and one saved but deleted since
        let history = dir.join("history.jsonl");
        history::add_to(&history, HistoryKind::Recording, Some(&earlier), 18, None, Some("Counter-Strike 2"));
        history::add_to(&history, HistoryKind::Recording, Some(&root.join("gone.mp4")), 18, None, None);

        let items = gallery::list(&history, &root, depth("{app}/{date}_{time}"), KINDS);
        let names: Vec<String> = items.iter().map(Item::name).collect();
        // the history's entry first (its time is when it was added), then the folder newest first
        assert_eq!(names, ["afterglow_2026-10-01.mp4", "clip.mp4", "afterglow_2026-10-24.mp4"]);
        assert!(items.iter().all(|i| i.kind == Kind::Recording));
        // search finds a clip by the app it shows, from the history or its folder
        let find = |q: &str| -> Vec<String> {
            let words = glint_core::gallery::words(q);
            items.iter().filter(|i| i.matches(glint_core::gallery::Filter::All, &words)).map(Item::name).collect()
        };
        assert_eq!(find("counter"), ["afterglow_2026-10-01.mp4"]);
        assert_eq!(find("telegram"), ["clip.mp4"]);
        assert_eq!(items[0].app.as_deref(), Some("Counter-Strike 2"));
        assert!((items[1].time - items[2].time).abs_diff(3600 * 2) <= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
