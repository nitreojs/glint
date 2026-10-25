use std::path::PathBuf;

use glint_core::config::{Config, ShotFormat};
use glint_core::geom::rect::Rect;
use glint_core::history::{self, Kind};
use glint_core::naming::{Source, Subject};
use glint_core::upload::Payload;
use windows::Win32::Foundation::HWND;

use crate::platform::paths::{file_name, unique_path};
use crate::upload;

pub(crate) mod editor;
mod output;
pub(crate) mod pin;

use editor::Output;
pub(crate) use output::{ImageFormat, copy_files, copy_text, copy_to_clipboard, encode};

/// A finished screenshot as 0xAARRGGBB rows, and what to do with it.
pub(crate) struct Shot {
    pub pixels: Vec<u32>,
    pub w: u32,
    pub h: u32,
    /// The top byte is alpha (a multi-window shot); otherwise it's garbage and the shot is opaque.
    pub alpha: bool,
    /// Where the image was on the desktop.
    pub at: Rect,
    pub output: Output,
    /// The app window it shows, when it shows one app's window (or a part of it).
    pub source: Option<Source>,
}

pub(crate) struct Delivered {
    pub copied: bool,
    pub saved: Option<(PathBuf, usize)>,
    pub uploading: bool,
}

/// Copies, saves and / or uploads a finished screenshot, as the editor's button or the settings
/// say; `subject` names the file unless the shot is one app's window.
pub(crate) fn deliver(shot: &Shot, cfg: &Config, owner: HWND, subject: Subject) -> Result<Delivered, String> {
    let subject = shot.source.as_ref().map_or(subject, Subject::Window);
    let (copy, save, upload) = match shot.output {
        // With everything switched off in the settings, Enter still has to do something.
        Output::Default if !cfg.screenshot_copy && !cfg.screenshot_save && !cfg.screenshot_upload => (true, false, false),
        Output::Default => (cfg.screenshot_copy, cfg.screenshot_save, cfg.screenshot_upload),
        Output::Copy => (true, false, false),
        Output::Save => (false, true, false),
        Output::Upload => (false, false, true),
        Output::Pin => (false, false, false),
    };
    let mut errors = Vec::new();
    if copy && let Err(e) = output::copy_to_clipboard(Some(owner), &shot.pixels, shot.w, shot.h, shot.alpha) {
        errors.push(format!("couldn't copy: {e}"));
    }
    let mut saved = None;
    let mut uploading = false;
    if save || upload {
        let format = match cfg.screenshot_format {
            ShotFormat::Png => ImageFormat::Png,
            ShotFormat::Jpg => ImageFormat::Jpg,
        };
        match output::encode(&shot.pixels, shot.w, shot.h, shot.alpha, format, cfg.screenshot_quality as u8) {
            Ok(bytes) => {
                if save {
                    let path = unique_path(cfg, Kind::Shot, subject, format.extension());
                    match std::fs::write(&path, &bytes) {
                        Ok(()) => {
                            history::add(Kind::Shot, Some(&path), bytes.len() as u64, None, subject.app());
                            saved = Some((path, bytes.len()));
                        }
                        Err(e) => errors.push(format!("couldn't save: {}: {e}", path.display())),
                    }
                }
                if upload {
                    let (path, name) = match &saved {
                        Some((p, _)) => (Some(p.clone()), p.file_name().unwrap_or_default().to_string_lossy().into_owned()),
                        None => (None, file_name(cfg, Kind::Shot, subject, format.extension())),
                    };
                    match upload::start(cfg, Kind::Shot, path, Payload::bytes(name, bytes), owner) {
                        Ok(()) => uploading = true,
                        Err(e) => errors.push(format!("couldn't upload: {e}")),
                    }
                }
            }
            Err(e) => errors.push(format!("couldn't encode: {e}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(Delivered { copied: copy, saved, uploading })
}
