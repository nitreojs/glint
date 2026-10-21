use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::format::human_size;
use crate::log::log;
use crate::paths::app_dir;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Shot,
    Recording,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub size: u64,
    pub time: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
}

const COMPACT_AT: u64 = 256 * 1024;
const KEEP: usize = 500;

fn file() -> PathBuf {
    app_dir().join("history.jsonl")
}

pub fn add(kind: Kind, path: Option<&Path>, size: u64, url: Option<&str>) {
    let time = now();
    let entry = Entry { kind, path: path.map(Path::to_path_buf), url: url.map(str::to_string), size, time };
    if let Err(e) = append(&file(), &entry) {
        log(&format!("history: {e}"));
    }
}

pub fn recent(n: usize) -> Vec<Entry> {
    read(&file()).into_iter().rev().filter(|e| e.url.is_some() || e.exists()).take(n).collect()
}

pub fn last_recording() -> Option<PathBuf> {
    read(&file()).into_iter().rev().find(|e| e.kind == Kind::Recording && e.is_mp4() && e.exists())?.path
}

pub fn clear() {
    let _ = std::fs::remove_file(file());
}

/// Points entries at the files' new places after `moved` (from, to) were moved.
pub fn relocate(moved: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    relocate_in(&file(), moved)
}

fn relocate_in(file: &Path, moved: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    let mut entries = read(file);
    let mut changed = false;
    for e in &mut entries {
        if let Some((_, to)) = moved.iter().find(|(from, _)| e.path.as_ref() == Some(from)) {
            e.path = Some(to.clone());
            changed = true;
        }
    }
    if !changed {
        return Ok(());
    }
    write(file, &entries)
}

impl Entry {
    pub fn exists(&self) -> bool {
        self.path.as_deref().is_some_and(Path::is_file)
    }

    pub fn is_mp4(&self) -> bool {
        self.path.as_deref().and_then(Path::extension).is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    }

    pub fn label(&self, local: impl Fn(u64) -> Option<Local>) -> String {
        let what = match (self.kind, self.path.as_deref().and_then(Path::extension)) {
            (Kind::Shot, _) => "Screenshot",
            (Kind::Recording, Some(e)) if e.eq_ignore_ascii_case("gif") => "GIF",
            (Kind::Recording, _) => "Recording",
        };
        let when = match (local(self.time), local(now())) {
            (Some(t), Some(today)) if (t.year, t.month, t.day) == (today.year, today.month, today.day) => {
                format!("{:02}:{:02}", t.hour, t.minute)
            }
            (Some(t), _) => format!("{} {}, {:02}:{:02}", month(t.month), t.day, t.hour, t.minute),
            (None, _) => String::new(),
        };
        let state = if self.exists() { human_size(self.size) } else { "link only".into() };
        format!("{what} · {when} · {state}")
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn month(m: u16) -> &'static str {
    const NAMES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    NAMES.get(usize::from(m.max(1)) - 1).copied().unwrap_or("")
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
    write(file, &entries[entries.len().saturating_sub(KEEP)..])
}

fn write(file: &Path, entries: &[Entry]) -> Result<(), String> {
    let mut text = String::new();
    for e in entries {
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
        let moved = dir.join("sub").join("b.mp4");
        relocate_in(&file, &[(dir.join("b.mp4"), moved.clone()), (dir.join("gone.png"), dir.join("x.png"))]).unwrap();
        let paths: Vec<_> = read(&file).into_iter().map(|e| e.path).collect();
        assert_eq!(paths, [Some(dir.join("a.png")), Some(moved), None]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
