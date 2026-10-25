//! The list of captures, read off the UI thread: the history plus a walk of the output folder,
//! read again whenever either changes (checked every second) or the window asks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use glint_core::config::Config;
use glint_core::gallery::{self, Item, Kind, OnDisk, path_id};
use glint_core::history;
use glint_core::log::log;

use crate::platform::paths::output_dir;

/// How deep the walk goes below the output folder (`{app}/` folders and one more).
const DEPTH: usize = 3;
/// Past this many files the folder isn't only glint's: stop looking.
const MOST: usize = 20_000;
const EVERY: Duration = Duration::from_secs(1);

pub(super) struct Loaded {
    pub items: Vec<Item>,
    /// How long reading took.
    pub took: Duration,
}

pub(super) struct Library {
    rx: Receiver<Loaded>,
    again: Sender<()>,
}

impl Library {
    /// Starts reading at once; `ctx` is set once the window exists, so it can be woken.
    pub fn start(ctx: std::sync::Arc<std::sync::OnceLock<egui::Context>>) -> Library {
        let (tx, rx) = mpsc::channel();
        let (again, asked) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("library".into()).spawn(move || watch(&tx, &asked, &ctx));
        if let Err(e) = spawned {
            log(&format!("gallery: can't read the captures: {e}"));
        }
        Library { rx, again }
    }

    /// The newest list, if one came since the last call.
    pub fn poll(&self) -> Option<Loaded> {
        self.rx.try_iter().last()
    }

    /// Read everything again now (the window came back to the front).
    pub fn refresh(&self) {
        let _ = self.again.send(());
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn watch(tx: &Sender<Loaded>, asked: &Receiver<()>, ctx: &std::sync::OnceLock<egui::Context>) {
    let mut seen = None;
    let mut root = PathBuf::new();
    let mut forced = true;
    loop {
        if forced {
            // the settings may have moved the folder since
            root = output_dir(&Config::load().unwrap_or_default());
        }
        let stamp = (mtime(&history::file()), mtime(&root), root.clone());
        if forced || seen.as_ref() != Some(&stamp) {
            seen = Some(stamp);
            let started = Instant::now();
            let items = load(&root);
            if tx.send(Loaded { items, took: started.elapsed() }).is_err() {
                return;
            }
            if let Some(ctx) = ctx.get() {
                ctx.request_repaint();
            }
        }
        forced = match asked.recv_timeout(EVERY) {
            Ok(()) => {
                while asked.try_recv().is_ok() {}
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

fn nanos(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

fn load(root: &Path) -> Vec<Item> {
    let mut found = Vec::new();
    walk(root, DEPTH, &mut found);
    let on_disk: HashMap<u64, (u64, u64)> = found.iter().map(|f| (path_id(&f.path), (f.size, f.modified))).collect();
    let stat = |p: &Path| match on_disk.get(&path_id(p)) {
        Some(&s) => Some(s),
        // outside the folder (it moved since, or an export saved elsewhere)
        None => std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| (m.len(), m.modified().map(nanos).unwrap_or(0))),
    };
    gallery::collect(&history::all(), found, root, stat)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<OnDisk>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if out.len() >= MOST {
            return;
        }
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_dir() {
            let hidden = e.file_name().to_string_lossy().starts_with('.');
            if depth > 1 && !hidden {
                walk(&path, depth - 1, out);
            }
        } else if kind.is_file() && Kind::of(&path).is_some()
            && let Ok(m) = e.metadata()
        {
            out.push(OnDisk { path, size: m.len(), modified: m.modified().map(nanos).unwrap_or(0) });
        }
    }
}
