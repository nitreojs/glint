use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT, S_OK, TRUE};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetWindow, GetWindowLongW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, WS_CAPTION, WS_CLIPCHILDREN, WS_DISABLED,
    WS_EX_TOOLWINDOW, WS_SYSMENU,
};
use windows::core::{BOOL, Interface, PWSTR};

/// A program with a window, or one playing sound, by exe.
#[derive(Debug)]
pub struct App {
    pub exe: String,
    /// Where its first copy runs from, when it could be looked at.
    pub path: Option<String>,
    /// Its biggest window's title.
    pub title: String,
    pub audio: bool,
    /// Windowless processes it started that play sound, which recording it picks up too.
    pub via: Vec<String>,
    /// Copies of the exe in different folders; the name matches all of them.
    pub copies: usize,
}

#[derive(Debug)]
pub(super) struct Proc {
    pid: u32,
    parent: u32,
    exe: String,
    name: String,
}

impl Proc {
    fn new(pid: u32, parent: u32, exe: String) -> Proc {
        Proc { pid, parent, name: key(&exe), exe }
    }
}

#[derive(Debug)]
struct Window {
    pid: u32,
    title: String,
    shown: bool,
    area: i64,
}

fn key(exe: &str) -> String {
    exe.to_lowercase()
}

fn ancestors(procs: &[Proc], pid: u32) -> impl Iterator<Item = &Proc> {
    let find = move |pid: u32| procs.iter().find(|p| p.pid == pid && pid != 0);
    let mut at = find(pid);
    let mut seen = Vec::new();
    std::iter::from_fn(move || {
        let p = at?;
        seen.push(p.pid);
        at = find(p.parent).filter(|a| !seen.contains(&a.pid));
        Some(p)
    })
    .skip(1)
    .take(64)
}

// process loopback captures a whole process tree, so an app picked by name also records what its children play.
fn audio_owner<'a>(procs: &'a [Proc], windowed: &[u32], pid: u32) -> Option<&'a Proc> {
    let me = procs.iter().find(|p| p.pid == pid)?;
    ancestors(procs, pid).find(|a| windowed.contains(&a.pid) && a.name != me.name && a.name != "explorer.exe")
}

pub(super) fn roots(procs: &[Proc], exe: &str) -> Vec<u32> {
    let k = key(exe);
    let inside = |p: &Proc| ancestors(procs, p.pid).any(|a| a.name == k);
    procs.iter().filter(|p| p.name == k && !inside(p)).map(|p| p.pid).collect()
}

