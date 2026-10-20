//! A low-level keyboard hook sees every key before global hotkeys fire (PrintScreen included,
//! which never reaches a window as WM_KEYDOWN), so it can record combos other apps have taken.
//! Key-downs are swallowed while recording; key-ups pass through so no key gets stuck.
//!
//! The hook lives in a windowless helper, `glint.exe --capture-keys`: installed in this process
//! it was never called while the settings window had focus (Windows 10), though a hook in any
//! other process saw the same keys. The helper reports over stdout and quits when its stdin
//! closes, so it can't outlive the settings window.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use super::{SettingsApp, repaint};
use crate::app::ipc::{self, WM_HOTKEY_PAUSE};
use crate::config::Config;
use crate::config::hotkeys::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, hotkey_string, parse_hotkey};

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

/// `glint.exe --capture-keys`: hooks the keyboard until stdin closes or the process is killed.
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
pub(super) fn key_names(hotkey: &str) -> Vec<String> {
    hotkey.split('+').map(|k| k.trim().to_lowercase()).collect()
}

/// The modifiers held right now, as keycap labels.
pub(super) fn held_names() -> Vec<String> {
    let held = HELD.load(Ordering::SeqCst);
    [(MOD_CONTROL, "ctrl"), (MOD_ALT, "alt"), (MOD_SHIFT, "shift"), (MOD_WIN, "win")]
        .into_iter()
        .filter(|(bit, _)| held & bit != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// The running capture helper; its reports land in [`HELD`] and [`CAPTURED`].
pub(super) struct Listener {
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

/// The two hotkeys the general section edits.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum HotkeyField {
    Record,
    Screenshot,
}

impl HotkeyField {
    fn name(self) -> &'static str {
        match self {
            HotkeyField::Record => "record",
            HotkeyField::Screenshot => "screenshot",
        }
    }

    fn other(self) -> HotkeyField {
        match self {
            HotkeyField::Record => HotkeyField::Screenshot,
            HotkeyField::Screenshot => HotkeyField::Record,
        }
    }

    pub(super) fn of(self, cfg: &mut Config) -> &mut String {
        match self {
            HotkeyField::Record => &mut cfg.hotkey,
            HotkeyField::Screenshot => &mut cfg.screenshot_hotkey,
        }
    }
}

impl SettingsApp {
    pub(super) fn start_listening(&mut self, field: HotkeyField) {
        if self.listening.is_some() {
            return;
        }
        self.listen_for = field;
        HELD.store(0, Ordering::SeqCst);
        *CAPTURED.lock().unwrap() = None;
        self.hotkey_error = None;
        // The tray app lets go of its hotkey so pressing it records it instead of firing.
        ipc::post_to_main(WM_HOTKEY_PAUSE, 1);
        match Listener::start() {
            Ok(l) => self.listening = Some(l),
            Err(e) => {
                ipc::post_to_main(WM_HOTKEY_PAUSE, 0);
                self.error = Some(format!("can't record keys: {e}"));
            }
        }
    }

    pub(super) fn stop_listening(&mut self) {
        self.hotkey_error = None;
        if let Some(l) = self.listening.take() {
            l.stop();
            ipc::post_to_main(WM_HOTKEY_PAUSE, 0);
        }
    }

    pub(super) fn poll_capture(&mut self) {
        if self.listening.is_none() {
            return;
        }
        let Some((mods, vk)) = CAPTURED.lock().unwrap().take() else {
            return;
        };
        // Esc / Backspace on their own cancel.
        if mods == 0 && (vk == 0x1B || vk == 0x08) {
            return self.stop_listening();
        }
        // Anything that can't be used keeps listening, so the next combo retries.
        let Some(text) = hotkey_string(mods, vk) else {
            self.hotkey_error = Some((Vec::new(), "that key can't be part of a hotkey. try another.".into()));
            return;
        };
        let keys = key_names(&text);
        let standalone = (0x70..=0x87).contains(&vk) || matches!(vk, 0x13 | 0x2C | 0x91); // F1-F24, Pause, PrtSc, ScrLk
        if mods == 0 && !standalone {
            let msg = format!("{} alone would fire while you type. add ctrl, alt, shift or win.", keys[0]);
            self.hotkey_error = Some((keys, msg));
            return;
        }
        if !hotkey_available(mods, vk) {
            let msg = format!("{} is already used by another app. try a different shortcut.", keys.join(" + "));
            self.hotkey_error = Some((keys, msg));
            return;
        }
        let other = self.listen_for.other();
        if parse_hotkey(other.of(&mut self.cfg)).is_ok_and(|combo| combo == (mods, vk)) {
            let msg = format!("{} is already the {} hotkey.", keys.join(" + "), other.name());
            self.hotkey_error = Some((keys, msg));
            return;
        }
        self.stop_listening();
        *self.listen_for.of(&mut self.cfg) = text;
    }
}

/// Whether nobody else holds this combo (the tray app has released its own meanwhile).
fn hotkey_available(mods: u32, vk: u32) -> bool {
    const PROBE_ID: i32 = 0x6C69;
    unsafe {
        let ok = RegisterHotKey(None, PROBE_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok();
        if ok {
            let _ = UnregisterHotKey(None, PROBE_ID);
        }
        ok
    }
}
