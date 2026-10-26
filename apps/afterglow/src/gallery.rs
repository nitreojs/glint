//! afterglow's side of the gallery, which lists glint's captures too when glint is there (see
//! [`glint_ui::gallery`]): a recording opens in glint's trimmer when glint is there, else in the
//! default player; anything else opens in its default app.

use std::path::{Path, PathBuf};

use glint_core::gallery::{Item, Kind};
use glint_core::history::{self, Entry, Kind as HistoryKind};
use glint_core::naming::Source;
use glint_core::paths::app_dir_for;
use glint_ui::gallery::{self, Extra, Host, Setup, Work};
use windows::Win32::Foundation::HWND;

use crate::app::AMBER;
use crate::config::APP;
use crate::glint;

const PLAYER: u32 = 1;

pub fn run(select: Option<PathBuf>) {
    let setup = Setup { app: APP, accent: AMBER, settings: crate::settings::TITLE, empty: "no clips yet" };
    gallery::run(setup, Afterglow { glint: glint::find() }, select);
}

/// Brings the gallery forward, or starts it (on `select`, when given).
pub fn open(select: Option<&Path>) {
    gallery::open(APP, select);
}

/// Remembers a saved clip, the app it shows and how long it plays (`%APPDATA%\afterglow\history.jsonl`).
pub fn remember(path: &Path, app: Option<&Source>, ms: u32) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let file = app_dir_for(APP).join("history.jsonl");
    history::add_to(&file, Entry::new(HistoryKind::Recording, Some(path), size).with_source(app).with_duration(Some(ms)));
}

struct Afterglow {
    glint: Option<PathBuf>,
}

impl Afterglow {
    /// glint's trimmer opens `item`.
    fn trims(&self, item: &Item) -> bool {
        item.kind == Kind::Recording && self.glint.is_some()
    }
}

impl Host for Afterglow {
    fn open_label(&self, item: &Item) -> &'static str {
        if self.trims(item) { "trim…" } else { "open" }
    }

    fn open(&mut self, item: &Item) -> Result<(), String> {
        match &self.glint {
            Some(exe) if item.kind == Kind::Recording => glint::trim(exe, &item.path),
            _ => {
                gallery::open_file(&item.path);
                Ok(())
            }
        }
    }

    fn extras(&self, item: &Item, _busy: bool) -> Vec<Extra> {
        if self.trims(item) { vec![Extra { key: PLAYER, name: "open in player", top: true }] } else { Vec::new() }
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
