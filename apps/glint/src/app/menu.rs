use glint_core::history::{self, Entry};
use glint_core::wide::wide;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::{ClickTarget, State, open_settings, shell, with_app};
use crate::platform::format::local_time;
use crate::platform::log::log_path;
use crate::platform::paths::output_dir;
use crate::shot;
use crate::upload;

const ID_RECORD: usize = 100;
const ID_SHOT: usize = 105;
const ID_SHOT_ALL: usize = 106;
const ID_FOLDER: usize = 101;
const ID_SETTINGS: usize = 102;
const ID_LOG: usize = 103;
const ID_EXIT: usize = 104;
const ID_PAUSE: usize = 107;
const ID_TRIM: usize = 108;
const ID_UPDATE: usize = 109;
const ID_RECENT: usize = 1000;
const ID_CLEAR_HISTORY: usize = 1199;
const RECENT: usize = 10;
const OPEN: usize = 0;
const OPEN_LINK: usize = 1;
const SHOW: usize = 2;
const COPY_LINK: usize = 3;
const UPLOAD: usize = 4;
const TRIM: usize = 5;
const ACTIONS: usize = 8;

unsafe fn recent_menu(entries: &[Entry]) -> HMENU {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return HMENU::default() };
        for (i, e) in entries.iter().enumerate() {
            let Ok(sub) = CreatePopupMenu() else { continue };
            let id = |action: usize| ID_RECENT + i * ACTIONS + action;
            let exists = e.exists();
            if exists {
                let _ = AppendMenuW(sub, MF_STRING, id(OPEN), w!("Open"));
                let _ = AppendMenuW(sub, MF_STRING, id(SHOW), w!("Show in folder"));
            }
            if e.url.is_some() {
                let _ = AppendMenuW(sub, MF_STRING, id(OPEN_LINK), w!("Open link"));
                let _ = AppendMenuW(sub, MF_STRING, id(COPY_LINK), w!("Copy link"));
            } else if exists {
                let _ = AppendMenuW(sub, MF_STRING, id(UPLOAD), w!("Upload"));
            }
            if exists && e.is_mp4() {
                let _ = AppendMenuW(sub, MF_STRING, id(TRIM), w!("Trim…"));
            }
            let label = wide(&e.label(local_time));
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, PCWSTR(label.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_CLEAR_HISTORY, w!("Clear history"));
        menu
    }
}

fn recent_action(hwnd: HWND, e: &Entry, action: usize) {
    let path = e.path.as_deref();
    let url = e.url.as_deref();
    let failed = |title: &str, text: String| {
        with_app(|a| a.notify(title, &text, true, ClickTarget::None));
    };
    match (action, path, url) {
        (OPEN, Some(p), _) => shell(&p.display().to_string(), ""),
        (SHOW, Some(p), _) => shell("explorer.exe", &format!("/select,\"{}\"", p.display())),
        (OPEN_LINK, _, Some(u)) => shell(u, ""),
        (COPY_LINK, _, Some(u)) => {
            if let Err(e) = shot::copy_text(Some(hwnd), u) {
                failed("glint: couldn't copy the link", e);
            }
        }
        (UPLOAD, Some(p), _) => {
            if let Some(Err(err)) = with_app(|a| upload::start_file(&a.cfg, e.kind, p, a.hwnd)) {
                failed("glint: couldn't upload", err);
            }
        }
        (TRIM, Some(p), _) => {
            if let Err(e) = crate::clip::spawn(p) {
                failed("glint: can't open the trimmer", e.to_string());
            }
        }
        _ => {}
    }
}

