//! Keystrokes for the overlay, from a low-level keyboard hook on its own thread. Windows skips
//! hooks that answer slowly, so the hook callback only stamps the event and queues it; a worker
//! thread resolves each key to a label (modifiers, dead-key-safe characters, a password skip)
//! well off the hook. Mirrors [`super::mouse`], split in two because the resolving is heavier.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use glint_core::config::Keystrokes;
use glint_core::log::log;
use glint_core::record::keys::{KeyLabel, Mods, Named, Stroke};
use glint_core::record::pipeline::Keys;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{CUIAutomation, CUIAutomation8, IUIAutomation, IUIAutomation2};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, GetKeyboardLayout, HKL, ToUnicodeEx, VIRTUAL_KEY, VK_BACK, VK_CAPITAL, VK_CONTROL,
    VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F24, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_MENU, VK_NEXT, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
    VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetMessageW,
    GetWindowThreadProcessId, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
};
use windows::core::Interface;

use super::win::{hns, qpc};

const POLL_MS: u64 = 10;
/// UIA waits this long for a provider to connect or answer before failing.
const UIA_MS: u32 = 120;
/// `ToUnicodeEx` without disturbing the keyboard state (dead keys), Windows 10 1607+.
const NO_STATE_CHANGE: u32 = 0x4;

struct Raw {
    vk: u16,
    scan: u32,
    down: bool,
    qpc: i64,
}

thread_local! {
    static QUEUE: RefCell<Option<Sender<Raw>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn on_key(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if down || up {
            let raw = Raw { vk: kb.vkCode as u16, scan: kb.scanCode, down, qpc: qpc() };
            QUEUE.with_borrow(|q| {
                if let Some(q) = q {
                    let _ = q.send(raw);
                }
            });
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

/// Pumps the low-level keyboard hook and quits when the worker posts `WM_QUIT` to this thread.
fn hook_loop(tx: Sender<Raw>, id: Sender<u32>) {
    QUEUE.set(Some(tx));
    unsafe {
        let module = GetModuleHandleW(None).ok().map(Into::into);
        match SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), module, 0) {
            Ok(hook) => {
                let _ = id.send(GetCurrentThreadId());
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    DispatchMessageW(&msg);
                }
                if let Err(e) = UnhookWindowsHookEx(hook) {
                    log(&format!("removing the keyboard hook failed: {e}"));
                }
            }
            Err(e) => log(&format!("keystroke overlay unavailable: SetWindowsHookExW failed: {e}")),
        }
    }
    QUEUE.set(None);
}

pub(super) struct Hook {
    level: Keystrokes,
}

impl Hook {
    pub(super) fn new(level: Keystrokes) -> Hook {
        Hook { level }
    }
}

