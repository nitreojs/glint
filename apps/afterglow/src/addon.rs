//! The Deadlock addon behind "deadlock events" (config `deadlock_addon`): a thread of the tray
//! app keeps it installed (or takes it out) with [`glint_core::events::addon::sync`] at start and
//! whenever the settings save, waiting for the game to close first, and tells how that went in a
//! notification. The settings window asks [`status`] for its hint.

use std::path::PathBuf;
use std::time::Duration;

use glint_core::events::addon::{self, Dmm, Done, State};
use glint_core::events::steam;
use glint_core::log::log;
use glint_core::paths::app_dir_for;
use glint_core::wide::from_wstr;
use glint_shell::afterglow::MAIN_CLASS;
use glint_shell::instance;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GetLastError, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::core::HSTRING;

use crate::config::{APP, Config};

/// The addon, as `mods/deadlock/build.py` builds it.
pub const VPK: &[u8] = include_bytes!("../assets/afterglow_events_dir.vpk");
const GAME_EXE: &str = "deadlock.exe";
/// Set by the settings window after it saved.
const SYNC_EVENT: &str = "afterglow-deadlock-addon-sync";
/// Held by the tray app whose thread keeps the addon.
const KEEPER: &str = "afterglow-deadlock-addon";
/// How often a change waiting for the game to close looks again.
const WAIT_GAME: Duration = Duration::from_secs(5);
/// Before the first look: the tray icon that tells how it went is up by then.
const FIRST_LOOK: Duration = Duration::from_secs(5);

/// Starts keeping the addon as the settings say; once per session, not in test runs.
pub fn start() {
    if instance::test_run() {
        return;
    }
    let keeper = unsafe { CreateMutexW(None, true, &HSTRING::from(instance::scoped(KEEPER))) };
    if keeper.is_err() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
    }
    let Ok(event) = (unsafe { CreateEventW(None, false, false, &HSTRING::from(instance::scoped(SYNC_EVENT))) }) else { return };
    let event = event.0 as usize;
    if let Err(e) = std::thread::Builder::new().name("deadlock addon".into()).spawn(move || keep(HANDLE(event as _))) {
        log(&format!("deadlock addon: can't start: {e}"));
    }
}

/// Asks the tray app to bring the addon in line with the saved settings.
pub fn poke() {
    unsafe {
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, &HSTRING::from(instance::scoped(SYNC_EVENT))) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        }
    }
}

fn keep(event: HANDLE) {
    let mut wait = Some(FIRST_LOOK);
    let mut told = String::new();
    loop {
        unsafe { WaitForSingleObject(event, wait.map_or(INFINITE, |w| w.as_millis() as u32)) };
        wait = None;
        let want = match Config::load() {
            Ok(c) => c.deadlock_addon,
            Err(e) => {
                log(&format!("deadlock addon: settings unreadable, left as it is: {e}"));
                continue;
            }
        };
        let (title, text, error) = match addons() {
            Err(_) if !want => continue,
            Err(why) => ("afterglow: deadlock events", why, true),
            Ok(dir) if game_running() => {
                let pending = match addon::state(&dir, VPK) {
                    State::Missing | State::Outdated(_) => want,
                    State::Current(_) => !want,
                    State::Off => false,
                };
                if !pending {
                    continue;
                }
                wait = Some(WAIT_GAME);
                let text = if want { "the addon goes in when deadlock closes" } else { "the addon comes out when deadlock closes" };
                ("afterglow: deadlock events", text.to_string(), false)
            }
            Ok(dir) => {
                let backup = app_dir_for(APP).join("dmm-backup.json");
                let store = dmm_store();
                match addon::sync(&dir, want, VPK, &Dmm { backup: &backup, store: store.as_deref() }) {
                    Ok(Done::Nothing) => continue,
                    Ok(Done::Installed(name)) => ("afterglow: deadlock events on", format!("the addon is in ({name}): your next match is followed"), false),
                    Ok(Done::Updated(name)) => ("afterglow: deadlock events", format!("the addon is up to date ({name})"), false),
                    Ok(Done::Removed) => ("afterglow: deadlock events off", "the addon is out of deadlock".to_string(), false),
                    Err(e) => ("afterglow: deadlock events", format!("couldn't change the addon: {e}"), true),
                }
            }
        };
        if text != told {
            log(&format!("deadlock addon: {text}"));
            notify(title, &text, error);
            told = text;
        }
    }
}

fn addons() -> Result<PathBuf, String> {
    let game = game().ok_or("deadlock isn't installed through steam on this pc")?;
    addon::addons_dir(&game)
}

fn notify(title: &str, text: &str, error: bool) {
    if let Ok(main) = unsafe { FindWindowW(&HSTRING::from(instance::scoped(MAIN_CLASS)), None) } {
        glint_shell::tray::notify(main, title, text, error);
    }
}

/// What the settings show beside the switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    NoGame,
    /// Deadlock is there but loads no addons.
    NoAddons,
    /// Launch options without `-condebug`: no console log to read.
    NoLog,
    State { state: State, running: bool },
}

/// Where the addon stands now.
pub fn status() -> Status {
    let Some(steam) = steam_root() else { return Status::NoGame };
    let Some(game) = steam::app_dir(&steam, steam::DEADLOCK) else { return Status::NoGame };
    let Ok(dir) = addon::addons_dir(&game) else { return Status::NoAddons };
    if steam::launch_options(&steam, steam::DEADLOCK).is_some_and(|o| !steam::has_condebug(&o)) {
        return Status::NoLog;
    }
    Status::State { state: addon::state(&dir, VPK), running: game_running() }
}

fn game() -> Option<PathBuf> {
    steam::app_dir(&steam_root()?, steam::DEADLOCK)
}

/// Steam's folder, as it registers it (`HKCU\Software\Valve\Steam`, `SteamPath`).
fn steam_root() -> Option<PathBuf> {
    let key = HSTRING::from(r"Software\Valve\Steam");
    let name = HSTRING::from("SteamPath");
    let mut bytes = 0u32;
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, &key, &name, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) != ERROR_SUCCESS || bytes < 2 {
            return None;
        }
        let mut buf = vec![0u16; bytes.div_ceil(2) as usize];
        if RegGetValueW(HKEY_CURRENT_USER, &key, &name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)) != ERROR_SUCCESS {
            return None;
        }
        Some(PathBuf::from(from_wstr(&buf))).filter(|p| p.is_dir())
    }
}

/// DMM's local mods store, when it's installed.
fn dmm_store() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("dev.stormix.deadlock-mod-manager").join("mods")).filter(|p| p.is_dir())
}

fn game_running() -> bool {
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more && !found {
            found = from_wstr(&entry.szExeFile).eq_ignore_ascii_case(GAME_EXE);
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}
