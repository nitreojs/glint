#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::path::PathBuf;

use glint::indicator::Indicator;
use glint::config::{Config, parse_hotkey};
use glint::overlay::{self, Selection};
use glint::recorder::{Outcome, Params, Recorder};
use glint::tray::Tray;
use glint::util::{date_time, human_duration, human_size, log, log_path, wide};
use glint::{autostart, ipc, monitors};

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{NIN_BALLOONUSERCLICK, NIN_SELECT, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// NIN_SELECT | NINF_KEY: the icon was activated with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_SELECTION: u32 = WM_APP + 2;
const WM_APP_DONE: u32 = WM_APP + 3;
const WM_APP_QUIT: u32 = WM_APP + 4;
/// Posted by the indicator pill's buttons (see the handler for WPARAM values).
const WM_APP_STOP: u32 = WM_APP + 5;
const HOTKEY_ID: i32 = 1;

const ID_RECORD: usize = 100;
const ID_FOLDER: usize = 101;
const ID_SETTINGS: usize = 102;
const ID_LOG: usize = 103;
const ID_EXIT: usize = 104;

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
    hotkey: Option<String>,
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

    fn register_hotkey(&mut self) {
        unsafe {
            if self.hotkey.take().is_some() {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
            }
            let (mods, vk) = parse_hotkey(&self.cfg.hotkey).expect("validated in Config::load");
            let mods = HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT;
            if RegisterHotKey(Some(self.hwnd), HOTKEY_ID, mods, vk).is_ok() {
                self.hotkey = Some(self.cfg.hotkey.clone());
            } else {
                let text = format!(
                    "{} is already used by another app (ShareX?). Pick another in Settings, or use the tray menu.",
                    self.cfg.hotkey
                );
                self.notify("glint: hotkey unavailable", &text, true, ClickTarget::None);
            }
        }
    }

    /// Releases the hotkey while the settings window records a new one.
    fn pause_hotkey(&mut self) {
        if self.hotkey.take().is_some() {
            unsafe {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_ID);
            }
        }
    }

    /// Re-reads settings; (re-)registers the hotkey when it changed or isn't held.
    fn reload(&mut self) {
        match Config::load() {
            Ok(cfg) => {
                let hotkey_changed = self.hotkey.as_deref() != Some(cfg.hotkey.as_str());
                let startup_changed = self.cfg.launch_at_startup != cfg.launch_at_startup;
                self.cfg = cfg;
                if hotkey_changed {
                    self.register_hotkey();
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

    fn toggle(&mut self) {
        match &self.state {
            State::Idle => self.start_selection(),
            State::Selecting(h) => overlay::cancel(*h),
            State::Recording { .. } => self.stop_recording(false),
            State::Stopping { .. } => {}
        }
    }

    fn start_selection(&mut self) {
        self.reload();
        let outputs = match monitors::enumerate() {
            Ok(o) if !o.is_empty() => o,
            Ok(_) => return self.notify("glint", "No monitors found.", true, ClickTarget::None),
            Err(e) => return self.notify("glint: can't list monitors", &e.message(), true, ClickTarget::None),
        };
        match overlay::open(self.hwnd, WM_APP_SELECTION, outputs, self.cfg.codec.codec().min_size()) {
            Ok(h) => self.state = State::Selecting(h),
            Err(e) => self.notify("glint: can't open the selection overlay", &e, true, ClickTarget::Log),
        }
    }

    fn on_selection(&mut self, sel: Option<Selection>) {
        let State::Selecting(picker) = std::mem::replace(&mut self.state, State::Idle) else { return };
        let sel = match sel {
            Some(sel) if !self.exiting => sel,
            _ => return overlay::dismiss(picker),
        };
        let out = &sel.output;
        let fps = match self.cfg.fps {
            0 => out.refresh_hz,
            cap => (cap as f64).min(out.refresh_hz),
        };
        let dir = self.cfg.output_dir();
        let (date, time) = date_time();
        let stem = self.cfg.file_stem(&date, &time);
        let mut path = dir.join(format!("{stem}.mp4"));
        for n in 2.. {
            if !path.exists() {
                break;
            }
            path = dir.join(format!("{stem}_{n}.mp4"));
        }
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
        overlay::dismiss(picker);
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
                overlay::dismiss(*h);
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

fn show_menu(hwnd: HWND) {
    let Some((recording, busy, hotkey)) = with_app(|a| {
        let recording = matches!(a.state, State::Recording { .. });
        let busy = matches!(a.state, State::Stopping { .. } | State::Selecting(_));
        (recording, busy, a.hotkey.clone())
    }) else {
        return;
    };
    let label = match (recording, hotkey) {
        (true, Some(h)) => format!("Stop recording\t{h}"),
        (true, None) => "Stop recording".to_string(),
        (false, Some(h)) => format!("Record region\t{h}"),
        (false, None) => "Record region".to_string(),
    };
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let label = wide(&label);
        let flags = if busy { MF_STRING | MF_GRAYED } else { MF_STRING };
        let _ = AppendMenuW(menu, flags, ID_RECORD, PCWSTR(label.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_FOLDER, w!("Open recordings folder"));
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
            with_app(|a| a.toggle());
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

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY if wp.0 as i32 == HOTKEY_ID => {
            with_app(|a| a.toggle());
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
            with_app(|a| if wp.0 == 1 { a.pause_hotkey() } else { a.register_hotkey() });
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
                    a.on_selection(*Box::from_raw(lp.0 as *mut Option<Selection>));
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

fn main() {
    if std::env::args().any(|a| a == glint::settings::CAPTURE_ARG) {
        glint::settings::capture_keys();
        return;
    }
    if std::env::args().any(|a| a == "--settings") {
        glint::settings::run();
        return;
    }
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
            state: State::Idle,
            tray: Tray::new(hwnd, WM_APP_TRAY),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            click: ClickTarget::None,
            exiting: false,
        };
        app.register_hotkey();
        let autostarted = std::env::args().any(|a| a == autostart::ARG);
        match cfg_err {
            Some(e) => app.notify("glint: settings error (using defaults)", &e, true, ClickTarget::None),
            None => {
                app.sync_autostart(); // keeps the entry pointing at this exe
                if app.hotkey.is_some() && !autostarted {
                    let text = format!("Press {} to record a region. Tray icon → settings.", app.cfg.hotkey);
                    app.notify("glint is running", &text, false, ClickTarget::None);
                }
            }
        }
        log(&format!("started, hotkey {:?}", app.hotkey));
        APP.with(|a| *a.borrow_mut() = Some(app));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