impl Keys for Hook {
    fn run(&mut self, stop: &AtomicBool, listening: &dyn Fn() -> bool, out: &mut dyn FnMut(Stroke)) {
        let (tx, rx) = mpsc::channel::<Raw>();
        let (id_tx, id_rx) = mpsc::channel::<u32>();
        let hook = thread::Builder::new().name("glint-keys-hook".into()).spawn(move || hook_loop(tx, id_tx));
        let hook = match hook {
            Ok(h) => h,
            Err(e) => {
                log(&format!("keystroke overlay unavailable: {e}"));
                return;
            }
        };
        let hook_tid = id_rx.recv_timeout(Duration::from_secs(5)).ok();
        let mut mods = current_mods();
        let mut pw = Password::new();
        while !stop.load(Ordering::SeqCst) {
            let raw = match rx.recv_timeout(Duration::from_millis(POLL_MS)) {
                Ok(raw) => raw,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // Track modifiers from the stream even while paused, so the state stays right.
            if let Some(m) = modifier(raw.vk) {
                mods.set(m, raw.down);
                continue;
            }
            if !listening() {
                continue;
            }
            let at = hns(raw.qpc);
            if !raw.down {
                out(Stroke { vk: raw.vk, down: false, at, mods, main: KeyLabel::Text(String::new()) });
                continue;
            }
            if let Some((main, keep)) = resolve(raw.vk, raw.scan, mods, self.level, &mut pw) {
                out(Stroke { vk: raw.vk, down: true, at, mods: keep, main });
            }
        }
        if let Some(tid) = hook_tid {
            let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
        let _ = hook.join();
    }
}

#[derive(Clone, Copy)]
enum Mod {
    Ctrl,
    Alt,
    Shift,
    Win,
}

trait Set {
    fn set(&mut self, m: Mod, down: bool);
}

impl Set for Mods {
    fn set(&mut self, m: Mod, down: bool) {
        match m {
            Mod::Ctrl => self.ctrl = down,
            Mod::Alt => self.alt = down,
            Mod::Shift => self.shift = down,
            Mod::Win => self.win = down,
        }
    }
}

fn modifier(vk: u16) -> Option<Mod> {
    match VIRTUAL_KEY(vk) {
        VK_LSHIFT | VK_RSHIFT | VK_SHIFT => Some(Mod::Shift),
        VK_LCONTROL | VK_RCONTROL | VK_CONTROL => Some(Mod::Ctrl),
        VK_LMENU | VK_RMENU | VK_MENU => Some(Mod::Alt),
        VK_LWIN | VK_RWIN => Some(Mod::Win),
        _ => None,
    }
}

fn named(vk: u16) -> Option<Named> {
    Some(match VIRTUAL_KEY(vk) {
        VK_RETURN => Named::Enter,
        VK_ESCAPE => Named::Esc,
        VK_TAB => Named::Tab,
        VK_BACK => Named::Backspace,
        VK_DELETE => Named::Delete,
        VK_SPACE => Named::Space,
        VK_LEFT => Named::Left,
        VK_RIGHT => Named::Right,
        VK_UP => Named::Up,
        VK_DOWN => Named::Down,
        VK_HOME => Named::Home,
        VK_END => Named::End,
        VK_PRIOR => Named::PageUp,
        VK_NEXT => Named::PageDown,
        VK_INSERT => Named::Insert,
        VK_SNAPSHOT => Named::PrintScreen,
        _ => return None,
    })
}

fn func(vk: u16) -> Option<u8> {
    (VK_F1.0..=VK_F24.0).contains(&vk).then(|| (vk - VK_F1.0 + 1) as u8)
}

/// Resolves a key-down to its label and the modifiers to store, or `None` when the level or a
/// password field means it is not recorded.
fn resolve(vk: u16, scan: u32, mods: Mods, level: Keystrokes, pw: &mut Password) -> Option<(KeyLabel, Mods)> {
    if let Some(n) = named(vk) {
        // Space is noisy on its own; record it only as part of a combo.
        if n == Named::Space && !mods.any() {
            return None;
        }
        return Some((KeyLabel::Named(n), mods));
    }
    if let Some(f) = func(vk) {
        return Some((KeyLabel::Func(f), mods));
    }
    let hkl = foreground_layout();
    if mods.chord() {
        // The base character, so Ctrl+Shift+S reads "S" rather than a control code.
        let base = to_char(vk, scan, false, false, hkl)?.to_uppercase();
        return Some((KeyLabel::Text(base), mods));
    }
    // A bare printable: only `all` records these, and never inside a password field.
    if level != Keystrokes::All || pw.is_password() {
        return None;
    }
    let s = to_char(vk, scan, mods.shift, caps_on(), hkl)?;
    Some((KeyLabel::Text(s), Mods::default()))
}

/// The character `vk` produces under `shift`/`caps`, without disturbing the keyboard state.
fn to_char(vk: u16, scan: u32, shift: bool, caps: bool, hkl: HKL) -> Option<String> {
    let mut state = [0u8; 256];
    if shift {
        state[VK_SHIFT.0 as usize] = 0x80;
    }
    if caps {
        state[VK_CAPITAL.0 as usize] = 0x01;
    }
    let mut buf = [0u16; 8];
    let n = unsafe { ToUnicodeEx(vk as u32, scan, &state, &mut buf, NO_STATE_CHANGE, Some(hkl)) };
    if n <= 0 {
        return None; // 0 = no translation, <0 = dead key (left uncommitted)
    }
    let s = String::from_utf16_lossy(&buf[..n as usize]);
    (!s.is_empty() && !s.chars().any(char::is_control)).then_some(s)
}

fn held(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

fn current_mods() -> Mods {
    Mods { ctrl: held(VK_CONTROL), alt: held(VK_MENU), shift: held(VK_SHIFT), win: held(VK_LWIN) || held(VK_RWIN) }
}

fn caps_on() -> bool {
    unsafe { GetKeyState(VK_CAPITAL.0 as i32) & 1 != 0 }
}

fn foreground_layout() -> HKL {
    unsafe { GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), None)) }
}

/// The focused window right now (the control within the foreground thread, or the window).
fn focus() -> isize {
    unsafe {
        let fg = GetForegroundWindow();
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gui = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        if GetGUIThreadInfo(tid, &mut gui).is_ok() && !gui.hwndFocus.0.is_null() {
            gui.hwndFocus.0 as isize
        } else {
            fg.0 as isize
        }
    }
}

/// Whether the focused control is a password field, cached until the focus changes. UI Automation
/// runs here on the worker thread, never in the hook callback.
struct Password {
    uia: Option<IUIAutomation>,
    at: isize,
    value: bool,
}

impl Password {
    fn new() -> Password {
        let uia: Option<IUIAutomation> = unsafe {
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
                .or_else(|_| CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))
                .ok()
        };
        if let Some(a) = &uia
            && let Ok(a2) = a.cast::<IUIAutomation2>()
        {
            unsafe {
                let _ = a2.SetConnectionTimeout(UIA_MS);
                let _ = a2.SetTransactionTimeout(UIA_MS);
            }
        }
        Password { uia, at: 0, value: false }
    }

    fn is_password(&mut self) -> bool {
        let now = focus();
        if now != self.at {
            self.at = now;
            self.value = self.query();
        }
        self.value
    }

    fn query(&self) -> bool {
        let Some(uia) = &self.uia else { return false };
        unsafe {
            match uia.GetFocusedElement() {
                Ok(el) => el.CurrentIsPassword().map(|b| b.as_bool()).unwrap_or(false),
                Err(_) => false,
            }
        }
    }
}
