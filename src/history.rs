use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

use crate::platform::format::human_size;
use crate::platform::log::log;
use crate::platform::paths::app_dir;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Shot,
    Recording,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub size: u64,
    pub time: u64,
}

const COMPACT_AT: u64 = 256 * 1024;
const KEEP: usize = 500;

fn file() -> PathBuf {
    app_dir().join("history.jsonl")
}

pub(crate) fn add(kind: Kind, path: Option<&Path>, size: u64, url: Option<&str>) {
    let time = now();
    let entry = Entry { kind, path: path.map(Path::to_path_buf), url: url.map(str::to_string), size, time };
    if let Err(e) = append(&file(), &entry) {
        log(&format!("history: {e}"));
    }
}

pub(crate) fn recent(n: usize) -> Vec<Entry> {
    read(&file()).into_iter().rev().filter(|e| e.url.is_some() || e.exists()).take(n).collect()
}

pub(crate) fn last_recording() -> Option<PathBuf> {
    read(&file()).into_iter().rev().find(|e| e.kind == Kind::Recording && e.is_mp4() && e.exists())?.path
}

pub(crate) fn clear() {
    let _ = std::fs::remove_file(file());
}

impl Entry {
    pub(crate) fn exists(&self) -> bool {
        self.path.as_deref().is_some_and(Path::is_file)
    }

    pub(crate) fn is_mp4(&self) -> bool {
        self.path.as_deref().and_then(Path::extension).is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    }

    pub(crate) fn label(&self) -> String {
        let what = match (self.kind, self.path.as_deref().and_then(Path::extension)) {
            (Kind::Shot, _) => "Screenshot",
            (Kind::Recording, Some(e)) if e.eq_ignore_ascii_case("gif") => "GIF",
            (Kind::Recording, _) => "Recording",
        };
        let when = match (local(self.time), local(now())) {
            (Some(t), Some(today)) if (t.wYear, t.wMonth, t.wDay) == (today.wYear, today.wMonth, today.wDay) => {
                format!("{:02}:{:02}", t.wHour, t.wMinute)
            }
            (Some(t), _) => format!("{} {}, {:02}:{:02}", month(t.wMonth), t.wDay, t.wHour, t.wMinute),
            (None, _) => String::new(),
        };
        let state = if self.exists() { human_size(self.size) } else { "link only".into() };
        format!("{what} · {when} · {state}")
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn month(m: u16) -> &'static str {
    const NAMES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    NAMES.get(usize::from(m.max(1)) - 1).copied().unwrap_or("")
}

fn local(unix: u64) -> Option<SYSTEMTIME> {
    // filetime counts 100 ns ticks since 1601-01-01
    let ticks = (unix + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(local)
}

fn append(file: &Path, entry: &Entry) -> Result<(), String> {
    let mut line = serde_json::to_string(entry).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    f.write_all(line.as_bytes()).map_err(|e| format!("{}: {e}", file.display()))?;
    drop(f);
    if std::fs::metadata(file).is_ok_and(|m| m.len() > COMPACT_AT) {
        compact(file)?;
    }
    Ok(())
}

fn compact(file: &Path) -> Result<(), String> {
    let entries = read(file);
    let mut text = String::new();
    for e in &entries[entries.len().saturating_sub(KEEP)..] {
        text += &serde_json::to_string(e).map_err(|e| e.to_string())?;
        text.push('\n');
    }
    let tmp = file.with_extension("jsonl.tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, file)).map_err(|e| format!("{}: {e}", file.display()))
}

fn read(file: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    let mut entries: Vec<Entry> = Vec::new();
    for e in text.lines().filter_map(|l| serde_json::from_str::<Entry>(l).ok()) {
        match entries.iter_mut().rev().find(|old| old.path.is_some() && old.path == e.path) {
            Some(old) => {
                old.size = e.size;
                if e.url.is_some() {
                    old.url = e.url;
                }
            }
            None => entries.push(e),
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_uploads_update_their_capture() {
        let dir = std::env::temp_dir().join(format!("glint-history-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("history.jsonl");
        let _ = std::fs::remove_file(&file);
        let shot = Entry { kind: Kind::Shot, path: Some(dir.join("a.png")), url: None, size: 1200, time: 10 };
        let rec = Entry { kind: Kind::Recording, path: Some(dir.join("b.mp4")), url: None, size: 9000, time: 20 };
        let link = Entry { kind: Kind::Shot, path: None, url: Some("https://x/1.png".into()), size: 5, time: 30 };
        let uploaded = Entry { url: Some("https://x/a.png".into()), time: 40, ..shot.clone() };
        for e in [&shot, &rec, &link, &uploaded] {
            append(&file, e).unwrap();
        }
        std::fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(b"not json\n").unwrap();
        let back = read(&file);
        assert_eq!(back, vec![Entry { url: Some("https://x/a.png".into()), ..shot }, rec, link]);
        assert!(std::fs::read_to_string(&file).unwrap().contains(r#""kind":"recording""#));
        compact(&file).unwrap();
        assert_eq!(read(&file), back);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_time_converts() {
        let t = local(1_700_000_000).unwrap();
        assert_eq!((t.wYear, t.wMonth), (2023, 11));
        assert_eq!(month(11), "Nov");
    }
}
