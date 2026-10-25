use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Ui};
use glint_core::config::Config;
use glint_core::history;
use glint_core::log::log;
use glint_core::naming::GLINT;
use glint_ui::naming::{self, About};
use glint_ui::widgets::{self, Kind, Look, hint, row};

use crate::app::ipc;
use glint_shell::explorer::pick_folder;
use crate::platform::paths::{old_default_dir, output_dir, previous_output_dir, unused};
use crate::settings::{SettingsApp, repaint};

const CAPTURE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "mp4", "gif"];
/// Folder levels searched below a folder glint made itself (the pattern's folders).
const DEPTH: usize = 3;

/// Captures left in a folder glint used before.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::settings) struct Leftover {
    pub from: PathBuf,
    pub files: Vec<PathBuf>,
}

#[derive(Default)]
struct Progress {
    done: usize,
    total: usize,
    /// The summary once every file was tried, and whether something failed.
    finished: Option<(String, bool)>,
}

/// Offers to bring captures from the previous folder into the current one.
#[derive(Default)]
pub(in crate::settings) struct Mover {
    /// The output folder `leftovers` were looked up for.
    pub scanned: Option<PathBuf>,
    pub leftovers: Vec<Leftover>,
    job: Option<Arc<Mutex<Progress>>>,
    result: Option<(String, bool)>,
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
}

/// Files with a capture's extension in `dir` and up to [`DEPTH`] folders below.
fn captures_in(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_dir() && depth > 0 {
            captures_in(&path, depth - 1, out);
        } else if kind.is_file()
            && path.extension().is_some_and(|x| CAPTURE_EXTENSIONS.iter().any(|c| x.eq_ignore_ascii_case(c)))
        {
            out.push(path);
        }
    }
}

/// Folders glint saved to before `current` that still hold captures: the old default folder
/// (everything in it is glint's), and the folder used before the last change (only the files
/// glint's history knows, since it may be a shared folder like the desktop).
fn leftovers(current: &Path, previous: Option<PathBuf>, owned: &Path, known: &[PathBuf]) -> Vec<Leftover> {
    let mut found: Vec<Leftover> = Vec::new();
    for dir in [Some(owned.to_path_buf()), previous].into_iter().flatten() {
        if same_dir(&dir, current) || found.iter().any(|l| same_dir(&l.from, &dir)) {
            continue;
        }
        let mut files = Vec::new();
        if same_dir(&dir, owned) {
            captures_in(&dir, DEPTH, &mut files);
        } else {
            files.extend(known.iter().filter(|p| p.starts_with(&dir) && p.is_file()).cloned());
        }
        files.sort();
        if !files.is_empty() {
            found.push(Leftover { from: dir, files });
        }
    }
    found
}

/// Moves `from` to `to` (or a free name next to it), renaming on the same drive and copying
/// otherwise; returns where it ended up.
fn move_file(from: &Path, to: PathBuf) -> std::io::Result<PathBuf> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let to = unused(to);
    if std::fs::rename(from, &to).is_err() {
        std::fs::copy(from, &to)?;
        if let Err(e) = std::fs::remove_file(from) {
            let _ = std::fs::remove_file(&to);
            return Err(e);
        }
    }
    Ok(to)
}

/// Moves `files` from below `from` to the same places below `to`, reporting into `progress`.
fn move_all(from: &Path, files: &[PathBuf], to: &Path, progress: &Mutex<Progress>) {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for file in files {
        let rel = file.strip_prefix(from).unwrap_or(file.file_name().map(Path::new).unwrap_or(file));
        match move_file(file, to.join(rel)) {
            Ok(dest) => moved.push((file.clone(), dest)),
            Err(e) => failed.push(format!("{}: {e}", file.display())),
        }
        progress.lock().unwrap().done += 1;
        repaint();
    }
    if let Err(e) = history::relocate(&moved) {
        log(&format!("history: {e}"));
    }
    let s = |n: usize| if n == 1 { "" } else { "s" };
    let summary = match failed.first() {
        None => format!("moved {} file{}", moved.len(), s(moved.len())),
        Some(first) => {
            log(&format!("moving captures: {}", failed.join("; ")));
            format!("moved {} file{}, {} couldn't be moved: {first}", moved.len(), s(moved.len()), failed.len())
        }
    };
    progress.lock().unwrap().finished = Some((summary, !failed.is_empty()));
    repaint();
}

impl Mover {
    /// Looks for leftovers again when the saved output folder changed.
    fn refresh(&mut self, current: &Path) {
        if self.job.is_some() || self.scanned.as_deref() == Some(current) {
            return;
        }
        let known: Vec<PathBuf> = history::recent(usize::MAX).into_iter().filter_map(|e| e.path).collect();
        self.leftovers = leftovers(current, previous_output_dir(), &old_default_dir(), &known);
        self.scanned = Some(current.to_path_buf());
    }

    fn start(&mut self, which: usize, to: PathBuf) {
        let Some(left) = self.leftovers.get(which).cloned() else { return };
        let progress = Arc::new(Mutex::new(Progress { total: left.files.len(), ..Progress::default() }));
        self.result = None;
        self.job = Some(progress.clone());
        let spawned = std::thread::Builder::new()
            .name("move captures".into())
            .spawn(move || move_all(&left.from, &left.files, &to, &progress));
        if let Err(e) = spawned {
            self.job = None;
            self.result = Some((format!("can't move files: {e}"), true));
        }
    }

    /// Takes a finished job's result; returns the progress of a running one.
    fn poll(&mut self) -> Option<(usize, usize)> {
        let job = self.job.as_ref()?;
        let mut p = job.lock().unwrap();
        if let Some(done) = p.finished.take() {
            drop(p);
            self.job = None;
            self.result = Some(done);
            self.scanned = None;
            return None;
        }
        Some((p.done, p.total))
    }
}

