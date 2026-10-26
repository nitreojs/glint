//! One running copy per app and user session. Launched again without arguments of its own (the
//! Start menu, the exe, a shortcut), an app hands over to the running copy, which opens its gallery.

use std::ffi::OsStr;
use std::sync::LazyLock;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW, RegisterWindowMessageW,
};
use windows::core::{HSTRING, w};

use crate::autostart;

/// Test runs set this to run a tray app of their own beside the user's: it's added to the
/// single-instance mutex's name and the main window's class, and that app leaves the user's
/// registry and Start menu alone ([`test_run`]).
const INSTANCE_VAR: &str = "GLINT_INSTANCE";

/// The tag a test run gave this instance, if any.
fn tag() -> Option<String> {
    std::env::var(INSTANCE_VAR).ok().filter(|t| !t.is_empty())
}

/// A test run's tray app beside the user's: it doesn't sync launch at sign-in, the explorer menu
/// or the Start menu shortcuts, which are the user's.
pub fn test_run() -> bool {
    tag().is_some()
}

/// `name` for this instance: as is, unless a test run gave it a [`INSTANCE_VAR`] of its own.
pub fn scoped(name: &str) -> String {
    match tag() {
        Some(tag) => format!("{name}-{tag}"),
        None => name.to_string(),
    }
}

/// Takes `app`'s single-instance mutex for the life of the process. Right after an update the
/// previous process is still exiting, so `wait` keeps trying for a while.
pub fn take(app: &str, wait: bool) -> bool {
    let name = HSTRING::from(format!("Local\\{}-single-instance", scoped(app)));
    for _ in 0..200 {
        unsafe {
            let mutex = CreateMutexW(None, false, &name);
            if GetLastError() != ERROR_ALREADY_EXISTS {
                return true;
            }
            if let Ok(h) = mutex {
                let _ = CloseHandle(h);
            }
        }
        if !wait {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// What a launch does when the app is running already.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Again {
    /// Started plainly (the Start menu, the exe, a shortcut): the running copy opens its gallery.
    Gallery,
    /// Started at sign-in while it runs: nothing to say.
    Quiet,
    /// Started with arguments of its own: say it's running.
    Notice,
}

/// What a launch with `args` (past the exe) does while the app runs.
pub fn again<S: AsRef<OsStr>>(args: &[S]) -> Again {
    if args.is_empty() {
        Again::Gallery
    } else if args.iter().any(|a| a.as_ref() == autostart::ARG) {
        Again::Quiet
    } else {
        Again::Notice
    }
}

static GALLERY_MESSAGE: LazyLock<u32> = LazyLock::new(|| unsafe { RegisterWindowMessageW(w!("glint-open-gallery")) });

/// What a second launch posts to the running copy's main window: open the gallery, or bring it
/// forward. 0 when it couldn't be registered.
pub fn gallery_message() -> u32 {
    *GALLERY_MESSAGE
}

/// Asks the running app whose main window is of `class` to open its gallery, letting it come to
/// the front as this launch could; false when that window isn't there.
pub fn open_gallery(class: &str) -> bool {
    let msg = gallery_message();
    let Ok(main) = (unsafe { FindWindowW(&HSTRING::from(scoped(class)), None) }) else {
        return false;
    };
    if msg == 0 {
        return false;
    }
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(main, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        PostMessageW(Some(main), msg, WPARAM(0), LPARAM(0)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_launch_opens_the_gallery_and_others_keep_their_way() {
        let none: [&str; 0] = [];
        assert_eq!(again(&none), Again::Gallery);
        assert_eq!(again(&[autostart::ARG]), Again::Quiet);
        assert_eq!(again(&["--updated", "1.2.3", autostart::ARG]), Again::Quiet);
        assert_eq!(again(&["--updated", "1.2.3"]), Again::Notice);
        assert_eq!(again(&[r"C:\clips\a.mp4"]), Again::Notice);
    }

    #[test]
    fn the_gallery_message_is_registered_once() {
        let msg = gallery_message();
        assert!(msg >= 0xC000, "registered messages start at 0xC000, got {msg:#x}");
        assert_eq!(gallery_message(), msg);
    }
}
