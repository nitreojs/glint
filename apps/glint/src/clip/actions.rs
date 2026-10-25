use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use glint_core::clip::export::Settings;
use glint_core::paths::app_dir;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromRect};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FileSaveDialog, IFileSaveDialog,
    IShellItem, SHCreateItemFromParsingName, SHFILEOPSTRUCTW, SHFileOperationW, SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsIconic, IsZoomed, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNORMAL, SetWindowPos,
};
use windows::core::{HSTRING, PCWSTR, w};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub(super) struct Saved {
    pub rect: Option<[i32; 4]>,
    pub volume: f32,
    pub export: Settings,
    /// The export panel's "more options" are open.
    pub more: bool,
    /// The retired all / unmuted / mix audio choice: muted lanes now decide, and "mix" became `combine`.
    #[serde(rename = "audio", skip_serializing)]
    legacy_audio: Option<String>,
}

impl Default for Saved {
    fn default() -> Self {
        Saved { rect: None, volume: 1.0, export: Settings::default(), more: false, legacy_audio: None }
    }
}

fn saved_path() -> PathBuf {
    app_dir().join("clip.json")
}

impl Saved {
    fn parse(text: &str) -> Saved {
        let mut s: Saved = serde_json::from_str(text).unwrap_or_default();
        if s.legacy_audio.take().as_deref() == Some("mix") {
            s.export.combine = true;
        }
        s.volume = s.volume.clamp(0.0, 1.0);
        s
    }

    pub(super) fn load() -> Saved {
        Saved::parse(&std::fs::read_to_string(saved_path()).unwrap_or_default())
    }

    pub(super) fn store(&self) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(saved_path(), text);
        }
    }
}

pub(super) fn window_rect(hwnd: HWND) -> Option<[i32; 4]> {
    unsafe {
        if IsIconic(hwnd).as_bool() || IsZoomed(hwnd).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        Some([r.left, r.top, r.right - r.left, r.bottom - r.top])
    }
}

pub(super) fn place(hwnd: HWND, [x, y, w, h]: [i32; 4]) {
    let r = RECT { left: x, top: y, right: x + w, bottom: y + h };
    unsafe {
        if !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid() {
            let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

pub(super) fn show_in_folder(path: &Path) {
    glint_shell::explorer::reveal(path);
}

pub(super) fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
}

pub(super) fn recycle(owner: HWND, path: &Path) -> Result<(), String> {
    let from: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        hwnd: owner,
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 if !op.fAnyOperationsAborted.as_bool() => Ok(()),
        0 => Err("the deletion was cancelled".into()),
        code => Err(format!("windows couldn't move it to the recycle bin (error {code:#x})")),
    }
}

pub(super) fn save_dialog(owner: HWND, suggested: &Path, ext: &str) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let (name, spec) = match ext {
            "gif" => (w!("GIF animation"), w!("*.gif")),
            _ => (w!("MP4 video"), w!("*.mp4")),
        };
        dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: name, pszSpec: spec }]).ok()?;
        dialog.SetDefaultExtension(&HSTRING::from(ext)).ok()?;
        if let Some(file) = suggested.file_name() {
            let _ = dialog.SetFileName(&HSTRING::from(file));
        }
        if let Some(dir) = suggested.parent()
            && let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None)
        {
            let _ = dialog.SetFolder(&item);
        }
        dialog.Show(Some(owner)).ok()?;
        let path = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as _));
        s.map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::export::{CodecChoice, Compress, Format};

    #[test]
    fn saved_state_tolerates_missing_and_old_fields() {
        let s = Saved::parse(r#"{"rect":[10,20,800,600]}"#);
        assert_eq!(s, Saved { rect: Some([10, 20, 800, 600]), ..Saved::default() });
        let s = Saved::parse(r#"{"volume":0.5,"audio":"mix","old":1}"#);
        assert_eq!((s.volume, s.export.combine, s.rect), (0.5, true, None));
        assert_eq!(s.legacy_audio, None);
        assert!(!serde_json::to_string(&s).unwrap().contains("audio"));
        assert!(!Saved::parse(r#"{"audio":"unmuted"}"#).export.combine);
        let s = Saved::parse(r#"{"export":{"format":"gif","target_mb":10,"gif_fps":24}}"#);
        assert_eq!((s.export.format, s.export.target_mb, s.export.gif_fps), (Format::Gif, Some(10), 24));
        assert_eq!(Saved::parse("garbage"), Saved::default());
        let s = Saved::parse(r#"{"more":true,"export":{"fps":30,"kbps":3000,"codec":"hevc"}}"#);
        assert!(s.more && !Saved::default().more);
        assert_eq!((s.export.fps, s.export.kbps, s.export.codec), (Some(30), Some(3000), CodecChoice::Hevc));
        assert_eq!(Saved::parse(r#"{"export":{"compress":"strong"}}"#).export.compress, Some(Compress::Strong));
        assert_eq!(Saved::default().export.compress, None);
    }
}
