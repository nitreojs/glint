//! afterglow's tray app: a hidden window holding the tray icon and the save hotkey, and the
//! buffer that runs behind it. Clicking the icon or pressing the hotkey saves the last stretch,
//! a double click opens the gallery; the right-click menu pauses and resumes the buffer, opens
//! the gallery, the folder and the settings. Updates come as glint's do
//! ([`glint_shell::update`]): checked soon after start and hourly, a dot on the icon and
//! "restart to update" in the menu once one is downloaded.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use glint_core::format::human_duration;
use glint_core::geom::rect::Rect;
use glint_core::record::replay::Clip;
use glint_core::config::UpdateMode;
use glint_core::icon::{add_badge, icon_pixels};
use glint_core::history::Kind as HistoryKind;
use glint_core::log::log;
use glint_core::naming::{Naming, Source, Subject};
use glint_core::record::pipeline::Outcome;
use glint_core::update::{AFTERGLOW, notes_url};
use glint_core::upload::proxy::Setting;
use glint_core::wide::wide;
use glint_shell::afterglow::{MAIN_CLASS, WM_NOTE_CAPTURABLE};
use glint_shell::identity::{front_window, identify};
use glint_shell::instance::{self, Again};
use glint_shell::menu::{Glyph, Header, Item, Menu};
use glint_shell::time::date_time;
use glint_shell::tray::{TrayIcon, destroy, icon_size, make_icon};
use glint_shell::start_menu::{self, Shortcut};
use glint_shell::update::{self, Build, Checked, Click, Host, Updates};
use glint_shell::{autostart, explorer, hotkey, window};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{NIN_BALLOONUSERCLICK, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::buffer::{self, Buffer, Notify};
use crate::gallery;
use crate::note::{self, Kind};
use crate::config::{APP, Config, NAMES, Place};
use crate::settings::SETTINGS_ARG;

/// The settings window saved: re-read config.toml.
pub const WM_RELOAD: u32 = WM_APP + 20;
/// WPARAM 1: let go of the save hotkey (the settings window is recording a new one); 0: take it back.
pub const WM_HOTKEY_PAUSE: u32 = WM_APP + 21;
/// Save the replay now, from another process.
pub const WM_SAVE_REPLAY: u32 = WM_APP + 30;
/// The settings window's note picker is hovered: show a sample at `Place::ALL[WPARAM]`, or with
/// any other WPARAM take the sample away.
pub const WM_PREVIEW_NOTE: u32 = WM_APP + 22;

const WM_TRAY: u32 = WM_APP + 1;
const WM_SAVED: u32 = WM_APP + 2;
const WM_ENDED: u32 = WM_APP + 3;
const SAVE_HOTKEY: i32 = 1;
const RESTART_TIMER: usize = 1;
const TIP_TIMER: usize = 2;
const CLICK_TIMER: usize = 3;

/// This build of afterglow, as the updater knows it.
pub const BUILD: Build = Build { product: AFTERGLOW, version: env!("CARGO_PKG_VERSION") };

/// afterglow's accent: the tray icon while buffering, the settings window.
/// A save that hasn't answered in this long is given up on, so it can't hold back the next one.
const STUCK: Duration = Duration::from_secs(90);

/// A save being written: how long the clip is ("0:42 of 1:00" when the memory cap cut it short),
/// the monitor it's of, whether the cap cut it, the app it shows, how long it plays in
/// milliseconds, and since when.
struct Saving {
    id: u64,
    long: String,
    monitor: Rect,
    capped: bool,
    app: Option<Source>,
    ms: u32,
    since: Instant,
}

/// A clip cut from the buffer, the monitor it shows, its file name below the output folder and
/// the app in front.
struct Cut {
    clip: Clip,
    monitor: Rect,
    name: PathBuf,
    app: Option<Source>,
}

/// The app whose window is in front, when that window is on `monitor`; None for the desktop
/// and the taskbar, which name clips after the screen.
fn front_app(monitor: Rect) -> Option<Source> {
    let (hwnd, r) = front_window()?;
    monitor.contains(r.x + r.w / 2, r.y + r.h / 2).then(|| identify(hwnd)).flatten()
}

pub const AMBER: [u8; 3] = glint_core::gallery::source::AMBER;
const GREY: [u8; 3] = [0x7A, 0x77, 0x72];

const ID_SAVE: usize = 1;
const ID_PAUSE: usize = 2;
const ID_FOLDER: usize = 3;
const ID_SETTINGS: usize = 4;
const ID_LOG: usize = 5;
const ID_QUIT: usize = 6;
const ID_LAST: usize = 7;
const ID_GALLERY: usize = 8;
const ID_UPDATE: usize = 9;

/// What clicking the last notification does.
enum Balloon {
    /// Shows the last clip saved, if there is one.
    LastClip,
    Update(Click),
    /// Opens a page: what's new after an update.
    Link(String),
}

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
    /// The save being written, if one is.
    saving: Option<Saving>,
    /// Numbers saves, so a late answer from one given up on isn't taken for the next one's.
    saves: u64,
    /// The memory cap cutting a clip short was explained once already.
    told_cap: bool,
    /// Notices about how the buffer is recorded (the software encoder) told since start.
    told: Vec<String>,
    /// The note up is the settings window's sample, so the picker may take it away.
    sample: bool,
    /// A click on the tray icon cut this clip (or found nothing to cut); it's saved (or says so)
    /// unless the click turns into a double click.
    clicked: Option<Option<Cut>>,
    last_saved: Option<PathBuf>,
    taskbar_created: u32,
    /// Restarts in a row that failed, so the error is announced once.
    failures: u32,
    balloon: Balloon,
    update: Updates,
    /// A downloaded update waits: the icon has a dot and the tooltip says so.
    update_ready: bool,
    /// Quitting to restart into an update.
    exiting: bool,
}

