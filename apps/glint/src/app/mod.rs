use std::cell::RefCell;
use std::path::PathBuf;

use glint_core::config::{ClickMode, Config, Keystrokes, ShotStyle, UpdateMode};
use glint_core::format::{human_duration, human_size};
use glint_core::geom::rect::Rect;
use glint_core::history::{self, Entry, Kind};
use glint_core::log::log;
use glint_core::naming::Subject;
use glint_core::record::pipeline::{Outcome, Recorder};
use glint_core::update::notes_url;
use glint_core::wide::wide;
use glint_media::record::{self, ClickHighlights, KeyOverlay, Params, Source};
use glint_shell::identity::{front_window, identify};
use glint_shell::instance::{self, Again};
use glint_shell::log::path as log_path;
use glint_shell::start_menu::{self, Shortcut};
use glint_shell::update::{self, Checked, Host, Updates};
use glint_shell::{autostart, file_menu, monitors, window};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::Shell::{NIN_BALLOONUSERCLICK, NIN_SELECT, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::indicator::{self, Indicator};
use crate::picker::{self, Picked, Purpose};
use crate::platform::{capture, cursor};
use crate::platform::paths::{migrate_output_dir, unique_path};
use crate::shot::editor::Output;
use crate::shot::{self, Delivered, Shot};
use crate::upload::{self, Done};

mod hotkeys;
pub(crate) mod ipc;
mod menu;
pub(crate) mod tray;
pub(crate) mod updates;

pub use ipc::SETTINGS_ARG;
use hotkeys::{ESC_HOTKEY_ID, FULL_HOTKEY_ID, HOTKEY_ID, MONITOR_HOTKEY_ID, PAUSE_HOTKEY_ID, SHOT_HOTKEY_ID};
use menu::show_menu;
use tray::{IDLE_TIP, Tray};
use updates::BUILD;

/// NIN_SELECT | NINF_KEY: the icon was activated with the keyboard.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_SELECTION: u32 = WM_APP + 2;
const WM_APP_DONE: u32 = WM_APP + 3;
const WM_APP_QUIT: u32 = WM_APP + 4;
/// Posted by the indicator pill's buttons (see the handler for WPARAM values).
const WM_APP_STOP: u32 = WM_APP + 5;
const COUNTDOWN_TIMER_ID: usize = 7;
/// Runs the tray icon's single-click action once the double-click time has passed.
const CLICK_TIMER_ID: usize = 8;
/// A pinned screenshot's copy / save: a `Box<Shot>` in LPARAM.
const WM_APP_PIN: u32 = WM_APP + 30;

enum State {
    Idle,
    Selecting(HWND),
    Countdown { params: Params, monitors: Vec<Rect>, indicator: Indicator, left: u32, fps: f64 },
    Recording { rec: Recorder, indicator: Option<Indicator>, size: (i32, i32), fps: f64 },
    Stopping { size: (i32, i32), fps: f64 },
}

/// What clicking the last notification opens.
enum ClickTarget {
    None,
    /// A saved capture: the gallery opens on it.
    Capture(PathBuf),
    Link(String),
    Log,
    /// Restart into the update, or download and install it.
    Update,
}

struct App {
    hwnd: HWND,
    cfg: Config,
    /// The record, screenshot, all-screens and monitor hotkeys, while registered.
    hotkey: Option<String>,
    shot_hotkey: Option<String>,
    full_hotkey: Option<String>,
    monitor_hotkey: Option<String>,
    pause_hotkey: Option<String>,
    state: State,
    tray: Tray,
    taskbar_created: u32,
    click: ClickTarget,
    exiting: bool,
    update: Updates,
    /// The file of the recording being made and the app it shows, for its history entry.
    recording_app: Option<(PathBuf, glint_core::naming::Source)>,
    /// The window in front when the picker was asked for: a region's recording shows its app
    /// when the region's middle is on it.
    front: Option<(HWND, Rect)>,
    /// Notices about how recordings are made (the software encoder) told since glint started.
    told: Vec<String>,
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

    fn notify_done(&mut self, title: &str, text: &str, click: ClickTarget) {
        if self.cfg.notify_success {
            self.notify(title, text, false, click);
        }
    }

    /// Re-reads settings; (re-)registers the hotkey when it changed or isn't held.
    fn reload(&mut self) {
        match Config::load() {
            Ok(cfg) => {
                let recording = matches!(self.state, State::Recording { .. });
                let hotkeys_changed = self.hotkey.as_deref() != Some(cfg.hotkey.as_str())
                    || self.shot_hotkey.as_deref() != Some(cfg.screenshot_hotkey.as_str())
                    || self.full_hotkey.as_deref() != Some(cfg.fullscreen_hotkey.as_str())
                    || self.monitor_hotkey.as_deref() != Some(cfg.monitor_hotkey.as_str())
                    || recording && self.pause_hotkey.as_deref() != Some(cfg.pause_hotkey.as_str());
                let startup_changed = self.cfg.launch_at_startup != cfg.launch_at_startup;
                let menu_changed = self.cfg.explorer_menu != cfg.explorer_menu;
                let accent_changed = self.cfg.accent_rgb() != cfg.accent_rgb();
                let updates_changed =
                    self.cfg.update_mode != cfg.update_mode || self.cfg.update_beta != cfg.update_beta;
                let before = std::mem::replace(&mut self.cfg, cfg);
                if hotkeys_changed {
                    self.register_hotkeys(Some(&before));
                }
                if startup_changed {
                    self.sync_autostart();
                }
                if menu_changed {
                    self.sync_explorer_menu();
                }
                if accent_changed {
                    self.tray.set_accent(self.cfg.accent_rgb());
                }
                if updates_changed {
                    self.updates_reconfigured();
                }
            }
            Err(e) => self.notify("glint: settings error", &e, true, ClickTarget::None),
        }
    }

    /// Adds or removes "open in glint editor" on videos in Explorer's menu and "glint editor"
    /// under its "open with"; adding keeps them pointing at this exe.
    fn sync_explorer_menu(&mut self) {
        if instance::test_run() {
            return;
        }
        let exts = crate::clip::VIDEO_EXTS;
        let on = self.cfg.explorer_menu;
        let synced = file_menu::sync("glint.edit", "open in glint editor", &exts, crate::clip::CLIP_ARG, on)
            .and_then(|()| file_menu::open_with("glint editor", &exts, crate::clip::CLIP_ARG, on));
        if let Err(e) = synced {
            self.notify("glint: explorer menu", &e, true, ClickTarget::None);
        }
    }

    fn sync_autostart(&mut self) {
        if instance::test_run() {
            return;
        }
        if let Err(e) = autostart::sync("glint", self.cfg.launch_at_startup) {
            self.notify("glint: launch at startup", &e, true, ClickTarget::None);
        }
    }

    /// A hotkey (or the tray menu) was used: start picking, or stop what's running.
    fn toggle(&mut self, shot: bool) {
        match &self.state {
            State::Idle => self.start_selection(shot),
            State::Selecting(h) => picker::cancel(*h),
            State::Countdown { .. } if !shot => self.cancel_countdown(),
            State::Recording { .. } if !shot => self.stop_recording(false),
            State::Countdown { .. } | State::Recording { .. } | State::Stopping { .. } => {}
        }
    }

    fn toggle_pause(&mut self) {
        let State::Recording { rec, indicator, .. } = &self.state else { return };
        let pause = !rec.paused();
        if pause {
            rec.pause();
        } else {
            rec.resume();
        }
        if let Some(i) = indicator {
            i.set_paused(pause);
        }
        log(if pause { "paused" } else { "resumed" });
        self.update_tip();
    }

    fn on_indicator(&mut self, button: usize) {
        match (&self.state, button) {
            (State::Countdown { .. }, indicator::DISCARD) => self.cancel_countdown(),
            (State::Recording { .. }, indicator::PAUSE) => self.toggle_pause(),
            (State::Recording { .. }, indicator::STOP | indicator::DISCARD) => {
                self.stop_recording(button == indicator::DISCARD)
            }
            _ => {}
        }
    }

    fn update_tip(&mut self) {
        if let State::Recording { rec, size, fps, .. } = &self.state {
            let what = if rec.paused() { "paused" } else { "recording" };
            let tip = format!("glint: {what} {}×{} @ {:.0} fps", size.0, size.1, fps);
            self.tray.set_recording(true, &tip);
        }
    }

    fn start_selection(&mut self, shot: bool) {
        self.front = front_window();
        self.reload();
        crate::shot::editor::use_toolbar(self.cfg.toolbar.clone(), self.cfg.toolbar_keys);
        let outputs = match monitors::enumerate() {
            Ok(o) if !o.is_empty() => o,
            Ok(_) => return self.notify("glint", "No monitors found.", true, ClickTarget::None),
            Err(e) => return self.notify("glint: can't list monitors", &e.message(), true, ClickTarget::None),
        };
        let purpose = match shot {
            true => Purpose::Shot { draw_first: self.cfg.screenshot_style == ShotStyle::Sharex },
            false => Purpose::Record { min: self.cfg.codec.codec().min_size() },
        };
        let (freeze, elements) = (self.cfg.freeze_screen, self.cfg.ui_elements);
        let with_cursor = self.cfg.screenshot_cursor;
        match picker::open(self.hwnd, WM_APP_SELECTION, outputs, purpose, freeze, elements, with_cursor) {
            Ok(h) => self.state = State::Selecting(h),
            Err(e) => self.notify("glint: can't open the selection overlay", &e, true, ClickTarget::Log),
        }
    }

    /// Screenshots every monitor at once, or only the one under the mouse, without the picker;
    /// ignored while anything else runs.
    fn shot_screens(&mut self, under_mouse: bool) {
        if !matches!(self.state, State::Idle) {
            return;
        }
        let pointer = if self.cfg.screenshot_cursor { cursor::current() } else { None };
        let captured = if under_mouse { capture::monitor_under_mouse() } else { capture::virtual_screen() };
        self.reload();
        match captured {
            Ok((mut pixels, at)) => {
                // nothing to edit it in: the cursor is drawn in
                if let Some((img, p)) = pointer {
                    cursor::stamp(&mut pixels, at.w, &img, (p.0 - at.x, p.1 - at.y));
                }
                let (w, h) = (at.w as u32, at.h as u32);
                let shot = Shot { pixels, w, h, alpha: false, at, output: Output::Default, source: None };
                self.finish_shot(shot, Subject::Screen)
            }
            Err(e) => {
                log(&format!("screenshot: {e}"));
                self.notify("glint: screenshot failed", &e, true, ClickTarget::Log);
            }
        }
    }

    fn on_selection(&mut self, picked: Option<Picked>) {
        let State::Selecting(picker) = std::mem::replace(&mut self.state, State::Idle) else { return };
        let sel = match picked {
            Some(Picked::Region(sel)) if !self.exiting => sel,
            Some(Picked::Shot(shot)) if !self.exiting => {
                picker::dismiss(picker);
                return self.finish_shot(shot, Subject::Region);
            }
            _ => return picker::dismiss(picker),
        };
        let native = sel.outputs.iter().map(|o| o.refresh_hz).fold(0.0, f64::max);
        let fps = match self.cfg.fps {
            0 => native,
            cap => (cap as f64).min(native),
        };
        let subject = sel.source.as_ref().map_or(Subject::Region, Subject::Window);
        let path = unique_path(&self.cfg, Kind::Recording, subject, "mp4");
        let (cx, cy) = (sel.rect.x + sel.rect.w / 2, sel.rect.y + sel.rect.h / 2);
        let in_front = || self.front.take().filter(|(_, r)| r.contains(cx, cy)).and_then(|(h, _)| identify(h));
        let app = subject.source().cloned().or_else(in_front).filter(|s| !s.app.trim().is_empty());
        self.recording_app = app.map(|app| (path.clone(), app));
        let params = Params {
            sources: sel
                .outputs
                .iter()
                .map(|o| Source { adapter_idx: o.adapter_idx, output_idx: o.output_idx, desktop: o.rect })
                .collect(),
            region: sel.rect,
            fps,
            codec: self.cfg.codec.codec(),
            qp: self.cfg.quality,
            effort: self.cfg.effort,
            keyframe_secs: 2.0,
            bitrate: None,
            draw_cursor: self.cfg.draw_cursor,
            clicks: self.cfg.click_highlights.then(|| ClickHighlights {
                burn_in: self.cfg.clicks == ClickMode::BurnIn,
                accent: self.cfg.accent_rgb(),
            }),
            keys: (self.cfg.keystrokes != Keystrokes::Off).then(|| KeyOverlay {
                level: self.cfg.keystrokes,
                burn_in: self.cfg.keystrokes_mode == ClickMode::BurnIn,
                pos: self.cfg.keystroke_position,
                accent: self.cfg.accent_rgb(),
            }),
            audio: self.cfg.audio_sources.clone(),
            track_names: self.cfg.audio_track_names.clone(),
            path,
        };
        let monitors: Vec<Rect> = sel.outputs.iter().map(|o| o.rect).collect();
        if self.cfg.countdown > 0 {
            picker::dismiss(picker);
            return self.start_countdown(params, monitors, fps);
        }
        self.begin_recording(params, &monitors, fps, None, Some(picker));
    }

    fn start_countdown(&mut self, params: Params, monitors: Vec<Rect>, fps: f64) {
        let left = self.cfg.countdown;
        let inside = self.cfg.indicator_inside;
        match Indicator::show(params.region, &monitors, self.hwnd, WM_APP_STOP, left, inside) {
            Ok(indicator) => {
                unsafe {
                    SetTimer(Some(self.hwnd), COUNTDOWN_TIMER_ID, 1000, None);
                }
                self.grab_escape();
                self.tray.set_recording(true, "glint: starting…");
                self.state = State::Countdown { params, monitors, indicator, left, fps };
            }
            Err(e) => {
                log(&format!("countdown unavailable: {e}"));
                self.begin_recording(params, &monitors, fps, None, None);
            }
        }
    }

    fn tick_countdown(&mut self) {
        if let State::Countdown { left, indicator, .. } = &mut self.state {
            *left = left.saturating_sub(1);
            if *left > 0 {
                return indicator.count(*left);
            }
        }
        self.end_countdown();
        if let State::Countdown { params, monitors, indicator, fps, .. } = std::mem::replace(&mut self.state, State::Idle) {
            self.begin_recording(params, &monitors, fps, Some(indicator), None);
        }
    }

    fn end_countdown(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), COUNTDOWN_TIMER_ID);
        }
        self.release_escape();
    }

    fn cancel_countdown(&mut self) {
        if matches!(self.state, State::Countdown { .. }) {
            self.end_countdown();
            self.state = State::Idle;
            self.tray.set_recording(false, IDLE_TIP);
            log("countdown cancelled");
        }
    }

    fn begin_recording(
        &mut self,
        params: Params,
        monitors: &[Rect],
        fps: f64,
        mut indicator: Option<Indicator>,
        picker: Option<HWND>,
    ) {
        let region = params.region;
        let hwnd = self.hwnd.0 as isize;
        let done = move |res: Result<Outcome, String>| unsafe {
            let b = Box::into_raw(Box::new(res));
            if PostMessageW(Some(HWND(hwnd as _)), WM_APP_DONE, WPARAM(0), LPARAM(b as isize)).is_err() {
                drop(Box::from_raw(b));
            }
        };
        // The pipeline initializes while the picker or the countdown bubble fades out; capture
        // starts once they're gone.
        let started = record::start(params, done);
        if let Some(p) = picker {
            picker::dismiss(p);
        }
        if let Some(i) = &mut indicator {
            i.end_countdown();
        }
        match started.and_then(|s| s.wait()) {
            Ok(rec) => {
                for notice in rec.notices() {
                    log(notice);
                    if !self.told.contains(notice) {
                        self.told.push(notice.clone());
                        self.notify("glint: recording", notice, false, ClickTarget::Log);
                    }
                }
                let indicator = match indicator {
                    Some(mut i) => i.start().map(|_| i),
                    None => Indicator::show(region, monitors, self.hwnd, WM_APP_STOP, 0, self.cfg.indicator_inside),
                };
                let indicator = indicator.map_err(|e| log(&format!("indicator unavailable: {e}"))).ok();
                self.state = State::Recording { rec, indicator, size: (region.w, region.h), fps };
                self.grab_pause_hotkey();
                self.update_tip();
            }
            Err(e) => {
                log(&format!("start failed: {e}"));
                self.tray.set_recording(false, IDLE_TIP);
                self.notify("glint: couldn't start recording", &e, true, ClickTarget::Log);
            }
        }
    }

    /// Copies / saves / uploads `shot`; `subject` names its file when it isn't one app's window.
    fn finish_shot(&mut self, shot: Shot, subject: Subject) {
        if shot.output == Output::Pin {
            if let Err(e) = shot::pin::open(shot, self.hwnd, WM_APP_PIN, self.cfg.accent_rgb()) {
                log(&format!("pin: {e}"));
                self.notify("glint: couldn't pin the screenshot", &e, true, ClickTarget::Log);
            }
            return;
        }
        let size = format!("{}×{}", shot.w, shot.h);
        let Delivered { copied, saved, uploading } = match shot::deliver(&shot, &self.cfg, self.hwnd, subject) {
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
                self.notify_done(title, &text, ClickTarget::Capture(path));
            }
            None if copied => self.notify_done("Screenshot copied", &format!("{size}, ready to paste"), ClickTarget::None),
            None if uploading => {
                let text = format!("{size}, to {}", self.cfg.upload_destination);
                self.notify_done("Uploading screenshot…", &text, ClickTarget::None);
            }
            None => {}
        }
    }

    /// Ends the recording; with `discard` the file is thrown away instead of saved.
    fn stop_recording(&mut self, discard: bool) {
        if let State::Recording { rec, size, fps, .. } = std::mem::replace(&mut self.state, State::Idle) {
            self.release_pause_hotkey();
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
        self.release_pause_hotkey();
        self.tray.set_recording(false, IDLE_TIP);
        match res {
            Ok(Outcome::Discarded) => {
                self.notify_done("Recording discarded", "Nothing was saved.", ClickTarget::None);
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
                match s.tracks.as_slice() {
                    [] => {}
                    [one] => text += &format!(" · {one}"),
                    all => text += &format!("\n{} audio tracks: {}", all.len(), all.join(", ")),
                }
                for issue in &s.audio_issues {
                    text += &format!("\n{issue}");
                }
                if s.dropped > 0 {
                    text += &format!("\n{} frames dropped (lower \"effort\" in settings)", s.dropped);
                }
                if let Some(why) = &s.stopped_early {
                    text += &format!("\nStopped early: {why}");
                }
                let app = self.recording_app.take().filter(|(p, _)| *p == s.path).map(|(_, app)| app);
                let ms = (s.duration_s * 1000.0).round() as u32;
                history::add(Entry::new(Kind::Recording, Some(&s.path), bytes).with_source(app.as_ref()).with_duration(Some(ms)));
                let mut upload_failed = false;
                if self.cfg.recording_upload
                    && let Err(e) = upload::start_file(&self.cfg, Kind::Recording, &s.path, self.hwnd)
                {
                    text += &format!("\nCouldn't upload: {e}");
                    upload_failed = true;
                }
                if self.cfg.open_trimmer
                    && let Err(e) = crate::clip::spawn(&s.path)
                {
                    log(&format!("can't open the trimmer: {e}"));
                }
                if s.dropped > 0 || s.stopped_early.is_some() || !s.audio_issues.is_empty() || upload_failed {
                    self.notify("Recording saved", &text, false, ClickTarget::Capture(s.path));
                } else {
                    self.notify_done("Recording saved", &text, ClickTarget::Capture(s.path));
                }
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

    fn on_uploaded(&mut self, done: Done) {
        if !upload::active() {
            self.tray.set_idle_tip(IDLE_TIP);
        }
        match done.result {
            Ok(url) => {
                history::add(Entry::new(done.kind, done.path.as_deref(), done.size).with_url(&url));
                let copied = self.cfg.upload_copy_link && {
                    let copied = glint_shell::clipboard::copy_text(Some(self.hwnd), &url);
                    if let Err(e) = &copied {
                        log(&format!("can't copy the link: {e}"));
                    }
                    copied.is_ok()
                };
                let title = if copied { "Uploaded, link copied" } else { "Uploaded" };
                let text = format!("{url}\n{} · {}", human_size(done.size), done.host);
                self.notify_done(title, &text, ClickTarget::Link(url));
            }
            Err(e) => {
                log(&format!("upload to {} failed: {e}", done.host));
                self.notify("glint: upload failed", &format!("{}: {e}", done.host), true, ClickTarget::Log);
            }
        }
    }

    /// Exit: finish a running recording first so the file is finalized.
    fn exit(&mut self) {
        self.exiting = true;
        match &self.state {
            State::Recording { .. } => self.stop_recording(false),
            State::Stopping { .. } => {}
            State::Countdown { .. } => {
                self.cancel_countdown();
                self.close_window();
            }
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

    fn open_click_target(&mut self) {
        if matches!(self.click, ClickTarget::Update) {
            return self.install_update();
        }
        match &self.click {
            ClickTarget::Capture(p) => crate::gallery::open(Some(p)),
            ClickTarget::Link(url) => shell(url, ""),
            ClickTarget::Log => shell("notepad.exe", &format!("\"{}\"", log_path().display())),
            ClickTarget::Update | ClickTarget::None => {}
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
    let spawned = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg(ipc::SETTINGS_ARG).spawn());
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
        WM_HOTKEY if wp.0 as i32 == FULL_HOTKEY_ID || wp.0 as i32 == MONITOR_HOTKEY_ID => {
            with_app(|a| a.shot_screens(wp.0 as i32 == MONITOR_HOTKEY_ID));
            return LRESULT(0);
        }
        WM_HOTKEY if wp.0 as i32 == PAUSE_HOTKEY_ID => {
            with_app(|a| a.toggle_pause());
            return LRESULT(0);
        }
        WM_HOTKEY if wp.0 as i32 == ESC_HOTKEY_ID => {
            with_app(|a| a.cancel_countdown());
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == COUNTDOWN_TIMER_ID => {
            with_app(|a| a.tick_countdown());
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == update::CHECK_TIMER_ID || wp.0 == update::IDLE_TIMER_ID => {
            with_app(|a| a.on_update_timer(wp.0));
            return LRESULT(0);
        }
        WM_TIMER if wp.0 == CLICK_TIMER_ID => {
            unsafe {
                let _ = KillTimer(Some(hwnd), CLICK_TIMER_ID);
            }
            with_app(|a| a.toggle(true));
            return LRESULT(0);
        }
        WM_APP_STOP => {
            with_app(|a| a.on_indicator(wp.0));
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
        ipc::WM_RELOAD => {
            with_app(|a| a.reload());
            return LRESULT(0);
        }
        ipc::WM_HOTKEY_PAUSE => {
            with_app(|a| if wp.0 == 1 { a.pause_hotkeys() } else { a.register_hotkeys(Some(&a.cfg.clone())) });
            return LRESULT(0);
        }
        upload::WM_UPLOAD_PROGRESS => {
            with_app(|a| a.tray.set_idle_tip(&format!("glint: uploading {} %", wp.0)));
            return LRESULT(0);
        }
        WM_APP_TRAY => {
            match (lp.0 & 0xFFFF) as u32 {
                WM_CONTEXTMENU => show_menu(hwnd, false),
                NIN_KEYSELECT => show_menu(hwnd, true),
                WM_LBUTTONDOWN => unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    SetTimer(Some(hwnd), CLICK_TIMER_ID, GetDoubleClickTime(), None);
                },
                WM_LBUTTONDBLCLK => {
                    unsafe {
                        let _ = KillTimer(Some(hwnd), CLICK_TIMER_ID);
                    }
                    crate::gallery::open(None);
                }
                NIN_BALLOONUSERCLICK => {
                    with_app(|a| a.open_click_target());
                }
                _ => {}
            }
            return LRESULT(0);
        }
        // Results posted from the overlay / encoder thread own a Box in LPARAM. If the state
        // is busy (re-entrant dispatch), re-post instead of dropping the result.
        WM_APP_SELECTION | WM_APP_DONE | WM_APP_PIN | upload::WM_UPLOADED | update::WM_UPDATE_DONE => {
            let handled = with_app(|a| unsafe {
                match msg {
                    WM_APP_SELECTION => a.on_selection(*Box::from_raw(lp.0 as *mut Option<Picked>)),
                    WM_APP_PIN => a.finish_shot(*Box::from_raw(lp.0 as *mut Shot), Subject::Region),
                    upload::WM_UPLOADED => a.on_uploaded(*Box::from_raw(lp.0 as *mut Done)),
                    update::WM_UPDATE_DONE => a.on_checked(*Box::from_raw(lp.0 as *mut Checked)),
                    _ => a.on_done(*Box::from_raw(lp.0 as *mut Result<Outcome, String>)),
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
        // glint started again plainly (the Start menu, a shortcut) while it runs
        m if m != 0 && m == instance::gallery_message() => {
            // the launch let this process come forward; holding the foreground lets the gallery
            glint_ui::window::bring_forward(hwnd);
            crate::gallery::open(None);
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
    let args: Vec<String> = std::env::args().collect();
    let updated_from = update::updated_from(&args);
    let autostarted = args.iter().any(|a| a == autostart::ARG);
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if !instance::take("glint", updated_from.is_some()) {
            match instance::again(args.get(1..).unwrap_or_default()) {
                Again::Gallery if instance::open_gallery(ipc::MAIN_CLASS) => {}
                Again::Quiet => {}
                Again::Gallery | Again::Notice => {
                    MessageBoxW(None, w!("glint is already running (see the tray icon)."), w!("glint"), MB_ICONINFORMATION);
                }
            }
            return;
        }
        // a fresh log per run; after an update it goes on from the run that installed it
        if updated_from.is_none() {
            glint_shell::log::start_fresh();
        }
        BUILD.tidy(updated_from.is_some());
        let (mut cfg, cfg_err) = match Config::load() {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        };
        if cfg_err.is_none() {
            migrate_output_dir(&mut cfg);
        }
        let mut update_error = None;
        if cfg_err.is_none() && cfg.update_mode != UpdateMode::Off {
            let extra: &[&str] = if autostarted { &[autostart::ARG] } else { &[] };
            match BUILD.install_at_start(extra) {
                Some(Ok(())) => return,
                Some(Err(e)) => update_error = Some(e),
                None => {}
            }
        }

        let hwnd = window::hidden(ipc::MAIN_CLASS, "glint", Some(wndproc)).expect("main window");

        let tray = Tray::new(hwnd, WM_APP_TRAY, cfg.accent_rgb());
        let mut app = App {
            hwnd,
            cfg,
            hotkey: None,
            shot_hotkey: None,
            full_hotkey: None,
            monitor_hotkey: None,
            pause_hotkey: None,
            state: State::Idle,
            tray,
            taskbar_created: window::taskbar_created(),
            click: ClickTarget::None,
            exiting: false,
            update: Updates::new(BUILD),
            recording_app: None,
            front: None,
            told: Vec::new(),
        };
        app.register_hotkeys(None);
        if let Some(old) = &updated_from {
            log(&format!("updated from {old} to {}", BUILD.current()));
        }
        if !instance::test_run() {
            start_menu::sync(&[
                Shortcut { name: "glint", args: "", description: "screen recorder and screenshots" },
                Shortcut { name: "glint editor", args: crate::clip::CLIP_ARG, description: "trim and export videos" },
            ]);
        }
        match cfg_err {
            Some(e) => app.notify("glint: settings error (using defaults)", &e, true, ClickTarget::None),
            None => {
                app.sync_autostart(); // keeps the entry pointing at this exe
                app.sync_explorer_menu();
                let all_held = app.hotkey.is_some()
                    && app.shot_hotkey.is_some()
                    && app.full_hotkey.is_some()
                    && app.monitor_hotkey.is_some();
                if let Some(e) = &update_error {
                    app.notify("glint: couldn't update", e, true, ClickTarget::Log);
                } else if let Some(old) = &updated_from {
                    let now = BUILD.current();
                    let notes = ClickTarget::Link(notes_url(&format!("v{now}")));
                    app.notify_done(&format!("updated to {now}"), &format!("from {old} · click for what's new"), notes);
                } else if all_held && !autostarted {
                    let text = format!(
                        "{} records a region, {} takes a screenshot, {} captures all screens, {} the \
                         monitor under the mouse. Double-click the tray icon for settings.",
                        app.cfg.hotkey, app.cfg.screenshot_hotkey, app.cfg.fullscreen_hotkey, app.cfg.monitor_hotkey
                    );
                    app.notify_done("glint is running", &text, ClickTarget::None);
                }
            }
        }
        app.schedule_update_check(update::FIRST_CHECK_MS);
        log(&format!(
            "started, hotkeys record {:?}, screenshot {:?}, all screens {:?}, monitor {:?}",
            app.hotkey, app.shot_hotkey, app.full_hotkey, app.monitor_hotkey
        ));
        let own = app.own_windows();
        APP.with(|a| *a.borrow_mut() = Some(app));
        update::reopen(&args, &own);

        window::run();
    }
}
