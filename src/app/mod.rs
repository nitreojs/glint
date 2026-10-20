use std::cell::RefCell;
use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Shell::{NIN_BALLOONUSERCLICK, NIN_SELECT, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::config::Config;
use crate::picker::{self, Picked, Purpose};
use crate::platform::format::{human_duration, human_size};
use crate::platform::log::{log, log_path};
use crate::platform::monitors;
use crate::platform::wide::wide;
use crate::record::indicator::Indicator;
use crate::record::{Outcome, Params, Recorder};
use crate::shot::{self, Delivered, Shot};

mod autostart;
mod hotkeys;
pub(crate) mod ipc;
mod menu;
pub(crate) mod tray;

use hotkeys::{HOTKEY_ID, SHOT_HOTKEY_ID};
use menu::show_menu;
use tray::Tray;

/// NIN_SELECT | NINF_KEY: the icon was activated with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_SELECTION: u32 = WM_APP + 2;
const WM_APP_DONE: u32 = WM_APP + 3;
const WM_APP_QUIT: u32 = WM_APP + 4;
/// Posted by the indicator pill's buttons (see the handler for WPARAM values).
const WM_APP_STOP: u32 = WM_APP + 5;

enum State {
    Idle,
    Selecting(HWND),
    Recording { rec: Recorder, _indicator: Option<Indicator>, size: (i32, i32), fps: f64 },
    Stopping { size: (i32, i32), fps: f64 },
}

/// What clicking the last notification opens.
enum ClickTarget {
    None,
    File(PathBuf),
    Log,
}

struct App {
    hwnd: HWND,
    cfg: Config,
    /// The record and screenshot hotkeys, while registered.
    hotkey: Option<String>,
    shot_hotkey: Option<String>,
    state: State,
    tray: Tray,
    taskbar_created: u32,
    click: ClickTarget,
    exiting: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state; `None` if called re-entrantly while the state is borrowed.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

impl App {
    fn notify(&mut self, title: &str, text: &str, error: bool, click: ClickTarget) {
        self.click = click;
        self.tray.notify(title, text, error);
    }

    /// Re-reads settings; (re-)registers the hotkey when it changed or isn't held.
    fn reload(&mut self) {
        match Config::load() {
            Ok(cfg) => {
                let hotkeys_changed = self.hotkey.as_deref() != Some(cfg.hotkey.as_str())
                    || self.shot_hotkey.as_deref() != Some(cfg.screenshot_hotkey.as_str());
                let startup_changed = self.cfg.launch_at_startup != cfg.launch_at_startup;
                self.cfg = cfg;
                if hotkeys_changed {
                    self.register_hotkeys();
                }
                if startup_changed {
                    self.sync_autostart();
                }
            }
            Err(e) => self.notify("glint: settings error", &e, true, ClickTarget::None),
        }
    }

    fn sync_autostart(&mut self) {
        if let Err(e) = autostart::sync(self.cfg.launch_at_startup) {
            self.notify("glint: launch at startup", &e, true, ClickTarget::None);
        }
    }

    /// A hotkey (or the tray menu) was used: start picking, or stop what's running.
    fn toggle(&mut self, shot: bool) {
        match &self.state {
            State::Idle => self.start_selection(shot),
            State::Selecting(h) => picker::cancel(*h),
            State::Recording { .. } if !shot => self.stop_recording(false),
            State::Recording { .. } | State::Stopping { .. } => {}
        }
    }

    fn start_selection(&mut self, shot: bool) {
        self.reload();
        let outputs = match monitors::enumerate() {
            Ok(o) if !o.is_empty() => o,
            Ok(_) => return self.notify("glint", "No monitors found.", true, ClickTarget::None),
            Err(e) => return self.notify("glint: can't list monitors", &e.message(), true, ClickTarget::None),
        };
        let purpose = if shot { Purpose::Shot } else { Purpose::Record { min: self.cfg.codec.codec().min_size() } };
        match picker::open(self.hwnd, WM_APP_SELECTION, outputs, purpose, self.cfg.freeze_screen) {
            Ok(h) => self.state = State::Selecting(h),
            Err(e) => self.notify("glint: can't open the selection overlay", &e, true, ClickTarget::Log),
        }
    }

    fn on_selection(&mut self, picked: Option<Picked>) {
        let State::Selecting(picker) = std::mem::replace(&mut self.state, State::Idle) else { return };
        let sel = match picked {
            Some(Picked::Region(sel)) if !self.exiting => sel,
            Some(Picked::Shot(shot)) if !self.exiting => {
                picker::dismiss(picker);
                return self.finish_shot(shot);
            }
            _ => return picker::dismiss(picker),
        };
        let out = &sel.output;
        let fps = match self.cfg.fps {
            0 => out.refresh_hz,
            cap => (cap as f64).min(out.refresh_hz),
        };
        let path = self.cfg.unique_path("mp4");
        let params = Params {
            adapter_idx: out.adapter_idx,
            output_idx: out.output_idx,
            region: sel.rect.offset(-out.rect.x, -out.rect.y),
            fps,
            codec: self.cfg.codec.codec(),
            qp: self.cfg.quality,
            effort: self.cfg.effort,
            draw_cursor: self.cfg.draw_cursor,
            path,
        };
        let hwnd = self.hwnd.0 as isize;
        let done = move |res: Result<Outcome, String>| unsafe {
            let b = Box::into_raw(Box::new(res));
            if PostMessageW(Some(HWND(hwnd as _)), WM_APP_DONE, WPARAM(0), LPARAM(b as isize)).is_err() {
                drop(Box::from_raw(b));
            }
        };
        // The pipeline initializes while the picker fades out; capture starts once it's gone.
        let started = Recorder::start(params, done);
        picker::dismiss(picker);
        match started.and_then(|s| s.wait()) {
            Ok(rec) => {
                let indicator = Indicator::show(sel.rect, out.rect, self.hwnd, WM_APP_STOP)
                    .map_err(|e| log(&format!("indicator unavailable: {e}")))
                    .ok();
                let tip = format!("glint: recording {}×{} @ {:.0} fps", sel.rect.w, sel.rect.h, fps);
                self.tray.set_recording(true, &tip);
                self.state = State::Recording { rec, _indicator: indicator, size: (sel.rect.w, sel.rect.h), fps };
            }
            Err(e) => {
                log(&format!("start failed: {e}"));
                self.notify("glint: couldn't start recording", &e, true, ClickTarget::Log);
            }
        }
    }

    fn finish_shot(&mut self, shot: Shot) {
        let size = format!("{}×{}", shot.w, shot.h);
        let Delivered { copied, saved } = match shot::deliver(&shot, &self.cfg, self.hwnd) {
            Ok(delivered) => delivered,
            Err(text) => {
                log(&format!("screenshot: {text}"));
                return self.notify("glint: screenshot failed", &text, true, ClickTarget::Log);
            }
        };
        match saved {
            Some((path, bytes)) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let title = if copied { "Screenshot copied and saved" } else { "Screenshot saved" };
                let text = format!("{name}\n{} · {size}", human_size(bytes as u64));
                self.notify(title, &text, false, ClickTarget::File(path));
            }
            None => self.notify("Screenshot copied", &format!("{size}, ready to paste"), false, ClickTarget::None),
        }
    }

    /// Ends the recording; with `discard` the file is thrown away instead of saved.
    fn stop_recording(&mut self, discard: bool) {
        if let State::Recording { rec, size, fps, .. } = std::mem::replace(&mut self.state, State::Idle) {
            // The indicator fades out as the old state drops.
            if discard {
                rec.abort();
                self.tray.set_recording(true, "glint: discarding…");
            } else {
                rec.stop();
                self.tray.set_recording(true, "glint: saving…");
            }
            self.state = State::Stopping { size, fps };
        }
    }

    fn on_done(&mut self, res: Result<Outcome, String>) {
        let (size, fps) = match &self.state {
            State::Recording { size, fps, .. } | State::Stopping { size, fps } => (*size, *fps),
            _ => ((0, 0), 0.0),
        };
        self.state = State::Idle; // drops the indicator if capture ended on its own
        self.tray.set_recording(false, "glint");
        match res {
            Ok(Outcome::Discarded) => {
                self.notify("Recording discarded", "Nothing was saved.", false, ClickTarget::None);
            }
            Ok(Outcome::Saved(s)) => {
                let bytes = std::fs::metadata(&s.path).map(|m| m.len()).unwrap_or(0);
                let name = s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let mut text = format!(
                    "{name}\n{} · {} · {}×{} @ {:.0} fps",
                    human_size(bytes),
                    human_duration(s.duration_s),
                    size.0,
                    size.1,
                    fps
                );
                if s.dropped > 0 {
                    text += &format!("\n{} frames dropped (lower \"effort\" in settings)", s.dropped);
                }
                if let Some(why) = &s.stopped_early {
                    text += &format!("\nStopped early: {why}");
                }
                self.notify("Recording saved", &text, false, ClickTarget::File(s.path));
            }
            Err(e) => {
                log(&format!("recording failed: {e}"));
                self.notify("glint: recording failed", &e, true, ClickTarget::Log);
            }
        }
        if self.exiting {
            self.close_window();
        }
    }

    /// Exit: finish a running recording first so the file is finalized.
    fn exit(&mut self) {
        self.exiting = true;
        match &self.state {
            State::Recording { .. } => self.stop_recording(false),
            State::Stopping { .. } => {}
            State::Selecting(h) => {
                picker::dismiss(*h);
                self.state = State::Idle;
                self.close_window();
            }
            State::Idle => self.close_window(),
        }
    }

    /// Destroys the main window from the message loop, outside any state borrow.
    fn close_window(&self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_APP_QUIT, WPARAM(0), LPARAM(0));
        }
    }

    fn open_click_target(&self) {
        match &self.click {
            ClickTarget::File(p) => shell("explorer.exe", &format!("/select,\"{}\"", p.display())),
            ClickTarget::Log => shell("notepad.exe", &format!("\"{}\"", log_path().display())),
            ClickTarget::None => {}
        }
    }
}

