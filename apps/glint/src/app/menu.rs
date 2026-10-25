//! glint's tray menu: capture, the recent captures, the folder, settings and the log, in our own
//! popup (see `glint_shell::menu`).

use glint_core::format::human_size;
use glint_core::history::{self, Entry, Kind as HistoryKind};
use glint_shell::log::path as log_path;
use glint_shell::menu::{self, Glyph, Header, Item, Menu};
use glint_shell::time::local_time;
use windows::Win32::Foundation::HWND;

use super::{ClickTarget, State, open_settings, shell, with_app};
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
const ID_SHOT_MONITOR: usize = 110;
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

fn recent_menu(entries: &[Entry]) -> Vec<Item> {
    let mut items: Vec<Item> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let id = |action: usize| ID_RECENT + i * ACTIONS + action;
            let exists = e.exists();
            let mut actions = Vec::new();
            if exists {
                actions.push(Item::command(id(OPEN), "open").glyph(Glyph::Open));
                actions.push(Item::command(id(SHOW), "show in folder").glyph(Glyph::Folder));
            }
            if e.url.is_some() {
                actions.push(Item::command(id(OPEN_LINK), "open link").glyph(Glyph::Open));
                actions.push(Item::command(id(COPY_LINK), "copy link").glyph(Glyph::Copy));
            } else if exists {
                actions.push(Item::command(id(UPLOAD), "upload").glyph(Glyph::Upload));
            }
            if exists && e.is_mp4() {
                actions.push(Item::command(id(TRIM), "trim…").glyph(Glyph::Trim));
            }
            let glyph = match e.kind {
                HistoryKind::Shot => Glyph::Image,
                HistoryKind::Recording => Glyph::Video,
            };
            let size = if exists { human_size(e.size) } else { "link".into() };
            let detail = if e.is_gif() { format!("gif · {size}") } else { size };
            Item::submenu(&e.when(local_time), actions).glyph(glyph).hint(Some(&detail))
        })
        .collect();
    items.push(Item::separator());
    items.push(Item::command(ID_CLEAR_HISTORY, "clear history").glyph(Glyph::Clear));
    items
}

fn recent_action(hwnd: HWND, e: &Entry, action: usize) {
    let path = e.path.as_deref();
    let url = e.url.as_deref();
    let failed = |title: &str, text: String| {
        with_app(|a| a.notify(title, &text, true, ClickTarget::None));
    };
    match (action, path, url) {
        (OPEN, Some(p), _) => shell(&p.display().to_string(), ""),
        (SHOW, Some(p), _) => glint_shell::explorer::reveal(p),
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

/// What glint is doing, for the menu's header: the version when there's nothing to say.
fn status() -> Option<String> {
    with_app(|a| match &a.state {
        State::Idle if upload::active() => "uploading…".to_string(),
        State::Idle => match a.ready_version() {
            Some(v) => format!("{} → {v}", env!("CARGO_PKG_VERSION")),
            None => env!("CARGO_PKG_VERSION").to_string(),
        },
        State::Selecting(_) => "picking a region".to_string(),
        State::Countdown { left, .. } => format!("starting in {left}"),
        State::Recording { rec, size, .. } => {
            let what = if rec.paused() { "paused" } else { "recording" };
            format!("{what} · {}×{}", size.0, size.1)
        }
        State::Stopping { .. } => "saving…".to_string(),
    })
}

pub(super) fn show_menu(hwnd: HWND, keyboard: bool) {
    let Some((counting, paused, busy, keys, accent)) = with_app(|a| {
        let paused = match &a.state {
            State::Recording { rec, .. } => Some(rec.paused()),
            _ => None,
        };
        let counting = matches!(a.state, State::Countdown { .. });
        let busy = matches!(a.state, State::Stopping { .. } | State::Selecting(_));
        let keys = [&a.hotkey, &a.shot_hotkey, &a.full_hotkey, &a.monitor_hotkey, &a.pause_hotkey].map(Clone::clone);
        (counting, paused, busy, keys, a.cfg.accent_rgb())
    }) else {
        return;
    };
    let [hotkey, shot_hotkey, full_hotkey, monitor_hotkey, pause_hotkey] = keys;
    let recording = counting || paused.is_some();
    let record = match (counting, recording) {
        (true, _) => Item::command(ID_RECORD, "cancel countdown").glyph(Glyph::Clear),
        (false, true) => Item::command(ID_RECORD, "stop recording").glyph(Glyph::Stop).accented(),
        (false, false) => Item::command(ID_RECORD, "record region").glyph(Glyph::Record).accented(),
    };
    let mut items = vec![record.keys(hotkey.as_deref()).enabled(!busy)];
    if let Some(p) = paused {
        let pause = if p { Item::command(ID_PAUSE, "resume").glyph(Glyph::Play) } else { Item::command(ID_PAUSE, "pause").glyph(Glyph::Pause) };
        items.push(pause.keys(pause_hotkey.as_deref()));
    }
    let can_shoot = !busy && !recording;
    items.extend([
        Item::command(ID_SHOT, "screenshot").glyph(Glyph::Region).keys(shot_hotkey.as_deref()).enabled(can_shoot),
        Item::command(ID_SHOT_ALL, "all screens").glyph(Glyph::Screens).keys(full_hotkey.as_deref()).enabled(can_shoot),
        Item::command(ID_SHOT_MONITOR, "this monitor").glyph(Glyph::Monitor).keys(monitor_hotkey.as_deref()).enabled(can_shoot),
        Item::separator(),
    ]);
    let last = history::last_recording();
    let recent = history::recent(RECENT);
    items.push(Item::command(ID_TRIM, "trim last recording").glyph(Glyph::Trim).enabled(last.is_some()));
    let recent_items = if recent.is_empty() { Vec::new() } else { recent_menu(&recent) };
    items.extend([
        Item::submenu("recent", recent_items).glyph(Glyph::Recent),
        Item::command(ID_FOLDER, "open folder").glyph(Glyph::Folder),
        Item::command(ID_SETTINGS, "settings").glyph(Glyph::Settings),
        Item::command(ID_LOG, "log").glyph(Glyph::Log),
        Item::separator(),
    ]);
    if let Some(v) = with_app(|a| a.ready_version()).flatten() {
        items.push(Item::command(ID_UPDATE, "restart to update").glyph(Glyph::Update).accented().hint(Some(&v)));
    }
    items.push(Item::command(ID_EXIT, "quit").glyph(Glyph::Quit));
    let header = Header { title: "glint".into(), status: status().unwrap_or_default() };
    let menu = Menu { accent, header: Some(header), items };
    let Some(cmd) = menu::track(hwnd, &menu, keyboard, status) else { return };
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
        ID_SHOT_ALL | ID_SHOT_MONITOR => {
            with_app(|a| a.shot_screens(cmd == ID_SHOT_MONITOR));
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
