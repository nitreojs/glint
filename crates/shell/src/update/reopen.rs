//! An app's windows that run as processes of their own (settings, the gallery) keep running the
//! old exe through an update: the restart closes them and asks the new version (`--reopen
//! settings`) to open them again, once the old ones are gone.

use std::path::Path;
use std::time::{Duration, Instant};

use glint_core::log::log;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

use crate::identity::{image_path, pid_of};

/// Asks a new version to open a window again: `--reopen settings`, the window's argument without
/// its dashes (an app reading `--settings` anywhere in its arguments would start as that window).
pub const REOPEN_ARG: &str = "--reopen";

/// How long the new version waits for the old window to close before giving up on it.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// A window of the app's own: its title and the argument that starts it.
#[derive(Clone, Debug)]
pub struct OwnWindow {
    pub title: String,
    pub arg: &'static str,
}

/// The windows among `windows` that are open and run from this exe (not another install's).
pub(super) fn open_ones(windows: &[OwnWindow]) -> Vec<(HWND, &'static str)> {
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    windows
        .iter()
        .filter_map(|w| {
            let h = crate::window::find(&w.title)?;
            let path = pid_of(h).and_then(image_path)?;
            same_file(Path::new(&path), &exe).then_some((h, w.arg))
        })
        .collect()
}

/// The arguments asking the new version to open `open` again.
pub(super) fn args(open: &[(HWND, &'static str)]) -> Vec<String> {
    open.iter().flat_map(|&(_, arg)| [REOPEN_ARG.to_string(), name(arg).to_string()]).collect()
}

pub(super) fn close(open: &[(HWND, &'static str)]) {
    for &(h, _) in open {
        unsafe {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The windows `args` ask to open again, in `windows`' order.
fn requested<'a>(args: &[String], windows: &'a [OwnWindow]) -> Vec<&'a OwnWindow> {
    let asked: Vec<&str> = args.windows(2).filter(|p| p[0] == REOPEN_ARG).map(|p| p[1].as_str()).collect();
    windows.iter().filter(|w| asked.contains(&name(w.arg))).collect()
}

fn name(arg: &str) -> &str {
    arg.trim_start_matches('-')
}

/// Opens the windows `args` ask for again, each once the old version's is gone.
pub fn reopen(args: &[String], windows: &[OwnWindow]) {
    let wanted: Vec<OwnWindow> = requested(args, windows).into_iter().cloned().collect();
    if wanted.is_empty() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    std::thread::spawn(move || {
        let since = Instant::now();
        for w in wanted {
            while crate::window::find(&w.title).is_some() && since.elapsed() < CLOSE_WAIT {
                std::thread::sleep(Duration::from_millis(50));
            }
            if crate::window::find(&w.title).is_some() {
                log(&format!("update: the old {} is still open, not opening another", w.title));
                continue;
            }
            if let Err(e) = std::process::Command::new(&exe).arg(w.arg).spawn() {
                log(&format!("update: can't open {} again: {e}", w.title));
            }
        }
    });
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn own() -> Vec<OwnWindow> {
        vec![
            OwnWindow { title: "glint settings".into(), arg: "--settings" },
            OwnWindow { title: "glint gallery".into(), arg: "--gallery" },
        ]
    }

    #[test]
    fn only_the_windows_asked_for_are_opened_again() {
        let args: Vec<String> = ["glint.exe", "--updated", "0.3.0", "--reopen", "gallery"].map(String::from).into();
        let w = own();
        let titles: Vec<&str> = requested(&args, &w).iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, ["glint gallery"]);
        let none: Vec<String> = ["glint.exe", "--gallery"].map(String::from).into();
        assert!(requested(&none, &w).is_empty(), "a plain --gallery isn't a reopen");
    }

    #[test]
    fn the_new_version_is_asked_by_name_never_by_the_windows_own_argument() {
        let args = args(&[(HWND::default(), "--settings"), (HWND::default(), "--gallery")]);
        assert_eq!(args, ["--reopen", "settings", "--reopen", "gallery"]);
        assert!(!args.iter().any(|a| a == "--settings" || a == "--gallery"), "the tray app would start as that window");
    }
}