fn shell(file: &str, params: &str) {
    let f = wide(file);
    let p = wide(params);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(f.as_ptr()), PCWSTR(p.as_ptr()), None, SW_SHOWNORMAL);
    }
}

/// Brings the settings window forward, or starts it (`glint.exe --settings`).
fn open_settings() {
    if let Some(h) = ipc::find_settings() {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let spawned = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--settings").spawn());
    if let Err(e) = spawned {
        with_app(|a| a.notify("glint: can't open settings", &e.to_string(), true, ClickTarget::None));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY if wp.0 as i32 == HOTKEY_ID || wp.0 as i32 == SHOT_HOTKEY_ID => {
            with_app(|a| a.toggle(wp.0 as i32 == SHOT_HOTKEY_ID));
            return LRESULT(0);
        }
        // From the indicator pill: WPARAM 0 = stop and save, 1 = discard.
        WM_APP_STOP => {
            with_app(|a| a.stop_recording(wp.0 == 1));
            return LRESULT(0);
        }
        ipc::WM_RELOAD => {
            with_app(|a| a.reload());
            return LRESULT(0);
        }
        ipc::WM_HOTKEY_PAUSE => {
            with_app(|a| if wp.0 == 1 { a.pause_hotkeys() } else { a.register_hotkeys() });
            return LRESULT(0);
        }
        WM_APP_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT => show_menu(hwnd),
                NIN_BALLOONUSERCLICK => {
                    with_app(|a| a.open_click_target());
                }
                _ => {}
            }
            return LRESULT(0);
        }
        // Results posted from the overlay / encoder thread own a Box in LPARAM. If the state
        // is busy (re-entrant dispatch), re-post instead of dropping the result.
        WM_APP_SELECTION | WM_APP_DONE => {
            let handled = with_app(|a| unsafe {
                if msg == WM_APP_SELECTION {
                    a.on_selection(*Box::from_raw(lp.0 as *mut Option<Picked>));
                } else {
                    a.on_done(*Box::from_raw(lp.0 as *mut Result<Outcome, String>));
                }
            });
            if handled.is_none() {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, wp, lp);
                }
            }
            return LRESULT(0);
        }
        // taskkill / shutdown: finish a running recording before quitting.
        WM_CLOSE => {
            if with_app(|a| a.exit()).is_none() {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), msg, wp, lp);
                }
            }
            return LRESULT(0);
        }
        WM_APP_QUIT => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            APP.with(|a| a.borrow_mut().take()); // removes the tray icon
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        _ => {}
    }
    if with_app(|a| a.taskbar_created != 0 && msg == a.taskbar_created) == Some(true) {
        with_app(|a| a.tray.add());
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _mutex = CreateMutexW(None, false, w!("Local\\glint-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            MessageBoxW(None, w!("glint is already running (see the tray icon)."), w!("glint"), MB_ICONINFORMATION);
            return;
        }
        let _ = std::fs::write(log_path(), ""); // fresh log per run

        let hinst = GetModuleHandleW(None).expect("module handle");
        let class = ipc::MAIN_CLASS;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("glint"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("main window");

        let (cfg, cfg_err) = match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        };
        let mut app = App {
            hwnd,
            cfg,
            hotkey: None,
            shot_hotkey: None,
            state: State::Idle,
            tray: Tray::new(hwnd, WM_APP_TRAY),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            click: ClickTarget::None,
            exiting: false,
        };
        app.register_hotkeys();
        let autostarted = std::env::args().any(|a| a == autostart::ARG);
        match cfg_err {
            Some(e) => app.notify("glint: settings error (using defaults)", &e, true, ClickTarget::None),
            None => {
                app.sync_autostart(); // keeps the entry pointing at this exe
                if app.hotkey.is_some() && !autostarted {
                    let text = format!(
                        "{} records a region, {} takes a screenshot. Tray icon → settings.",
                        app.cfg.hotkey, app.cfg.screenshot_hotkey
                    );
                    app.notify("glint is running", &text, false, ClickTarget::None);
                }
            }
        }
        log(&format!("started, hotkeys record {:?}, screenshot {:?}", app.hotkey, app.shot_hotkey));
        APP.with(|a| *a.borrow_mut() = Some(app));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
