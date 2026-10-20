use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::{State, open_settings, shell, with_app};
use crate::platform::log::log_path;
use crate::platform::wide::wide;

const ID_RECORD: usize = 100;
const ID_SHOT: usize = 105;
const ID_FOLDER: usize = 101;
const ID_SETTINGS: usize = 102;
const ID_LOG: usize = 103;
const ID_EXIT: usize = 104;

pub(super) fn show_menu(hwnd: HWND) {
    let Some((recording, busy, hotkey, shot_hotkey)) = with_app(|a| {
        let recording = matches!(a.state, State::Recording { .. });
        let busy = matches!(a.state, State::Stopping { .. } | State::Selecting(_));
        (recording, busy, a.hotkey.clone(), a.shot_hotkey.clone())
    }) else {
        return;
    };
    let with_key = |text: &str, key: Option<String>| match key {
        Some(k) => format!("{text}\t{k}"),
        None => text.to_string(),
    };
    let label = with_key(if recording { "Stop recording" } else { "Record region" }, hotkey);
    let shot_label = with_key("Take screenshot", shot_hotkey);
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let (label, shot_label) = (wide(&label), wide(&shot_label));
        let flags = if busy { MF_STRING | MF_GRAYED } else { MF_STRING };
        let _ = AppendMenuW(menu, flags, ID_RECORD, PCWSTR(label.as_ptr()));
        let shot_flags = if busy || recording { MF_STRING | MF_GRAYED } else { MF_STRING };
        let _ = AppendMenuW(menu, shot_flags, ID_SHOT, PCWSTR(shot_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_FOLDER, w!("Open output folder"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LOG, w!("Open log"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    };
    match cmd {
        ID_RECORD => {
            with_app(|a| a.toggle(false));
        }
        ID_SHOT => {
            with_app(|a| a.toggle(true));
        }
        ID_FOLDER => {
            if let Some(dir) = with_app(|a| a.cfg.output_dir()) {
                let _ = std::fs::create_dir_all(&dir);
                shell("explorer.exe", &format!("\"{}\"", dir.display()));
            }
        }
        ID_SETTINGS => open_settings(),
        ID_LOG => shell("notepad.exe", &format!("\"{}\"", log_path().display())),
        ID_EXIT => {
            with_app(|a| a.exit());
        }
        _ => {}
    }
}