thread_local! {
    static APP_STATE: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP_STATE.with(|a| a.borrow_mut().as_mut().map(f))
}

impl App {
    fn notify(&mut self, title: &str, text: &str, error: bool) {
        self.balloon = Balloon::LastClip;
        self.tray.notify(title, text, error);
    }

    /// A notification click.
    fn balloon_clicked(&mut self) {
        match &self.balloon {
            Balloon::LastClip => {
                if let Some(p) = &self.last_saved {
                    explorer::reveal(p);
                }
            }
            Balloon::Update(Click::Install) => self.install_update(),
            Balloon::Update(Click::Log) => open("notepad.exe", Some(&format!("\"{}\"", glint_shell::log::path().display()))),
            Balloon::Link(url) => open(url, None),
        }
    }

    /// The icons with the update dot while one waits, the dot-less ones otherwise.
    fn draw_icons(&mut self) {
        let size = icon_size();
        let icon = |on: bool, color: [u8; 3]| {
            let mut px = icon_pixels(size, on, color);
            if self.update_ready {
                add_badge(&mut px, size, AMBER);
            }
            make_icon(size, &px)
        };
        let (on, off) = (icon(true, AMBER), icon(false, GREY));
        let old = [std::mem::replace(&mut self.on, on), std::mem::replace(&mut self.off, off)];
        self.refresh_tray();
        old.into_iter().for_each(destroy);
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
        let tip = if self.update_ready { tip.replacen('\n', " (update ready)\n", 1) } else { tip };
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
                for notice in b.notices() {
                    log(notice);
                    if !self.told.contains(notice) {
                        self.notify("afterglow", notice, false);
                        self.told.push(notice.clone());
                    }
                }
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

    /// The last stretch of the buffer, cut now, named after now and the app in front.
    fn cut(&self) -> Option<Cut> {
        let b = self.buffer.as_ref()?;
        let (clip, monitor) = (b.clip()?, b.monitor());
        let front = front_app(monitor);
        let subject = front.as_ref().map_or(Subject::Screen, Subject::Window);
        let (date, time) = date_time();
        let naming = Naming { date: &date, time: &time, kind: HistoryKind::Recording, subject };
        let name = NAMES.expand(&self.cfg.file_name, &naming);
        Some(Cut { clip, monitor, name, app: front.filter(|s| !s.app.trim().is_empty()) })
    }

    fn save(&mut self) {
        match self.cut() {
            Some(cut) => self.write(cut),
            None => self.nothing_yet(),
        }
    }

    /// The monitor being recorded, or the one that will be.
    fn recorded_monitor(&self) -> Option<Rect> {
        let kept = self.buffer.as_ref().map(Buffer::monitor);
        kept.or_else(|| buffer::monitor(self.cfg.monitor).ok().map(|o| o.rect))
    }

    fn preview_note(&mut self, place: Place) {
        if let Some(m) = self.recorded_monitor() {
            note::show(m, place, Kind::Info, "notes show here", "like this one, when a clip is saved");
            self.sample = true;
        }
    }

    fn hide_sample(&mut self) {
        if std::mem::take(&mut self.sample) {
            note::hide();
        }
    }

    fn nothing_yet(&mut self) {
        self.sample = false;
        if let Some(m) = self.recorded_monitor() {
            note::show(m, self.cfg.note_position, Kind::Warning, "nothing to save yet", "the buffer is still empty");
        }
    }

    /// A save is being written, and not for so long that it's given up on.
    fn save_running(&self) -> bool {
        self.saving.as_ref().is_some_and(|s| s.since.elapsed() < STUCK)
    }

    fn write(&mut self, Cut { clip, monitor, name, app }: Cut) {
        if let Some(s) = &self.saving {
            if s.since.elapsed() < STUCK {
                log("save asked for while one is still being written");
                note::show(monitor, self.cfg.note_position, Kind::Warning, "still saving", "the clip before this one");
                return;
            }
            log(&format!("save #{} never finished ({:.0} s): saving anyway", s.id, s.since.elapsed().as_secs_f64()));
        }
        let secs = clip.duration as f64 / 1e7;
        self.sample = false;
        self.saves += 1;
        let id = self.saves;
        let bytes: usize = clip.packets.iter().map(|(_, p)| p.data.len()).sum();
        log(&format!("save #{id}: {secs:.1} s, {} MB", bytes >> 20));
        let mut long = human_duration(secs);
        if clip.capped {
            let length = human_duration(self.cfg.length as f64);
            log(&format!("the memory cap ({} MB) held {secs:.1} s of {} s", self.cfg.max_memory_mb, self.cfg.length));
            long = format!("{long} of {length}");
            if !std::mem::replace(&mut self.told_cap, true) {
                let text = format!(
                    "{} MB held only {} of the {length} asked for. raise memory in settings → video, or lower the quality.",
                    self.cfg.max_memory_mb,
                    human_duration(secs)
                );
                self.notify("afterglow: the memory cap cut the clip short", &text, false);
            }
        }
        note::show(monitor, self.cfg.note_position, Kind::Saving, &format!("saving the last {long}"), APP);
        self.saving = Some(Saving { id, long, monitor, capped: clip.capped, app, ms: (secs * 1000.0).round() as u32, since: Instant::now() });
        buffer::save(id, clip, self.cfg.output_dir(), name, Notify::new(self.hwnd, WM_SAVED));
    }

    fn saved(&mut self, id: u64, result: Result<PathBuf, String>) {
        if self.saving.as_ref().is_none_or(|s| s.id != id) {
            log(&format!("save #{id} finished after it was given up on: {result:?}"));
            return;
        }
        let Some(Saving { long, monitor, capped, app, ms, since, .. }) = self.saving.take() else { return };
        match result {
            Ok(path) => {
                log(&format!("save #{id} done in {:.1} s: {}", since.elapsed().as_secs_f64(), path.display()));
                gallery::remember(&path, app.as_ref(), ms);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let kind = if capped { Kind::Warning } else { Kind::Saved };
                note::show(monitor, self.cfg.note_position, kind, &format!("saved the last {long}"), &name);
                self.last_saved = Some(path);
            }
            Err(e) => {
                log(&format!("save #{id} failed: {e}"));
                note::show(monitor, self.cfg.note_position, Kind::Failed, "couldn't save", "see the notification for why");
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
                let recording = |c: &Config| (c.monitor, c.fps, c.codec, c.bitrate, c.quality, c.audio_sources.clone(), c.audio_track_names.clone());
                let restart = recording(&cfg) != recording(&self.cfg);
                let hotkey = cfg.save_hotkey != self.cfg.save_hotkey;
                let updates = (cfg.update_mode, cfg.update_beta) != (self.cfg.update_mode, self.cfg.update_beta);
                self.cfg = cfg;
                if updates {
                    self.updates_reconfigured();
                }
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

    fn sync_autostart(&mut self) {
        if instance::test_run() {
            return;
        }
        if let Err(e) = autostart::sync(APP, self.cfg.launch_at_startup) {
            self.notify("afterglow: launch at startup", &e, true);
        }
    }

    /// What the buffer is doing, for the menu's header.
    fn status(&self) -> String {
        match &self.buffer {
            Some(b) => format!("buffering · {} held", human_duration(b.held().min(self.cfg.length as f64))),
            None if self.paused => "paused".into(),
            None if self.away => "away".into(),
            None => "starting…".into(),
        }
    }

    /// The tray menu as things are now.
    fn menu(&self) -> Menu {
        let length = human_duration(self.cfg.length as f64);
        let key = self.hotkey.then_some(self.cfg.save_hotkey.as_str());
        let mut items = vec![
            Item::command(ID_SAVE, &format!("save last {length}"))
                .glyph(Glyph::Replay)
                .accented()
                .keys(key)
                .enabled(self.buffer.is_some() && !self.save_running()),
            Item::command(ID_PAUSE, "buffer").glyph(Glyph::Record).checked(!self.paused),
            Item::separator(),
            Item::command(ID_GALLERY, "gallery").glyph(Glyph::Gallery),
            Item::command(ID_LAST, "show last clip").glyph(Glyph::Video).enabled(self.last_saved.is_some()),
            Item::command(ID_FOLDER, "open folder").glyph(Glyph::Folder),
            Item::command(ID_SETTINGS, "settings").glyph(Glyph::Settings),
            Item::command(ID_LOG, "log").glyph(Glyph::Log),
            Item::separator(),
        ];
        if let Some(v) = self.update.ready_version() {
            items.push(Item::command(ID_UPDATE, "restart to update").glyph(Glyph::Update).accented().hint(Some(&v)));
        }
        items.push(Item::command(ID_QUIT, "quit").glyph(Glyph::Quit));
        Menu { accent: AMBER, header: Some(Header { title: APP.into(), status: self.status() }), items }
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

impl Host for App {
    fn updates(&mut self) -> &mut Updates {
        &mut self.update
    }

    fn window(&self) -> HWND {
        self.hwnd
    }

    fn update_settings(&self) -> (UpdateMode, bool) {
        (self.cfg.update_mode, self.cfg.update_beta)
    }

    /// afterglow has no proxy setting of its own: the one browsers use.
    fn update_proxy(&self) -> Result<Setting, String> {
        Ok(Setting::System)
    }

    /// No save is being written and no click is about to cut one.
    fn idle(&self) -> bool {
        !self.save_running() && self.clicked.is_none() && !self.exiting
    }

    fn notify_update(&mut self, title: &str, text: &str, error: bool, click: Click) {
        self.tray.notify(title, text, error);
        self.balloon = Balloon::Update(click);
    }

    fn mark_update_ready(&mut self, ready: bool) {
        if self.update_ready != ready {
            self.update_ready = ready;
            self.draw_icons();
        }
    }

    /// Closes the window from the message loop, outside the app's borrow; the buffer stops
    /// with it.
    fn exit_for_update(&mut self) {
        self.exiting = true;
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The tray menu, run outside the app's borrow: the menu's own message loop dispatches timers,
/// and so may opening a folder or a file (ShellExecute), so those run outside it too.
fn menu(hwnd: HWND) {
    let Some(m) = with_app(|a| a.menu()) else { return };
    let Some(cmd) = glint_shell::menu::track(hwnd, &m, false, || with_app(|a| a.status())) else { return };
    match cmd {
        ID_SAVE => {
            with_app(App::save);
        }
        ID_PAUSE => {
            with_app(|a| a.set_paused(!a.paused));
        }
        ID_FOLDER => {
            if let Some(dir) = with_app(|a| a.cfg.output_dir()) {
                let _ = std::fs::create_dir_all(&dir);
                open(&dir.display().to_string(), None);
            }
        }
        ID_SETTINGS => open_settings(),
        ID_GALLERY => gallery::open(None),
        ID_LAST => {
            if let Some(p) = with_app(|a| a.last_saved.clone()).flatten() {
                explorer::reveal(&p);
            }
        }
        ID_LOG => open("notepad.exe", Some(&format!("\"{}\"", glint_shell::log::path().display()))),
        ID_UPDATE => {
            with_app(|a| a.install_update());
        }
        // WM_DESTROY takes the app, so not from inside it
        ID_QUIT => unsafe {
            let _ = DestroyWindow(hwnd);
        },
        _ => {}
    }
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
                    a.clicked = Some(a.cut());
                    unsafe {
                        SetTimer(Some(hwnd), CLICK_TIMER, GetDoubleClickTime(), None);
                    }
                }),
                WM_LBUTTONDBLCLK => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), CLICK_TIMER);
                    }
                    with_app(|a| a.clicked = None);
                    // outside the app's borrow: taking the foreground sends this window messages
                    // at once. Holding it lets the gallery (or the one already open) come forward.
                    unsafe {
                        let _ = SetForegroundWindow(hwnd);
                    }
                    gallery::open(None);
                    None
                }
                WM_CONTEXTMENU => {
                    menu(hwnd);
                    None
                }
                NIN_BALLOONUSERCLICK => with_app(App::balloon_clicked),
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
        WM_PREVIEW_NOTE => {
            match Place::ALL.get(wp.0) {
                Some(&place) => with_app(|a| a.preview_note(place)),
                None => with_app(App::hide_sample),
            };
            return LRESULT(0);
        }
        WM_NOTE_CAPTURABLE => return LRESULT(note::set_capturable(wp.0 == 1) as isize),
        WM_SAVED => {
            let (id, result) = unsafe { buffer::take::<(u64, Result<PathBuf, String>)>(lp) };
            with_app(|a| a.saved(id, result));
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
                match a.clicked.take() {
                    Some(Some(cut)) => a.write(cut),
                    Some(None) => a.nothing_yet(),
                    None => {}
                }
            });
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == update::CHECK_TIMER_ID || wp.0 == update::IDLE_TIMER_ID => {
            with_app(|a| a.on_update_timer(wp.0));
            return LRESULT(0);
        }
        update::WM_UPDATE_CHECK => {
            with_app(|a| a.check_now(wp.0));
            return LRESULT(0);
        }
        update::WM_UPDATE_INSTALL => {
            with_app(|a| a.install_update());
            return LRESULT(0);
        }
        update::WM_UPDATE_DONE => {
            let done = unsafe { *Box::from_raw(lp.0 as *mut Checked) };
            with_app(|a| a.on_checked(done));
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
        // afterglow started again plainly (the Start menu, a shortcut) while it runs
        m if m != 0 && m == instance::gallery_message() => {
            // the launch let this process come forward; holding the foreground lets the gallery
            glint_ui::window::bring_forward(hwnd);
            gallery::open(None);
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
    let args: Vec<String> = std::env::args().collect();
    let updated_from = update::updated_from(&args);
    let autostarted = args.iter().any(|a| a == autostart::ARG);
    if !instance::take(APP, updated_from.is_some()) {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        match instance::again(&args) {
            Again::Gallery if instance::open_gallery(MAIN_CLASS) => {}
            Again::Quiet => {}
            Again::Gallery | Again::Notice => unsafe {
                MessageBoxW(None, w!("afterglow is already running (see the tray icon)."), w!("afterglow"), MB_ICONINFORMATION);
            },
        }
        return;
    }
    // a fresh log per run; after an update it goes on from the run that installed it
    if updated_from.is_none() {
        glint_shell::log::start_fresh();
    }
    BUILD.tidy(updated_from.is_some());
    let (cfg, cfg_err) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e)),
    };
    let mut update_error = None;
    if cfg_err.is_none() && cfg.update_mode != UpdateMode::Off {
        let extra: &[&str] = if autostarted { &[autostart::ARG] } else { &[] };
        match BUILD.install_at_start(extra) {
            Some(Ok(())) => return,
            Some(Err(e)) => update_error = Some(e),
            None => {}
        }
    }
    crate::addon::start();
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
        saves: 0,
        told_cap: false,
        told: Vec::new(),
        sample: false,
        clicked: None,
        last_saved: None,
        taskbar_created: window::taskbar_created(),
        failures: 0,
        balloon: Balloon::LastClip,
        update: Updates::new(BUILD),
        update_ready: false,
        exiting: false,
    };
    app.grab_hotkey();
    if !instance::test_run() {
        start_menu::sync(&[Shortcut { name: APP, args: "", description: "instant replay: saves the last minute of your screen" }]);
    }
    match cfg_err {
        Some(e) => app.notify("afterglow: settings error (using defaults)", &e, true),
        None => app.sync_autostart(),
    }
    if let Some(old) = &updated_from {
        log(&format!("updated from {old} to {}", BUILD.current()));
    }
    app.start();
    if let Some(e) = &update_error {
        app.notify_update("afterglow: couldn't update", e, true, Click::Log);
    } else if let Some(old) = &updated_from {
        let now = BUILD.current();
        app.notify(&format!("afterglow updated to {now}"), &format!("from {old} · click for what's new"), false);
        app.balloon = Balloon::Link(notes_url(&AFTERGLOW.tag(&now)));
    } else if app.buffer.is_some() && !autostarted {
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
    app.schedule_update_check(update::FIRST_CHECK_MS);
    APP_STATE.with(|a| *a.borrow_mut() = Some(app));
    window::run();
}
