//! Game events while the buffer runs (config `game_events`): a thread looks for a game
//! [`GAMES`] knows every few seconds, follows its [`Source`] a few times a second while it runs,
//! and keeps what happened for as long as the buffer reaches back; a saved clip takes the events
//! it covers along in its file. The tray app can [`listen`] too, for auto-clip.
//!
//! A test run ([`instance::test_run`]) with `GLINT_GAME_EXE` set follows the game that exe is
//! as if it ran from there, without a process: Deadlock's log beside a made-up install.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glint_core::events::{self, GAMES, Game, GameEvent, Now, Payload, Ring, Source};
use glint_core::log::log;
use glint_core::wide::from_wstr;
use glint_shell::instance;
use glint_shell::time::local_time;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::core::PWSTR;

use crate::buffer::Notify;

/// How often a running game's source is polled.
const POLL: Duration = Duration::from_millis(250);
/// How often the processes are looked through for a game.
const LOOK: Duration = Duration::from_secs(3);
/// How long after a clip's end its last events may still be on their way (a poll, and the game's
/// own delay in writing them).
const LATE_MS: i64 = 750;
/// Kept beyond what a save covers (s).
const SPARE_S: i64 = 30;
/// A test run's stand-in for a running game's exe.
const STAND_IN_VAR: &str = "GLINT_GAME_EXE";

static RING: LazyLock<Mutex<Ring>> = LazyLock::new(|| Mutex::new(Ring::new(0)));
/// A game is being followed: a save waits for its late events.
static FOLLOWING: AtomicBool = AtomicBool::new(false);
/// Who hears each poll's new events, as a `Vec<GameEvent>`.
static LISTENER: Mutex<Option<Notify>> = Mutex::new(None);

fn ring() -> MutexGuard<'static, Ring> {
    RING.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps events for a buffer of `length` seconds.
pub fn keep(length: u32) {
    ring().set_keep((length as i64 + SPARE_S) * 1000);
}

/// Has `to` hear the new events as they come, as a `Vec<GameEvent>` ([`Notify::post`]); `None`
/// stops that.
pub fn listen(to: Option<Notify>) {
    *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) = to;
}

/// Follows games until dropped; dropping it forgets the events kept.
pub struct Watcher {
    _stop: Sender<()>,
}

impl Watcher {
    pub fn start(length: u32) -> Watcher {
        keep(length);
        let (stop, stopped) = mpsc::channel();
        if let Err(e) = std::thread::Builder::new().name("game events".into()).spawn(move || watch(&stopped)) {
            log(&format!("game events: can't start watching: {e}"));
        }
        Watcher { _stop: stop }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        ring().clear();
    }
}

/// A game being followed.
struct Running {
    game: &'static Game,
    /// `None` for a test run's stand-in.
    process: Option<Process>,
    source: Box<dyn Source>,
    /// Its source failed and said so; quiet until it works again.
    failing: bool,
}

/// A handle to a process, closed when dropped.
struct Process(HANDLE);

