//! Which app a window belongs to, for naming captures after it.

use std::path::Path;

use glint_core::naming::Source;
use glint_core::wide::wide;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, TRUE};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetWindowTextW, GetWindowThreadProcessId};
use windows::core::{BOOL, PCWSTR, PWSTR, w};

/// Store apps' windows belong to this host; the app itself owns a child window.
const FRAME_HOST: &str = "ApplicationFrameHost.exe";

/// The app behind `hwnd` (its exe, the exe's description, the window title); None when the
/// process can't be looked at (gone, or elevated while this app isn't).
pub fn identify(hwnd: HWND) -> Option<Source> {
    let mut path = image_path(pid_of(hwnd)?)?;
    if file_name(&path).eq_ignore_ascii_case(FRAME_HOST)
        && let Some(hosted) = hosted_app(hwnd)
    {
        path = hosted;
    }
    let exe = file_name(&path).to_string();
    let stem = Path::new(&exe).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let app = description(&path).filter(|d| !d.trim().is_empty()).unwrap_or(stem);
    Some(Source { exe, app: app.trim().to_string(), title: title(hwnd) })
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn pid_of(hwnd: HWND) -> Option<u32> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
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

/// The image of the first child window owned by another process than the frame host's.
fn hosted_app(frame: HWND) -> Option<String> {
    struct Search {
        host: u32,
        found: Option<String>,
    }
    unsafe extern "system" fn visit(child: HWND, lp: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lp.0 as *mut Search) };
        match pid_of(child).filter(|&pid| pid != search.host).and_then(image_path) {
            Some(path) => {
                search.found = Some(path);
                BOOL(0)
            }
            None => TRUE,
        }
    }
    let mut search = Search { host: pid_of(frame)?, found: None };
    unsafe {
        let _ = EnumChildWindows(Some(frame), Some(visit), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

/// The exe's FileDescription, in the first language its version resource lists.
fn description(path: &str) -> Option<String> {
    let file = wide(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(file.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(file.as_ptr()), None, size, data.as_mut_ptr().cast()).ok()?;
        let mut langs = Vec::new();
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        if VerQueryValueW(data.as_ptr().cast(), w!("\\VarFileInfo\\Translation"), &mut ptr, &mut len).as_bool()
            && !ptr.is_null()
        {
            let pairs = std::slice::from_raw_parts(ptr as *const u16, len as usize / 2);
            langs.extend(pairs.chunks_exact(2).map(|p| format!("{:04x}{:04x}", p[0], p[1])));
        }
        langs.extend(["040904b0".to_string(), "040904e4".to_string()]);
        langs.into_iter().find_map(|lang| {
            let key = wide(&format!("\\StringFileInfo\\{lang}\\FileDescription"));
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            let found = VerQueryValueW(data.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut ptr, &mut len).as_bool();
            if !found || ptr.is_null() || len == 0 {
                return None;
            }
            let text = std::slice::from_raw_parts(ptr as *const u16, len as usize);
            let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            Some(String::from_utf16_lossy(&text[..end]))
        })
    }
}

fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exes_name_their_app() {
        assert_eq!(file_name(r"C:\Windows\explorer.exe"), "explorer.exe");
        assert_eq!(file_name("plain.exe"), "plain.exe");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let notepad = description(&format!(r"{system}\System32\notepad.exe")).unwrap();
        assert!(!notepad.is_empty() && !notepad.contains('\0'), "{notepad:?}");
        assert_eq!(description(r"C:\nowhere\missing.exe"), None);
        assert_eq!(identify(HWND(std::ptr::null_mut())), None);
    }
}
