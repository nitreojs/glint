//! glint's side of the gallery: its captures (the history and the output folder), recordings open
//! in the trimmer, and an upload line once an uploader is set up.

use std::path::{Path, PathBuf};

use glint_core::config::Config;
use glint_core::gallery::{Item, Kind};
use glint_core::history::{self, Kind as HistoryKind};
use glint_core::log::log;
use glint_core::upload::{Destination, Payload};
use glint_ui::gallery::{self, Extra, Host, Note, Setup, Work};
use windows::Win32::Foundation::HWND;

use crate::app::ipc::SETTINGS_TITLE;
use crate::platform::paths::output_dir;

const APP: &str = "glint";
/// How deep the walk goes below the output folder (`{app}/` folders and one more).
const DEPTH: usize = 3;
const UPLOAD: u32 = 1;

pub fn run(select: Option<PathBuf>) {
    let setup = Setup {
        app: APP,
        accent: Config::load().unwrap_or_default().accent_rgb(),
        kinds: &[Kind::Shot, Kind::Recording, Kind::Gif],
        history: history::file(),
        folder: || (output_dir(&Config::load().unwrap_or_default()), DEPTH),
        settings: SETTINGS_TITLE,
        empty: "nothing captured yet",
    };
    gallery::run(setup, Glint { uploadable: uploadable() }, select);
}

/// Brings the gallery forward, or starts it (on `select`, when given).
pub(crate) fn open(select: Option<&Path>) {
    gallery::open(APP, select);
}

struct Glint {
    /// An uploader is set up in the settings.
    uploadable: bool,
}

fn uploadable() -> bool {
    Destination::from_config(&Config::load().unwrap_or_default()).is_ok()
}

impl Host for Glint {
    fn open_label(&self, item: &Item) -> &'static str {
        if item.kind == Kind::Recording { "trim…" } else { "open" }
    }

    fn open(&mut self, item: &Item) -> Result<(), String> {
        if item.kind == Kind::Recording {
            return crate::clip::spawn(&item.path).map_err(|e| format!("can't open the trimmer: {e}"));
        }
        gallery::open_file(&item.path);
        Ok(())
    }

    fn extras(&self, _item: &Item, busy: bool) -> Vec<Extra> {
        if self.uploadable && !busy { vec![Extra { key: UPLOAD, name: "upload", top: false }] } else { Vec::new() }
    }

    fn pick(&mut self, key: u32, item: &Item, owner: HWND) -> Result<Option<Work>, String> {
        if key != UPLOAD {
            return Ok(None);
        }
        upload(item, owner).map(Some).map_err(|e| format!("couldn't upload: {e}"))
    }

    fn refocus(&mut self) {
        self.uploadable = uploadable();
    }
}

/// Uploads `item`, then remembers its link and copies it (as the settings say).
fn upload(item: &Item, owner: HWND) -> Result<Work, String> {
    let cfg = Config::load().unwrap_or_default();
    let dest = Destination::from_config(&cfg)?;
    let payload = Payload::file(&item.path)?;
    dest.check_size(&payload)?;
    let (path, size, copy_link) = (item.path.clone(), payload.size, cfg.upload_copy_link);
    let kind = if item.kind == Kind::Shot { HistoryKind::Shot } else { HistoryKind::Recording };
    let owner = owner.0 as isize;
    Ok(std::thread::spawn(move || {
        let url = crate::upload::upload(&dest, payload).map(|u| u.url).map_err(|e| {
            log(&format!("gallery: upload to {} failed: {e}", dest.name));
            format!("couldn't upload to {}: {e}", dest.name)
        })?;
        history::add(kind, Some(&path), size, Some(&url), None);
        let copied = copy_link && glint_shell::clipboard::copy_text(Some(HWND(owner as _)), &url).is_ok();
        let text = if copied { "uploaded, link copied" } else { "uploaded" };
        Ok(Note { text: text.into(), link: Some(url) })
    }))
}
