//! afterglow's tray app: a hidden window holding the tray icon and the save hotkey, and the
//! buffer that runs behind it. Clicking the icon or pressing the hotkey saves the last stretch;
//! the right-click menu pauses and resumes the buffer, opens the folder and the settings file.

use std::cell::RefCell;
use std::path::PathBuf;

use glint_core::format::human_duration;
use glint_core::geom::rect::Rect;
use glint_core::record::replay::Clip;
use glint_core::icon::icon_pixels;
use glint_core::log::log;
use glint_core::record::pipeline::Outcome;
use glint_core::wide::wide;
use glint_shell::tray::{TrayIcon, destroy, icon_size, make_icon};
use glint_shell::{autostart, explorer, hotkey, instance, window};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{NIN_BALLOONUSERCLICK, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::buffer::{self, Buffer, Notify};
use crate::note::{self, Kind};
use crate::config::{APP, Config};
use crate::settings::SETTINGS_ARG;

/// Window class of the hidden main window: glint (or anything) posts [`WM_SAVE_REPLAY`] to it.
pub const MAIN_CLASS: &str = "afterglow_main";
/// The settings window saved: re-read config.toml.
pub const WM_RELOAD: u32 = WM_APP + 20;
/// WPARAM 1: let go of the save hotkey (the settings window is recording a new one); 0: take it back.
pub const WM_HOTKEY_PAUSE: u32 = WM_APP + 21;
/// Save the replay now, from another process.
pub const WM_SAVE_REPLAY: u32 = WM_APP + 30;

const WM_TRAY: u32 = WM_APP + 1;
const WM_SAVED: u32 = WM_APP + 2;
const WM_ENDED: u32 = WM_APP + 3;
const SAVE_HOTKEY: i32 = 1;
const RESTART_TIMER: usize = 1;
const TIP_TIMER: usize = 2;
const CLICK_TIMER: usize = 3;

/// afterglow's accent: the tray icon while buffering, the settings window.
pub const AMBER: [u8; 3] = [0xF5, 0x9E, 0x0B];
const GREY: [u8; 3] = [0x7A, 0x77, 0x72];

const ID_SAVE: usize = 1;
const ID_PAUSE: usize = 2;
const ID_FOLDER: usize = 3;
const ID_SETTINGS: usize = 4;
const ID_LOG: usize = 5;
const ID_QUIT: usize = 6;
const ID_LAST: usize = 7;

struct App {
    hwnd: HWND,
    cfg: Config,
    tray: TrayIcon,
    on: HICON,
    off: HICON,
    buffer: Option<Buffer>,
    /// Counts buffers started, so a late "ended" from one already replaced is ignored.
    generation: u64,
    /// Paused from the menu: no buffer, and none comes back on its own.
    paused: bool,
    /// The session is locked or the PC is going to sleep: nothing to record until it's back.
    away: bool,
    hotkey: bool,
    /// A save is being written: how long the clip is (seconds), and the monitor it's of.
    saving: Option<(f64, Rect)>,
    /// A click on the tray icon cut this clip; it's saved unless the click turns into a double click.
    clicked: Option<(Clip, Rect)>,
    last_saved: Option<PathBuf>,
    taskbar_created: u32,
    /// Restarts in a row that failed, so the error is announced once.
    failures: u32,
}

thread_local! {
    static APP_STATE: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP_STATE.with(|a| a.borrow_mut().as_mut().map(f))
}

impl App {
    fn notify(&self, title: &str, text: &str, error: bool) {
        self.tray.notify(title, text, error);
    }

    /// The icon and tooltip for what the buffer is doing.
    fn refresh_tray(&mut self) {
        let keys = if self.hotkey { format!(" · {} saves", self.cfg.save_hotkey) } else { String::new() };
        let (icon, tip) = match &self.buffer {
            Some(b) => {
                let held = b.held().min(self.cfg.length as f64);
                (self.on, format!("afterglow\nbuffering {} of {}{keys}", human_duration(held), human_duration(self.cfg.length as f64)))
            }
            None if self.paused => (self.off, "afterglow\npaused".to_string()),
            None if self.away => (self.off, "afterglow\nwaiting for you to come back".to_string()),
            None => (self.off, "afterglow\nstarting…".to_string()),
        };
        self.tray.show(icon, &tip);
    }

    fn start(&mut self) {
        if self.buffer.is_some() || self.paused || self.away {
            return;
        }
        self.generation += 1;
        match Buffer::start(&self.cfg, self.generation, Notify::new(self.hwnd, WM_ENDED)) {
            Ok(b) => {
                log(&format!("buffering the last {} s", self.cfg.length));
                self.buffer = Some(b);
                self.failures = 0;
            }
            Err(e) => {
                log(&format!("can't start the buffer: {e}"));
                if self.failures == 0 {
                    self.notify("afterglow: can't buffer", &e, true);
                }
                self.failures += 1;
                self.retry_later();
            }
        }
        self.refresh_tray();
    }

    fn retry_later(&self) {
        unsafe {
            SetTimer(Some(self.hwnd), RESTART_TIMER, 3000, None);
        }
    }

    fn stop(&mut self) {
        if let Some(b) = self.buffer.take() {
            b.stop();
        }
    }

    /// The last stretch of the buffer, cut now, and the monitor it's of.
    fn cut(&self) -> Option<(Clip, Rect)> {
        let b = self.buffer.as_ref()?;
        Some((b.clip()?, b.monitor()))
    }

    fn save(&mut self) {
        match self.cut() {
            Some(cut) => self.write(cut),
            None => self.nothing_yet(),
        }
    }

    fn nothing_yet(&self) {
        let kept = self.buffer.as_ref().map(Buffer::monitor);
        let monitor = kept.or_else(|| buffer::monitor(self.cfg.monitor).ok().map(|o| o.rect));
        if let Some(m) = monitor {
            note::show(m, Kind::Failed, "nothing to save yet", "the buffer is still empty");
        }
    }

    fn write(&mut self, (clip, monitor): (Clip, Rect)) {
        if self.saving.is_some() {
            return;
        }
        let secs = clip.duration as f64 / 1e7;
        log(&format!("saving {secs:.1} s"));
        self.saving = Some((secs, monitor));
        note::show(monitor, Kind::Saving, &format!("saving the last {}", human_duration(secs)), APP);
        buffer::save(clip, self.cfg.output_dir(), Notify::new(self.hwnd, WM_SAVED));
    }

    fn saved(&mut self, result: Result<PathBuf, String>) {
        let Some((secs, monitor)) = self.saving.take() else { return };
        match result {
            Ok(path) => {
                log(&format!("saved {}", path.display()));
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                note::show(monitor, Kind::Saved, &format!("saved the last {}", human_duration(secs)), &name);
                self.last_saved = Some(path);
            }
            Err(e) => {
                log(&format!("save failed: {e}"));
                note::show(monitor, Kind::Failed, "couldn't save", "see the notification for why");
                self.notify("afterglow: couldn't save", &e, true);
            }
        }
    }

    /// Locked, asleep or back: a locked screen or a sleeping PC has nothing to keep, and capture
    /// can't see the lock screen anyway.
    fn set_away(&mut self, away: bool) {
        if self.away == away {
            return;
        }
        self.away = away;
        log(if away { "away: buffer stopped" } else { "back: buffering again" });
        if away {
            self.stop();
        } else {
            self.start();
        }
        self.refresh_tray();
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused {
            self.stop();
        } else {
            self.start();
        }
        self.refresh_tray();
    }

    fn grab_hotkey(&mut self) {
        if self.hotkey {
            hotkey::unregister(self.hwnd, SAVE_HOTKEY);
        }
        self.hotkey = hotkey::register(self.hwnd, SAVE_HOTKEY, &self.cfg.save_hotkey);
        if !self.hotkey {
            log(&format!("save hotkey {} is taken by another app", self.cfg.save_hotkey));
            let text = format!("{} is already used by another app; the tray icon still saves.", self.cfg.save_hotkey);
            self.notify("afterglow: hotkey unavailable", &text, true);
        }
    }

    /// Applies saved settings. What the recording itself is made of (monitor, frame rate, codec,
    /// quality, sound) takes a new one; a new length or memory cap applies to the buffer as it is,
    /// and the rest keeps what's buffered.
    fn reload(&mut self) {
        match Config::load() {
            Ok(cfg) => {
                let recording = |c: &Config| (c.monitor, c.fps, c.codec, c.bitrate, c.quality, c.audio_sources.clone());
                let restart = recording(&cfg) != recording(&self.cfg);
                let hotkey = cfg.save_hotkey != self.cfg.save_hotkey;
                self.cfg = cfg;
                if hotkey || !self.hotkey {
                    self.grab_hotkey();
                }
                self.sync_autostart();
                if restart && self.buffer.is_some() {
                    log("settings changed the recording: starting a new buffer");
                    self.stop();
                    self.start();
                } else if let Some(b) = &mut self.buffer {
                    log("settings applied; the buffer keeps what it holds");
                    b.set_limits(&self.cfg);
                }
                self.refresh_tray();
            }
            Err(e) => self.notify("afterglow: settings error", &e, true),
        }
    }

    fn sync_autostart(&self) {
        if let Err(e) = autostart::sync(APP, self.cfg.launch_at_startup) {
            self.notify("afterglow: launch at startup", &e, true);
        }
    }

    /// The menu's labels: save (and whether it can), pause or resume.
    /// The menu's labels and states: save (and whether it can), pause or resume, and whether a
    /// clip was saved since launch.
    fn menu_items(&self) -> (String, bool, &'static str, bool) {
        let save = if self.hotkey { format!("Save replay\t{}", self.cfg.save_hotkey) } else { "Save replay".into() };
        let pause = if self.paused { "Resume buffer" } else { "Pause buffer" };
        (save, self.buffer.is_some() && self.saving.is_none(), pause, self.last_saved.is_some())
    }

    fn command(&mut self, cmd: usize) {
        match cmd {
            ID_SAVE => self.save(),
            ID_PAUSE => self.set_paused(!self.paused),
            ID_FOLDER => {
                let dir = self.cfg.output_dir();
                let _ = std::fs::create_dir_all(&dir);
                open(&dir.display().to_string(), None);
            }
            ID_SETTINGS => open_settings(),
            ID_LAST => {
                if let Some(p) = &self.last_saved {
                    explorer::reveal(p);
                }
            }
            ID_LOG => open("notepad.exe", Some(&format!("\"{}\"", glint_shell::log::path().display()))),
            ID_QUIT => unsafe {
                let _ = DestroyWindow(self.hwnd);
            },
            _ => {}
        }
    }

    /// The recording behind the buffer stopped on its own (a display change, an error): a new one
    /// starts shortly, unless paused.
    fn ended(&mut self, generation: u64, outcome: Result<Outcome, String>) {
        match &outcome {
            Ok(Outcome::Saved(s)) => log(&format!("buffer stopped: {}", s.stopped_early.as_deref().unwrap_or("done"))),
            Ok(Outcome::Discarded) => log("buffer discarded"),
            Err(e) => log(&format!("buffer failed: {e}")),
        }
        // one we stopped is gone already, or replaced by a newer one
        if generation == self.generation && self.buffer.take().is_some() && !self.paused {
            self.retry_later();
        }
        self.refresh_tray();
    }
}

/// The tray menu, run outside the app's borrow: the menu's own message loop dispatches timers.
fn menu(hwnd: HWND) {
    let Some((save, can_save, pause, has_last)) = with_app(|a| a.menu_items()) else { return };
    let (save, pause) = (wide(&save), wide(pause));
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let grayed = if can_save { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, grayed, ID_SAVE, PCWSTR(save.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_PAUSE, PCWSTR(pause.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let last = if has_last { MF_STRING } else { MF_STRING | MF_GRAYED };
        let _ = AppendMenuW(menu, last, ID_LAST, w!("Show last clip"));
        let _ = AppendMenuW(menu, MF_STRING, ID_FOLDER, w!("Open folder"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LOG, w!("Open log"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
        let cmd = TrackPopupMenu(menu, flags, pt.x, pt.y, Some(0), hwnd, None).0 as usize;
        let _ = DestroyMenu(menu);
        cmd
    };
    with_app(|a| a.command(cmd));
}

fn open_settings() {
    if let Ok(exe) = std::env::current_exe() {
        open(&exe.display().to_string(), Some(SETTINGS_ARG));
    }
}

fn open(target: &str, args: Option<&str>) {
    let (t, a) = (wide(target), args.map(wide));
    unsafe {
        let args = a.as_ref().map_or(PCWSTR::null(), |a| PCWSTR(a.as_ptr()));
        ShellExecuteW(None, w!("open"), PCWSTR(t.as_ptr()), args, None, SW_SHOWNORMAL);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                // a click cuts the clip at once, then waits to see whether it's a double click
                WM_LBUTTONDOWN => with_app(|a| {
                    match a.cut() {
                        Some(cut) => a.clicked = Some(cut),
                        None => a.nothing_yet(),
                    }
                    unsafe {
                        SetTimer(Some(hwnd), CLICK_TIMER, GetDoubleClickTime(), None);
                    }
                }),
                WM_LBUTTONDBLCLK => with_app(|a| {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), CLICK_TIMER);
                    }
                    a.clicked = None;
                    open_settings();
                }),
                WM_CONTEXTMENU => {
                    menu(hwnd);
                    None
                }
                NIN_BALLOONUSERCLICK => with_app(|a| {
                    if let Some(p) = &a.last_saved {
                        explorer::reveal(p);
                    }
                }),
                _ => None,
            };
            return LRESULT(0);
        }
        WM_HOTKEY if wp.0 as i32 == SAVE_HOTKEY => {
            with_app(App::save);
            return LRESULT(0);
        }
        // WTS_SESSION_LOCK / WTS_SESSION_UNLOCK
        WM_WTSSESSION_CHANGE if wp.0 == 7 || wp.0 == 8 => {
            with_app(|a| a.set_away(wp.0 == 7));
            return LRESULT(0);
        }
        WM_POWERBROADCAST if wp.0 == PBT_APMSUSPEND as usize || wp.0 == PBT_APMRESUMEAUTOMATIC as usize => {
            with_app(|a| a.set_away(wp.0 == PBT_APMSUSPEND as usize));
            return LRESULT(1);
        }
        WM_RELOAD => {
            with_app(App::reload);
            return LRESULT(0);
        }
        WM_HOTKEY_PAUSE => {
            with_app(|a| {
                if wp.0 == 1 {
                    if a.hotkey {
                        hotkey::unregister(a.hwnd, SAVE_HOTKEY);
                        a.hotkey = false;
                    }
                } else if !a.hotkey {
                    a.grab_hotkey();
                }
            });
            return LRESULT(0);
        }
        WM_SAVE_REPLAY => {
            with_app(App::save);
            return LRESULT(0);
        }
        WM_SAVED => {
            let result = unsafe { buffer::take::<Result<PathBuf, String>>(lp) };
            with_app(|a| a.saved(result));
            return LRESULT(0);
        }
        WM_ENDED => {
            let (generation, outcome) = unsafe { buffer::take::<(u64, Result<Outcome, String>)>(lp) };
            with_app(|a| a.ended(generation, outcome));
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == RESTART_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), RESTART_TIMER);
            }
            with_app(App::start);
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == CLICK_TIMER => {
            unsafe {
                let _ = KillTimer(Some(hwnd), CLICK_TIMER);
            }
            with_app(|a| {
                if let Some(cut) = a.clicked.take() {
                    a.write(cut);
                }
            });
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == TIP_TIMER => {
            with_app(App::refresh_tray);
            return LRESULT(0);
        }
        WM_DESTROY => {
            APP_STATE.with(|a| {
                if let Some(app) = a.borrow_mut().take() {
                    if let Some(b) = &app.buffer {
                        b.stop();
                    }
                    destroy(app.on);
                    destroy(app.off);
                }
            });
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
    }
    if !instance::take(APP, false) {
        unsafe {
            MessageBoxW(None, w!("afterglow is already running (see the tray icon)."), w!("afterglow"), MB_ICONINFORMATION);
        }
        return;
    }
    let _ = std::fs::write(glint_shell::log::path(), "");
    let (cfg, cfg_err) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e)),
    };
    let hwnd = window::hidden(MAIN_CLASS, "afterglow", Some(wndproc)).expect("main window");
    unsafe {
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
    }
    let size = icon_size();
    let (on, off) = (make_icon(size, &icon_pixels(size, true, AMBER)), make_icon(size, &icon_pixels(size, false, GREY)));
    let tray = TrayIcon::new(hwnd, WM_TRAY, off, "afterglow\nstarting…");
    let paused = !cfg.buffer;
    let mut app = App {
        hwnd,
        cfg,
        tray,
        on,
        off,
        buffer: None,
        generation: 0,
        paused,
        away: false,
        hotkey: false,
        saving: None,
        clicked: None,
        last_saved: None,
        taskbar_created: window::taskbar_created(),
        failures: 0,
    };
    app.grab_hotkey();
    match cfg_err {
        Some(e) => app.notify("afterglow: settings error (using defaults)", &e, true),
        None => app.sync_autostart(),
    }
    app.start();
    if app.buffer.is_some() && std::env::args().all(|a| a != autostart::ARG) {
        let text = format!(
            "keeping the last {}. {} or a click on the icon saves it.",
            human_duration(app.cfg.length as f64),
            app.cfg.save_hotkey
        );
        app.notify("afterglow is running", &text, false);
    }
    unsafe {
        SetTimer(Some(hwnd), TIP_TIMER, 1000, None);
    }
    APP_STATE.with(|a| *a.borrow_mut() = Some(app));
    window::run();
}
