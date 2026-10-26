use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::log::log;
use crate::naming::Source;
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
    /// The app whose window was captured, when it was one app's (or the app in front).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// That app's exe, a full path: where its icon comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    /// How long it plays (recordings and gifs), in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
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

/// The history file (`history.jsonl`).
pub fn file() -> PathBuf {
    app_dir().join("history.jsonl")
}

/// Records a capture in glint's history.
pub fn add(entry: Entry) {
    add_to(&file(), entry);
}

/// Records a capture in the history `file` (another app's than glint's).
pub fn add_to(file: &Path, entry: Entry) {
    if let Err(e) = append(file, &entry) {
        log(&format!("history: {e}"));
    }
}

/// What the first of the history `files` that knows `path` says about it.
pub fn find_in(files: &[PathBuf], path: &Path) -> Option<Entry> {
    let id = crate::gallery::path_id(path);
    files.iter().find_map(|f| read(f).into_iter().find(|e| e.path.as_deref().is_some_and(|p| crate::gallery::path_id(p) == id)))
}

/// Every entry in the history `file`, oldest first, whether its file is still there or not.
pub fn all_in(file: &Path) -> Vec<Entry> {
    read(file)
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
    /// A capture of `kind` made now, saved at `path` (none for a link to something unsaved).
    pub fn new(kind: Kind, path: Option<&Path>, size: u64) -> Entry {
        Entry { kind, path: path.map(Path::to_path_buf), url: None, size, time: now(), app: None, exe: None, duration_ms: None }
    }

    pub fn with_url(mut self, url: &str) -> Entry {
        self.url = Some(url.to_string());
        self
    }

    /// The app it shows, and its exe when that's known.
    pub fn with_app(mut self, app: Option<&str>, exe: Option<&str>) -> Entry {
        self.app = app.filter(|a| !a.trim().is_empty()).map(str::to_string);
        self.exe = exe.filter(|e| self.app.is_some() && !e.trim().is_empty()).map(str::to_string);
        self
    }

    /// The app window `source` it shows.
    pub fn with_source(self, source: Option<&Source>) -> Entry {
        self.with_app(source.map(|s| s.app.as_str()), source.map(|s| s.path.as_str()))
    }

    pub fn with_duration(mut self, ms: Option<u32>) -> Entry {
        self.duration_ms = ms.filter(|ms| *ms > 0);
        self
    }

    pub fn exists(&self) -> bool {
        self.path.as_deref().is_some_and(Path::is_file)
    }

    pub fn is_mp4(&self) -> bool {
        self.path.as_deref().and_then(Path::extension).is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
    }

    pub fn is_gif(&self) -> bool {
        self.path.as_deref().and_then(Path::extension).is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    }

    /// When it was made: `14:02` today, `oct 3, 14:02` before.
    pub fn when(&self, local: impl Fn(u64) -> Option<Local>) -> String {
        when(local(self.time), local(now()))
    }
}

fn when(at: Option<Local>, today: Option<Local>) -> String {
    match (at, today) {
        (Some(t), Some(today)) if (t.year, t.month, t.day) == (today.year, today.month, today.day) => {
            format!("{:02}:{:02}", t.hour, t.minute)
        }
        (Some(t), _) => format!("{} {}, {:02}:{:02}", month(t.month).to_lowercase(), t.day, t.hour, t.minute),
        (None, _) => String::new(),
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
                if e.app.is_some() {
                    old.app = e.app;
                    old.exe = e.exe;
                }
                if e.duration_ms.is_some() {
                    old.duration_ms = e.duration_ms;
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
    fn when_is_a_time_today_and_a_date_before() {
        let at = |day, hour, minute| Some(Local { year: 2026, month: 10, day, hour, minute });
        assert_eq!(when(at(25, 9, 5), at(25, 23, 0)), "09:05");
        assert_eq!(when(at(3, 14, 2), at(25, 23, 0)), "oct 3, 14:02");
        assert_eq!(when(None, at(25, 23, 0)), "");
    }

    #[test]
    fn entries_round_trip_and_uploads_update_their_capture() {
        let dir = std::env::temp_dir().join(format!("glint-history-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("history.jsonl");
        let _ = std::fs::remove_file(&file);
        let shot = Entry {
            app: Some("Telegram Desktop".into()),
            exe: Some(r"C:\Telegram\Telegram.exe".into()),
            time: 10,
            ..Entry::new(Kind::Shot, Some(&dir.join("a.png")), 1200)
        };
        let rec = Entry { time: 20, ..Entry::new(Kind::Recording, Some(&dir.join("b.mp4")), 9000) };
        let link = Entry { time: 30, ..Entry::new(Kind::Shot, None, 5).with_url("https://x/1.png") };
        let uploaded = Entry { url: Some("https://x/a.png".into()), time: 40, app: None, exe: None, ..shot.clone() };
        // a line with the length a later look found
        let measured = Entry { time: 50, ..rec.clone().with_duration(Some(4_250)) };
        for e in [&shot, &rec, &link, &uploaded, &measured] {
            append(&file, e).unwrap();
        }
        std::fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(b"not json\n").unwrap();
        // a line from before apps, exes and lengths were kept
        std::fs::OpenOptions::new().append(true).open(&file).unwrap().write_all(br#"{"kind":"shot","path":"old.png","size":3,"time":5}"#).unwrap();
        let back = read(&file);
        let old = Entry { time: 5, ..Entry::new(Kind::Shot, Some(Path::new("old.png")), 3) };
        let rec_measured = Entry { duration_ms: Some(4_250), ..rec };
        assert_eq!(back, vec![Entry { url: Some("https://x/a.png".into()), ..shot }, rec_measured, link, old]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(r#""kind":"recording""#) && text.contains(r#""app":"Telegram Desktop""#));
        assert!(text.contains(r#""duration_ms":4250"#) && text.contains(r#""exe":"C:\\Telegram\\Telegram.exe""#));
        assert_eq!(text.matches(r#""app""#).count(), 1);
        assert_eq!(find_in(&[dir.join("none.jsonl"), file.clone()], &dir.join("b.mp4")).and_then(|e| e.duration_ms), Some(4_250));
        compact(&file).unwrap();
        assert_eq!(read(&file), back);
        let moved = dir.join("sub").join("b.mp4");
        relocate_in(&file, &[(dir.join("b.mp4"), moved.clone()), (dir.join("gone.png"), dir.join("x.png"))]).unwrap();
        let paths: Vec<_> = read(&file).into_iter().map(|e| e.path).collect();
        assert_eq!(paths, [Some(dir.join("a.png")), Some(moved), None, Some(PathBuf::from("old.png"))]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
