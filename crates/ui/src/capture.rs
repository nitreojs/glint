//! Recording a new hotkey. A low-level keyboard hook sees every key before global hotkeys fire
//! (PrintScreen included, which never reaches a window as WM_KEYDOWN), so it can record combos
//! other apps have taken. Key-downs are swallowed while recording; key-ups pass through so no
//! key gets stuck.
//!
//! The hook lives in a windowless helper, the app's exe run with [`CAPTURE_ARG`]: installed in
//! the settings process it was never called while the settings window had focus (Windows 10),
//! though a hook in any other process saw the same keys. The helper reports over stdout and
//! quits when its stdin closes, so it can't outlive the settings window.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use glint_core::config::hotkeys::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, hotkey_string, parse_hotkey};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use crate::settings::repaint;

pub const CAPTURE_ARG: &str = "--capture-keys";

/// Modifier keys currently held, as MOD_* bits.
static HELD: AtomicU32 = AtomicU32::new(0);
/// (modifiers, virtual key) of the first non-modifier key pressed while recording.
static CAPTURED: Mutex<Option<(u32, u32)>> = Mutex::new(None);

fn modifier_bit(vk: u32) -> u32 {
    match vk {
        0x10 | 0xA0 | 0xA1 => MOD_SHIFT,
        0x11 | 0xA2 | 0xA3 => MOD_CONTROL,
        0x12 | 0xA4 | 0xA5 => MOD_ALT,
        0x5B | 0x5C => MOD_WIN,
        _ => 0,
    }
}

/// Helper side: one line per event, flushed at once.
fn report(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

unsafe extern "system" fn capture_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let down = wp.0 == WM_KEYDOWN as usize || wp.0 == WM_SYSKEYDOWN as usize;
            let bit = modifier_bit(kb.vkCode);
            let held = HELD.load(Ordering::SeqCst);
            if bit != 0 {
                let now = if down { held | bit } else { held & !bit };
                if now != held {
                    HELD.store(now, Ordering::SeqCst);
                    report(&format!("held {now}"));
                }
            } else if down {
                report(&format!("key {held} {}", kb.vkCode));
            }
            if down {
                return LRESULT(1);
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// The helper (`<app>.exe` with [`CAPTURE_ARG`]): hooks the keyboard until stdin closes or the
/// process is killed.
pub fn capture_keys() {
    std::thread::spawn(|| {
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]); // EOF (or no stdin at all)
        std::process::exit(0);
    });
    unsafe {
        let module = GetModuleHandleW(None).ok().map(HINSTANCE::from);
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(capture_hook), module, 0) else { return };
        report("ready");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        let _ = UnhookWindowsHookEx(hook);
    }
}

/// "Ctrl+Alt+PrintScreen" as keycap labels.
pub fn key_names(hotkey: &str) -> Vec<String> {
    hotkey.split('+').map(|k| k.trim().to_lowercase()).collect()
}

/// The modifiers held right now, as keycap labels.
pub fn held_names() -> Vec<String> {
    let held = HELD.load(Ordering::SeqCst);
    [(MOD_CONTROL, "ctrl"), (MOD_ALT, "alt"), (MOD_SHIFT, "shift"), (MOD_WIN, "win")]
        .into_iter()
        .filter(|(bit, _)| held & bit != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// The running capture helper; its reports land in [`HELD`] and [`CAPTURED`].
struct Listener {
    child: std::process::Child,
}

impl Listener {
    fn start() -> Result<Listener, String> {
        use std::io::BufRead;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(exe)
            .arg(CAPTURE_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut lines = std::io::BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
        // Keys pressed before the hook is in place would slip through, so wait for it.
        if !matches!(lines.next(), Some(Ok(l)) if l == "ready") {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the key capture helper didn't start".into());
        }
        std::thread::spawn(move || {
            for line in lines.map_while(Result::ok) {
                let mut words = line.split(' ');
                let kind = words.next();
                let nums: Vec<u32> = words.filter_map(|w| w.parse().ok()).collect();
                match (kind, nums.as_slice()) {
                    (Some("held"), &[mods]) => HELD.store(mods, Ordering::SeqCst),
                    (Some("key"), &[mods, vk]) => {
                        let mut c = CAPTURED.lock().unwrap();
                        if c.is_none() {
                            *c = Some((mods, vk));
                        }
                    }
                    _ => continue,
                }
                repaint();
            }
        });
        Ok(Listener { child })
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Records one hotkey at a time. `release(true)` asks the tray app to let go of its hotkeys
/// while recording (so pressing one records it instead of firing), `release(false)` to take
/// them back.
pub struct Recorder {
    listening: Option<Listener>,
    release: fn(bool),
    /// Keys and reason of the last combo that can't be used; recording goes on.
    error: Option<(Vec<String>, String)>,
}

impl Recorder {
    pub fn new(release: fn(bool)) -> Recorder {
        Recorder { listening: None, release, error: None }
    }

    pub fn listening(&self) -> bool {
        self.listening.is_some()
    }

    /// Why the last combo pressed can't be used, while still recording.
    pub fn error(&self) -> Option<&(Vec<String>, String)> {
        self.error.as_ref()
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.listening.is_some() {
            return Ok(());
        }
        HELD.store(0, Ordering::SeqCst);
        *CAPTURED.lock().unwrap() = None;
        self.error = None;
        (self.release)(true);
        match Listener::start() {
            Ok(l) => {
                self.listening = Some(l);
                Ok(())
            }
            Err(e) => {
                (self.release)(false);
                Err(format!("can't record keys: {e}"))
            }
        }
    }

    pub fn stop(&mut self) {
        self.error = None;
        if let Some(l) = self.listening.take() {
            l.stop();
            (self.release)(false);
        }
    }

    /// The combo just pressed, once it's one that can be used; `others` are the app's other
    /// hotkeys as (name, combo). Esc or Backspace alone cancel.
    pub fn poll(&mut self, others: &[(&str, &str)]) -> Option<String> {
        self.listening.as_ref()?;
        let (mods, vk) = CAPTURED.lock().unwrap().take()?;
        if mods == 0 && (vk == 0x1B || vk == 0x08) {
            self.stop();
            return None;
        }
        // Anything that can't be used keeps recording, so the next combo retries.
        let Some(text) = hotkey_string(mods, vk) else {
            self.error = Some((Vec::new(), "that key can't be in a hotkey".into()));
            return None;
        };
        let keys = key_names(&text);
        let standalone = (0x70..=0x87).contains(&vk) || matches!(vk, 0x13 | 0x2C | 0x91); // F1-F24, Pause, PrtSc, ScrLk
        if mods == 0 && !standalone {
            let msg = "would fire while you type: add ctrl, alt, shift or win".to_owned();
            self.error = Some((keys, msg));
            return None;
        }
        if !available(mods, vk) {
            let msg = "another app already uses it".to_owned();
            self.error = Some((keys, msg));
            return None;
        }
        if let Some((name, _)) = others.iter().find(|(_, combo)| parse_hotkey(combo).is_ok_and(|c| c == (mods, vk))) {
            let msg = format!("already the {name} hotkey");
            self.error = Some((keys, msg));
            return None;
        }
        self.stop();
        Some(text)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Whether nobody else holds this combo (the tray app has released its own meanwhile).
fn available(mods: u32, vk: u32) -> bool {
    const PROBE_ID: i32 = 0x6C69;
    unsafe {
        let ok = RegisterHotKey(None, PROBE_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok();
        if ok {
            let _ = UnregisterHotKey(None, PROBE_ID);
        }
        ok
    }
}
