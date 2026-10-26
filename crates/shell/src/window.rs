//! The hidden main window a tray app hangs off: it owns the tray icon, receives hotkeys and
//! timers, and other processes find it by its class.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, FindWindowW, GetMessageW, MSG, PostMessageW, RegisterClassExW,
    RegisterWindowMessageW, TranslateMessage, WNDCLASSEXW, WNDPROC, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::instance;

/// A hidden top-level window of `class` (not message-only: those miss `TaskbarCreated`), scoped
/// to a test run's instance ([`instance::scoped`]).
pub fn hidden(class: &str, title: &str, wndproc: WNDPROC) -> Result<HWND, String> {
    let class = HSTRING::from(instance::scoped(class));
    unsafe {
        let hinst = GetModuleHandleW(None).map_err(|e| format!("GetModuleHandle: {e}"))?;
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: wndproc,
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            &class,
            &HSTRING::from(title),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowEx: {e}"))
    }
}

/// The message Explorer broadcasts after it restarts: tray icons have to be added again.
pub fn taskbar_created() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

/// Dispatches messages until `WM_QUIT`.
pub fn run() {
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Posts `msg` to the running app whose main window is of `class`; false when it isn't running.
pub fn post(class: &str, msg: u32, wparam: usize) -> bool {
    unsafe {
        match FindWindowW(&HSTRING::from(instance::scoped(class)), None) {
            Ok(h) => PostMessageW(Some(h), msg, WPARAM(wparam), LPARAM(0)).is_ok(),
            Err(_) => false,
        }
    }
}
