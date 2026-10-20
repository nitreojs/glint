use std::path::PathBuf;

use windows::Win32::Foundation::RECT;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Shell::{FOLDERID_Videos, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// Axis-aligned rectangle in physical pixels (exclusive right/bottom).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn from_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Rect { x: x0.min(x1), y: y0.min(y1), w: (x1 - x0).abs(), h: (y1 - y0).abs() }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn area(&self) -> i64 {
        self.w.max(0) as i64 * self.h.max(0) as i64
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }
    pub fn inflate(&self, d: i32) -> Rect {
        Rect { x: self.x - d, y: self.y - d, w: self.w + 2 * d, h: self.h + 2 * d }
    }
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
    pub fn to_win(self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
    pub fn from_win(r: &RECT) -> Rect {
        Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
    }
}

/// NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `s` into a fixed-size UTF-16 buffer, truncating and NUL-terminating.
pub fn copy_wstr(dst: &mut [u16], s: &str) {
    let max = dst.len().saturating_sub(1);
    let mut n = 0;
    for c in s.encode_utf16().take(max) {
        dst[n] = c;
        n += 1;
    }
    dst[n] = 0;
}

pub fn from_wstr(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

/// `%APPDATA%\glint`, created on demand.
pub fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = base.join("glint");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn videos_dir() -> PathBuf {
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as _));
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Videos")).unwrap_or_else(std::env::temp_dir)
}

/// Local date and time as (`YYYY-MM-DD`, `HH-MM-SS`).
pub fn date_time() -> (String, String) {
    let t = unsafe { GetLocalTime() };
    (format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay), format!("{:02}-{:02}-{:02}", t.wHour, t.wMinute, t.wSecond))
}

/// Local time as `YYYY-MM-DD_HH-MM-SS`.
pub fn timestamp() -> String {
    let (date, time) = date_time();
    format!("{date}_{time}")
}

pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= 1024.0 * MB {
        format!("{:.2} GB", b / (1024.0 * MB))
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.0} KB", b / 1024.0)
    }
}

pub fn human_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Appends a line to `%APPDATA%\glint\glint.log`.
pub fn log(msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "[{}] {}", timestamp(), msg);
    }
}

pub fn log_path() -> PathBuf {
    app_dir().join("glint.log")
}