impl Process {
    fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn watch(stopped: &Receiver<()>) {
    let mut running: Option<Running> = None;
    let mut heard = Vec::new();
    loop {
        let wait = if running.is_some() { POLL } else { LOOK };
        if stopped.recv_timeout(wait) != Err(RecvTimeoutError::Timeout) {
            break;
        }
        match &mut running {
            None => {
                running = stand_in().or_else(find);
                if let Some(r) = &running {
                    log(&format!("game events: following {}", r.game.name));
                    FOLLOWING.store(true, Ordering::Relaxed);
                }
            }
            Some(r) if r.process.as_ref().is_some_and(Process::exited) => {
                log(&format!("game events: {} quit", r.game.name));
                running = None;
                FOLLOWING.store(false, Ordering::Relaxed);
            }
            Some(r) => {
                let now = now();
                match r.source.poll(now, &mut heard) {
                    Ok(()) => r.failing = false,
                    Err(e) if !r.failing => {
                        log(&format!("game events: {}: {e}", r.game.name));
                        r.failing = true;
                    }
                    Err(_) => {}
                }
                if !heard.is_empty() {
                    if let Some(to) = *LISTENER.lock().unwrap_or_else(|e| e.into_inner()) {
                        to.post::<Vec<GameEvent>>(heard.clone());
                    }
                    let mut ring = ring();
                    for e in heard.drain(..) {
                        ring.push(e, now.unix_ms);
                    }
                }
            }
        }
    }
    FOLLOWING.store(false, Ordering::Relaxed);
}

/// The wall clock, with this PC's offset from UTC as it is now.
fn now() -> Now {
    let unix_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    let unix_s = unix_ms.div_euclid(1000);
    let offset = local_time(unix_s as u64).map_or(0, |l| {
        let minutes = events::days(l.year as i64, l.month as u32, l.day as u32) * 1440 + l.hour as i64 * 60 + l.minute as i64;
        minutes * 60 - (unix_s - unix_s.rem_euclid(60))
    });
    Now { unix_ms, offset }
}

/// A test run's stand-in for a running game ([`STAND_IN_VAR`]): the game whose exe it names, as
/// if started now from there by an unknown player.
fn stand_in() -> Option<Running> {
    if !instance::test_run() {
        return None;
    }
    let exe = PathBuf::from(std::env::var_os(STAND_IN_VAR)?);
    let name = exe.file_name()?.to_string_lossy().into_owned();
    let game = GAMES.iter().find(|g| name.eq_ignore_ascii_case(g.exe))?;
    let source = (game.open)(&exe, now().unix_ms, None)?;
    Some(Running { game, process: None, source, failing: false })
}

/// A running game [`GAMES`] knows, with its source.
fn find() -> Option<Running> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut more = Process32FirstW(snap, &mut entry).is_ok();
        while more && found.is_none() {
            let exe = from_wstr(&entry.szExeFile);
            if let Some(game) = GAMES.iter().find(|g| exe.eq_ignore_ascii_case(g.exe)) {
                found = open(game, entry.th32ProcessID);
            }
            more = Process32NextW(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

fn open(game: &'static Game, pid: u32) -> Option<Running> {
    let process = Process(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) }.ok()?);
    let exe = image(process.0)?;
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(process.0, &mut created, &mut exited, &mut kernel, &mut user) }.ok()?;
    let player = crate::addon::steam_root().and_then(|steam| glint_core::events::steam::persona(&steam));
    let source = (game.open)(&exe, unix_ms(created), player.as_deref())?;
    Some(Running { game, process: Some(process), source, failing: false })
}

/// The exe `process` runs.
fn image(process: HANDLE) -> Option<PathBuf> {
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.is_ok();
    ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

/// FILETIME (100 ns ticks since 1601) as unix ms.
fn unix_ms(t: FILETIME) -> i64 {
    let ticks = ((t.dwHighDateTime as i64) << 32) | t.dwLowDateTime as i64;
    (ticks - 116_444_736_000_000_000) / 10_000
}

/// Puts the events a clip covers into its finished file at `path`: one whose first frame was at
/// `wall` (unix ms), `duration` long (100 ns units). Waits a moment for the last ones while a
/// game is followed. Nothing to put in leaves the file as it is.
pub fn attach(path: &Path, wall: i64, duration: i64) {
    if FOLLOWING.load(Ordering::Relaxed) {
        let due = wall + duration / 10_000 + LATE_MS;
        let wait = due - now().unix_ms;
        if wait > 0 {
            std::thread::sleep(Duration::from_millis(wait.min(LATE_MS) as u64));
        }
    }
    let marks = ring().marks(wall, duration);
    if marks.is_empty() {
        return;
    }
    let n = marks.len();
    match events::save(path, &Payload { version: events::VERSION, events: marks }) {
        Ok(()) => log(&format!("saved {n} game event(s) for the trimmer")),
        Err(e) => log(&format!("couldn't save the game events: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetimes_turn_into_unix_time() {
        let ticks: i64 = 116_444_736_000_000_000 + 17_000_000_000_000_000 + 12_345_678;
        let t = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        assert_eq!(unix_ms(t), 1_700_000_001_234);
    }

    #[test]
    fn the_utc_offset_is_whole_minutes_and_sane() {
        let n = now();
        assert_eq!(n.offset % 60, 0);
        assert!(n.offset.abs() <= 14 * 3600, "{}", n.offset);
    }
}