fn entry<'a, 'p>(apps: &'a mut HashMap<&'p str, (App, i64)>, p: &'p Proc) -> &'a mut (App, i64) {
    apps.entry(p.name.as_str()).or_insert_with(|| {
        let app = App { exe: p.exe.clone(), path: None, title: String::new(), audio: false, via: Vec::new(), copies: 1 };
        (app, i64::MIN)
    })
}

fn build(procs: &[Proc], windows: &[Window], audio: &[u32], me: u32, path: impl Fn(u32) -> Option<String>) -> Vec<App> {
    let find = |pid: u32| procs.iter().find(|p| p.pid == pid && pid != 0);
    let own = find(me).map(|p| p.name.as_str());
    let proc_of = |pid: u32| find(pid).filter(|p| Some(p.name.as_str()) != own);
    let mut apps: HashMap<&str, (App, i64)> = HashMap::new();
    for w in windows {
        let Some(p) = proc_of(w.pid) else { continue };
        let score = if w.shown { w.area } else { w.area - i64::MAX / 2 };
        let (app, best) = entry(&mut apps, p);
        if score > *best {
            *best = score;
            app.title = w.title.clone();
        }
    }
    let windowed: Vec<u32> = windows.iter().map(|w| w.pid).collect();
    for &pid in audio {
        let Some(p) = proc_of(pid) else { continue };
        entry(&mut apps, p).0.audio = true;
        if let Some(owner) = audio_owner(procs, &windowed, pid).and_then(|o| proc_of(o.pid)) {
            let via = &mut entry(&mut apps, owner).0.via;
            if !via.iter().any(|v| key(v) == p.name) {
                via.push(p.exe.clone());
            }
        }
    }
    let mut out: Vec<App> = apps.into_values().map(|(a, _)| a).collect();
    for app in &mut out {
        let paths: Vec<String> = roots(procs, &app.exe).into_iter().filter_map(&path).collect();
        let mut folders: Vec<String> = paths.iter().map(|p| key(p)).collect();
        folders.sort();
        folders.dedup();
        app.copies = folders.len().max(1);
        app.path = paths.into_iter().next();
    }
    sort(&mut out);
    out
}

fn sort(apps: &mut [App]) {
    apps.sort_by(|a, b| b.audio.cmp(&a.audio).then_with(|| key(&a.exe).cmp(&key(&b.exe))));
}

pub(super) fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            let exe = String::from_utf16_lossy(&e.szExeFile[..len]);
            out.push(Proc::new(e.th32ProcessID, e.th32ParentProcessID, exe));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

unsafe fn text(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn class(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

unsafe fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn windows() -> Vec<Window> {
    unsafe extern "system" fn hosted(child: HWND, lp: LPARAM) -> BOOL {
        let (frame, found) = unsafe { &mut *(lp.0 as *mut (u32, u32)) };
        let pid = unsafe { pid_of(child) };
        if pid != *frame {
            *found = pid;
            return BOOL(0);
        }
        TRUE
    }
    unsafe extern "system" fn add(hwnd: HWND, lp: LPARAM) -> BOOL {
        let list = unsafe { &mut *(lp.0 as *mut Vec<Window>) };
        unsafe {
            if GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid())
                || GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0
            {
                return TRUE;
            }
            let title = text(hwnd);
            if title.is_empty() {
                return TRUE;
            }
            let mut cloaked = 0u32;
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as _, 4);
            let shown = IsWindowVisible(hwnd).as_bool() && cloaked == 0;
            let frame = WS_CAPTION.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0;
            let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
            if !shown && (style & frame != frame || style & WS_DISABLED.0 != 0) {
                return TRUE;
            }
            let mut pid = pid_of(hwnd);
            // uwp apps draw inside a frame window owned by ApplicationFrameHost.exe.
            if class(hwnd) == "ApplicationFrameWindow" {
                let mut probe = (pid, 0u32);
                let _ = EnumChildWindows(Some(hwnd), Some(hosted), LPARAM(&mut probe as *mut (u32, u32) as isize));
                if probe.1 == 0 {
                    return TRUE;
                }
                pid = probe.1;
            }
            let mut rc = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rc);
            let area = i64::from((rc.right - rc.left).max(0)) * i64::from((rc.bottom - rc.top).max(0));
            list.push(Window { pid, title, shown, area });
        }
        TRUE
    }
    let mut list = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(add), LPARAM(&mut list as *mut Vec<Window> as isize));
    }
    list
}

// needs com on the calling thread.
unsafe fn audio_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return pids;
        };
        let Ok(all) = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) else { return pids };
        for i in 0..all.GetCount().unwrap_or(0) {
            let Ok(mgr) = all.Item(i).and_then(|d| d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)) else {
                continue;
            };
            let Ok(sessions) = mgr.GetSessionEnumerator() else { continue };
            for j in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(s) = sessions.GetSession(j).and_then(|s| s.cast::<IAudioSessionControl2>()) else { continue };
                if s.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                if let Ok(pid) = s.GetProcessId()
                    && pid != 0
                    && !pids.contains(&pid)
                {
                    pids.push(pid);
                }
            }
        }
    }
    pids
}

fn scan() -> Vec<App> {
    unsafe {
        let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let audio = audio_pids();
        if com {
            CoUninitialize();
        }
        build(&processes(), &windows(), &audio, GetCurrentProcessId(), image_path)
    }
}