pub(super) fn show_menu(hwnd: HWND) {
    let Some((counting, paused, busy, hotkey, shot_hotkey, full_hotkey, pause_hotkey)) = with_app(|a| {
        let paused = match &a.state {
            State::Recording { rec, .. } => Some(rec.paused()),
            _ => None,
        };
        let counting = matches!(a.state, State::Countdown { .. });
        let busy = matches!(a.state, State::Stopping { .. } | State::Selecting(_));
        let keys = (a.hotkey.clone(), a.shot_hotkey.clone(), a.full_hotkey.clone(), a.pause_hotkey.clone());
        (counting, paused, busy, keys.0, keys.1, keys.2, keys.3)
    }) else {
        return;
    };
    let with_key = |text: &str, key: Option<String>| match key {
        Some(k) => format!("{text}\t{k}"),
        None => text.to_string(),
    };
    let recording = counting || paused.is_some();
    let record = match (counting, recording) {
        (true, _) => "Cancel countdown",
        (false, true) => "Stop recording",
        (false, false) => "Record region",
    };
    let label = with_key(record, hotkey);
    let pause_label = paused.map(|p| wide(&with_key(if p { "Resume recording" } else { "Pause recording" }, pause_hotkey)));
    let shot_label = with_key("Take screenshot", shot_hotkey);
    let full_label = with_key("Screenshot all screens", full_hotkey);
    let last = history::last_recording();
    let recent = history::recent(RECENT);
    let update_label = with_app(|a| a.ready_version()).flatten().map(|v| wide(&format!("Restart to update to {v}")));
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let (label, shot_label, full_label) = (wide(&label), wide(&shot_label), wide(&full_label));
        let flags = if busy { MF_STRING | MF_GRAYED } else { MF_STRING };
        let _ = AppendMenuW(menu, flags, ID_RECORD, PCWSTR(label.as_ptr()));
        if let Some(pause_label) = &pause_label {
            let _ = AppendMenuW(menu, MF_STRING, ID_PAUSE, PCWSTR(pause_label.as_ptr()));
        }
        let shot_flags = if busy || recording { MF_STRING | MF_GRAYED } else { MF_STRING };
        let _ = AppendMenuW(menu, shot_flags, ID_SHOT, PCWSTR(shot_label.as_ptr()));
        let _ = AppendMenuW(menu, shot_flags, ID_SHOT_ALL, PCWSTR(full_label.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let trim_flags = if last.is_some() { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, trim_flags, ID_TRIM, w!("Trim last recording"));
        let recent_flags = if recent.is_empty() { MF_POPUP | MF_GRAYED } else { MF_POPUP };
        let _ = AppendMenuW(menu, recent_flags, recent_menu(&recent).0 as usize, w!("Recent"));
        let _ = AppendMenuW(menu, MF_STRING, ID_FOLDER, w!("Open output folder"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LOG, w!("Open log"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        if let Some(label) = &update_label {
            let _ = AppendMenuW(menu, MF_STRING, ID_UPDATE, PCWSTR(label.as_ptr()));
        }
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
        ID_PAUSE => {
            with_app(|a| a.toggle_pause());
        }
        ID_SHOT => {
            with_app(|a| a.toggle(true));
        }
        ID_SHOT_ALL => {
            with_app(|a| a.shot_all_screens());
        }
        ID_TRIM => {
            if let Some(path) = last
                && let Err(e) = crate::clip::spawn(&path)
            {
                with_app(|a| a.notify("glint: can't open the trimmer", &e.to_string(), true, ClickTarget::None));
            }
        }
        ID_FOLDER => {
            if let Some(dir) = with_app(|a| output_dir(&a.cfg)) {
                let _ = std::fs::create_dir_all(&dir);
                shell("explorer.exe", &format!("\"{}\"", dir.display()));
            }
        }
        ID_SETTINGS => open_settings(),
        ID_LOG => shell("notepad.exe", &format!("\"{}\"", log_path().display())),
        ID_CLEAR_HISTORY => history::clear(),
        c if (ID_RECENT..ID_RECENT + RECENT * ACTIONS).contains(&c) => {
            let i = (c - ID_RECENT) / ACTIONS;
            if let Some(e) = recent.get(i) {
                recent_action(hwnd, e, (c - ID_RECENT) % ACTIONS);
            }
        }
        ID_UPDATE => {
            with_app(|a| a.install_update());
        }
        ID_EXIT => {
            with_app(|a| a.exit());
        }
        _ => {}
    }
}
