use std::path::PathBuf;

use windows::Win32::Foundation::HWND;

use crate::config::{Config, ShotFormat};

pub(crate) mod editor;
mod output;

use editor::Output;
use output::ImageFormat;

/// A finished screenshot as 0x00RRGGBB rows, and what to do with it.
pub(crate) struct Shot {
    pub pixels: Vec<u32>,
    pub w: u32,
    pub h: u32,
    pub output: Output,
}

pub(crate) struct Delivered {
    pub copied: bool,
    pub saved: Option<(PathBuf, usize)>,
}

/// Copies and / or saves a finished screenshot, as the editor's button or the settings say.
pub(crate) fn deliver(shot: &Shot, cfg: &Config, owner: HWND) -> Result<Delivered, String> {
    let (copy, save) = match shot.output {
        // With both switched off in the settings, Enter still has to do something.
        Output::Default if !cfg.screenshot_copy && !cfg.screenshot_save => (true, false),
        Output::Default => (cfg.screenshot_copy, cfg.screenshot_save),
        Output::Copy => (true, false),
        Output::Save => (false, true),
    };
    let mut errors = Vec::new();
    if copy && let Err(e) = output::copy_to_clipboard(Some(owner), &shot.pixels, shot.w, shot.h) {
        errors.push(format!("couldn't copy: {e}"));
    }
    let mut saved = None;
    if save {
        let format = match cfg.screenshot_format {
            ShotFormat::Png => ImageFormat::Png,
            ShotFormat::Jpg => ImageFormat::Jpg,
        };
        let path = cfg.unique_path(format.extension());
        let written = output::encode(&shot.pixels, shot.w, shot.h, format, cfg.screenshot_quality as u8)
            .and_then(|bytes| {
                let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
                std::fs::write(&path, &bytes).map(|_| bytes.len()).map_err(|e| format!("{}: {e}", path.display()))
            });
        match written {
            Ok(bytes) => saved = Some((path, bytes)),
            Err(e) => errors.push(format!("couldn't save: {e}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    Ok(Delivered { copied: copy, saved })
}