impl SettingsApp {
    pub(in crate::settings) fn output(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let default_dir = output_dir(&Config { output_dir: String::new(), ..Config::default() });
        let current = output_dir(&self.saved);
        let folder_saved = self.cfg.output_dir == self.saved.output_dir;
        if folder_saved {
            self.mover.refresh(&current);
        }
        let running = self.mover.poll();
        row(ui, look, "folder", 7.0, |ui| {
            widgets::text_field(ui, look, &mut self.cfg.output_dir, &default_dir.display().to_string(), false);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if widgets::button(ui, look, "browse…", Kind::Secondary, 28.0, true).clicked()
                    && let Some(dir) = pick_folder(&output_dir(&self.cfg), ipc::find_settings())
                {
                    self.cfg.output_dir = dir.display().to_string();
                }
                if widgets::button(ui, look, "open folder", Kind::Secondary, 28.0, true).clicked() {
                    let dir = output_dir(&self.cfg);
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                }
            });
            ui.add_space(2.0);
            hint(ui, "recordings and screenshots are saved here. leave empty for the default.", p.t3);
        });
        let shown = folder_saved || running.is_some() || self.mover.result.is_some();
        let any = !self.mover.leftovers.is_empty() || running.is_some() || self.mover.result.is_some();
        widgets::reveal(ui, egui::Id::unique("older-captures"), shown && any, |ui| self.move_row(ui, look, &current, running));
        let about = About {
            window: "files start with the app you captured, like telegram desktop_…; whole screens are screen_…",
            folders: "a folder per app; whole screens go to screen, regions to other",
        };
        naming::rows(ui, look, &GLINT, &mut self.cfg.file_name, &about);
    }

    fn move_row(&mut self, ui: &mut Ui, look: &Look, current: &Path, running: Option<(usize, usize)>) {
        let p = look.p;
        let mut start = None;
        row(ui, look, "older captures", 7.0, |ui| {
            if let Some((done, total)) = running {
                hint(ui, format!("moving {done} of {total}…"), p.t2);
                return;
            }
            for (i, left) in self.mover.leftovers.iter().enumerate() {
                let n = left.files.len();
                let what = if n == 1 { "capture is" } else { "captures are" };
                hint(ui, format!("{n} {what} still in {}", left.from.display()), p.t2);
                ui.push_id(i, |ui| {
                    if widgets::button(ui, look, "move my files here", Kind::Secondary, 28.0, true).clicked() {
                        start = Some(i);
                    }
                });
                ui.add_space(4.0);
            }
            let result = self.mover.result.clone();
            widgets::reveal_with(ui, egui::Id::unique("move-result"), result, |ui, (text, failed)| {
                hint(ui, text, if failed { p.warn } else { p.t3 });
            });
        });
        if let Some(i) = start {
            self.mover.start(i, current.to_path_buf());
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
pub(in crate::settings) mod tests {
    use super::*;

    pub(in crate::settings) fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("glint-output-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn leftovers_are_glints_own_files_only() {
        let root = temp("leftovers");
        let (owned, previous, current) = (root.join("Videos").join("glint"), root.join("Desktop"), root.join("now"));
        for f in [owned.join("a.mp4"), owned.join("Telegram").join("b.PNG"), owned.join("notes.txt"), previous.join("mine.png"), previous.join("shot.png")] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(&f, b"x").unwrap();
        }
        let known = [previous.join("shot.png"), previous.join("deleted.png"), root.join("elsewhere.png")];
        let found = leftovers(&current, Some(previous.clone()), &owned, &known);
        assert_eq!(
            found,
            [
                Leftover { from: owned.clone(), files: vec![owned.join("Telegram").join("b.PNG"), owned.join("a.mp4")] },
                Leftover { from: previous.clone(), files: vec![previous.join("shot.png")] },
            ]
        );
        // nothing to offer for the folder in use, and each folder once
        let found = leftovers(&owned, Some(owned.join("")), &owned, &known);
        assert!(found.is_empty(), "{found:?}");
        assert!(same_dir(Path::new(r"C:\A\b\"), Path::new(r"c:\a\B")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn moving_keeps_folders_and_never_overwrites() {
        let root = temp("move");
        let (from, to) = (root.join("old"), root.join("new"));
        let files = [from.join("a.png"), from.join("Telegram").join("b.mp4")];
        for f in &files {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, f.display().to_string()).unwrap();
        }
        std::fs::create_dir_all(&to).unwrap();
        std::fs::write(to.join("a.png"), b"already here").unwrap();
        let progress = Mutex::new(Progress::default());
        move_all(&from, &files, &to, &progress);
        let p = progress.into_inner().unwrap();
        assert_eq!(p.done, 2);
        assert_eq!(p.finished, Some(("moved 2 files".into(), false)));
        assert_eq!(std::fs::read(to.join("a.png")).unwrap(), b"already here");
        assert_eq!(std::fs::read_to_string(to.join("a_2.png")).unwrap(), files[0].display().to_string());
        assert_eq!(std::fs::read_to_string(to.join("Telegram").join("b.mp4")).unwrap(), files[1].display().to_string());
        assert!(files.iter().all(|f| !f.exists()));
        let progress = Mutex::new(Progress::default());
        move_all(&from, &[from.join("gone.png")], &to, &progress);
        let (text, failed) = progress.into_inner().unwrap().finished.unwrap();
        assert!(failed && text.starts_with("moved 0 files, 1 couldn't be moved: "), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