pub fn running_apps(done: impl FnOnce() + Send + 'static) -> Receiver<Vec<App>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("glint-apps".into()).spawn(move || {
        if tx.send(scan()).is_ok() {
            done();
        }
    });
    if spawned.is_err() {
        glint_core::log::log("apps: couldn't start the app scan thread");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, exe: &str) -> Proc {
        Proc::new(pid, parent, exe.into())
    }

    fn w(pid: u32, title: &str, shown: bool, area: i64) -> Window {
        Window { pid, title: title.into(), shown, area }
    }

    #[test]
    fn child_audio_goes_to_the_windowed_ancestor() {
        let procs = [
            p(1, 0, "explorer.exe"),
            p(10, 1, "chrome.exe"),
            p(11, 10, "chrome.exe"),
            p(20, 1, "ms-teams.exe"),
            p(21, 20, "msedgewebview2.exe"),
            p(22, 21, "msedgewebview2.exe"),
            p(30, 1, "game.exe"),
        ];
        let windowed = [1, 10, 20];
        let owner = |pid| audio_owner(&procs, &windowed, pid).map(|p| p.pid);
        assert_eq!(owner(11), None);
        assert_eq!(owner(22), Some(20));
        assert_eq!(owner(30), None);
        assert_eq!(owner(99), None);
    }

    #[test]
    fn ancestors_survive_pid_cycles() {
        let procs = [p(5, 6, "a.exe"), p(6, 5, "b.exe")];
        assert_eq!(ancestors(&procs, 5).map(|p| p.pid).collect::<Vec<_>>(), [6]);
    }

    #[test]
    fn roots_are_the_top_of_each_tree() {
        let procs = [
            p(1, 0, "explorer.exe"),
            p(10, 1, "discord.exe"),
            p(11, 10, "discord.exe"),
            p(12, 11, "discord.exe"),
            p(20, 1, "discord.exe"),
            p(30, 10, "helper.exe"),
            p(31, 30, "discord.exe"),
            p(40, 1, "Telegram.exe"),
            p(41, 40, "telegram.exe"),
        ];
        assert_eq!(roots(&procs, "discord.exe"), [10, 20]);
        assert_eq!(roots(&procs, "TELEGRAM.exe"), [40]);
        assert_eq!(roots(&procs, "obs64.exe"), [0u32; 0]);
    }

    #[test]
    fn groups_by_exe_with_the_main_window_and_audio_first() {
        let procs = [
            p(1, 0, "explorer.exe"),
            p(10, 1, "Code.exe"),
            p(11, 10, "Code.exe"),
            p(20, 1, "Telegram.exe"),
            p(30, 1, "Telegram.exe"),
            p(40, 1, "Spotify.exe"),
            p(41, 40, "Spotify.exe"),
            p(50, 1, "ms-teams.exe"),
            p(51, 50, "msedgewebview2.exe"),
            p(60, 61, "glint.exe"),
            p(61, 1, "glint.exe"),
        ];
        let windows = [
            w(10, "small", true, 10),
            w(11, "main.rs - glint", true, 1000),
            w(20, "Telegram", false, 5000),
            w(30, "Telegram (2)", true, 100),
            w(40, "Spotify Premium", false, 0),
            w(50, "Teams", true, 50),
            w(60, "settings", true, 50),
            w(61, "glint", false, 50),
        ];
        let path = |pid: u32| match pid {
            20 => Some(r"C:\Telegram\Telegram.exe".into()),
            30 => Some(r"D:\tele\Telegram.exe".into()),
            10 => Some(r"C:\Code\Code.exe".into()),
            _ => None,
        };
        let apps = build(&procs, &windows, &[41, 51, 61], 60, path);
        let names: Vec<&str> = apps.iter().map(|a| a.exe.as_str()).collect();
        assert_eq!(names, ["msedgewebview2.exe", "Spotify.exe", "Code.exe", "ms-teams.exe", "Telegram.exe"]);
        let get = |exe: &str| apps.iter().find(|a| a.exe == exe).unwrap();
        assert_eq!(get("Code.exe").title, "main.rs - glint");
        assert_eq!(get("Code.exe").copies, 1);
        assert_eq!(get("Telegram.exe").title, "Telegram (2)");
        assert_eq!(get("Telegram.exe").copies, 2);
        assert!(get("Spotify.exe").audio);
        assert_eq!(get("ms-teams.exe").via, ["msedgewebview2.exe"]);
        assert!(!get("ms-teams.exe").audio);
        assert_eq!(get("Telegram.exe").path.as_deref(), Some(r"C:\Telegram\Telegram.exe"));
        assert_eq!(get("Spotify.exe").path, None);
    }

    #[test]
    fn sorts_audio_first_then_by_name_ignoring_case() {
        let app = |exe: &str, audio| App { exe: exe.into(), path: None, title: String::new(), audio, via: Vec::new(), copies: 1 };
        let mut apps = vec![app("zed.exe", false), app("Brave.exe", true), app("alpha.exe", false), app("vlc.exe", true)];
        sort(&mut apps);
        let names: Vec<&str> = apps.iter().map(|a| a.exe.as_str()).collect();
        assert_eq!(names, ["Brave.exe", "vlc.exe", "alpha.exe", "zed.exe"]);
    }
}
