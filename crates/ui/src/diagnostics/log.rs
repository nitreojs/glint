use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

use eframe::egui;

use glint_core::diagnostics::{Category, Event, Level, Row};
use glint_core::format::human_duration;

const TAIL_BYTES: u64 = 4 << 20;

pub(super) struct Entry {
    pub date: String,
    pub time: String,
    pub raw: String,
    pub summary: String,
    pub category: Category,
    pub level: Level,
    pub event: Option<Event>,
    search: String,
}

impl Entry {
    fn new(line: &str) -> Self {
        let row = Row::parse(line);
        let duration = |ms| human_duration(f64::from(ms) / 1000.0);
        let summary = match &row.event {
            Some(Event::ReplaySaved { id, elapsed_ms, .. }) => {
                format!("Replay #{id} saved in {:.1} s", *elapsed_ms as f64 / 1000.0)
            }
            Some(Event::ReplayShortened { duration_ms, requested_s, .. }) => {
                format!("Saved {} of {}", duration(*duration_ms), human_duration(f64::from(*requested_s)))
            }
            Some(Event::ReplayTimedOut { id, seconds, .. }) => {
                format!("Replay #{id} didn’t finish in {seconds} s")
            }
            Some(Event::ReplayFailed { id, .. }) => format!("Replay #{id} couldn’t be saved"),
            None => row.message.to_owned(),
        };
        let search = format!("{} {summary} {}", row.raw, row.category.label()).to_lowercase();
        let time = if row.time.is_empty() { "—".into() } else { row.time.replace('-', ":") };

        Self {
            date: row.date.to_owned(), time, raw: row.raw.to_owned(), summary, category: row.category,
            level: row.level, event: row.event, search,
        }
    }

    pub fn status(&self) -> &'static str {
        match self.event {
            Some(Event::ReplayShortened { .. }) => "△ shortened",
            Some(Event::ReplayTimedOut { .. }) => "× timed out",
            _ => match self.level {
                Level::Info => self.category.label(),
                Level::Success => "✓ saved",
                Level::Warning => "△ warning",
                Level::Error => "× error",
            },
        }
    }
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub query: String,
    pub category: Option<Category>,
    pub level: Option<Level>,
}

impl Filter {
    pub fn matches(&self, entry: &Entry, words: &[&str]) -> bool {
        self.category.is_none_or(|category| category == entry.category)
            && self.level.is_none_or(|level| level == entry.level)
            && words.iter().all(|word| entry.search.contains(word))
    }
}

#[derive(Default)]
pub(super) struct Snapshot {
    pub entries: Vec<Entry>,
    pub shortened: bool,
    pub error: Option<String>,
    pub file_exists: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl Snapshot {
    fn read(path: &Path, previous: &mut Option<Stamp>) -> io::Result<Option<Self>> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        let stamp = Stamp { len: meta.len(), modified: meta.modified().ok() };
        if *previous == Some(stamp) {
            return Ok(None);
        }
        let start = stamp.len.saturating_sub(TAIL_BYTES);
        file.seek(SeekFrom::Start(start.saturating_sub(1)))?;
        let mut bytes = Vec::new();
        file.take(TAIL_BYTES + 1).read_to_end(&mut bytes)?;
        let mut bytes = bytes.as_slice();
        if start > 0 {
            bytes = bytes.iter().position(|&byte| byte == b'\n').map_or(&[], |i| &bytes[i + 1..]);
        }
        // An append can race this read; only complete lines have stable message boundaries.
        bytes = bytes.iter().rposition(|&byte| byte == b'\n').map_or(&[], |i| &bytes[..=i]);
        let entries = String::from_utf8_lossy(bytes).lines().filter(|line| !line.is_empty()).map(Entry::new).collect();
        *previous = Some(stamp);

        Ok(Some(Self { entries, shortened: start > 0, file_exists: true, error: None }))
    }
}

pub(super) struct Watch {
    pub updates: Receiver<Snapshot>,
    _stop: mpsc::Sender<()>,
}

pub(super) fn watch(path: PathBuf, ctx: egui::Context) -> Watch {
    let (stop, stopped) = mpsc::channel();
    let (tx, rx) = mpsc::sync_channel(1);
    let errors = tx.clone();
    let handle = std::thread::Builder::new().name("diagnostics log".into()).spawn(move || {
        let mut stamp = None;
        let mut failure = None;
        loop {
            let next = match Snapshot::read(&path, &mut stamp) {
                Ok(snapshot) => {
                    failure = None;
                    snapshot
                }
                Err(error) => {
                    stamp = None;
                    let missing = error.kind() == io::ErrorKind::NotFound;
                    let message = if missing { String::new() } else { error.to_string() };
                    if failure.as_ref() == Some(&message) {
                        None
                    } else {
                        let error = (!missing).then(|| message.clone());
                        failure = Some(message);
                        Some(Snapshot { error, ..Snapshot::default() })
                    }
                }
            };
            if let Some(snapshot) = next {
                if tx.send(snapshot).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
            if stopped.recv_timeout(Duration::from_millis(750)) != Err(mpsc::RecvTimeoutError::Timeout) {
                return;
            }
        }
    });
    if let Err(error) = handle {
        let _ = errors.send(Snapshot { error: Some(error.to_string()), ..Snapshot::default() });
    }

    Watch { updates: rx, _stop: stop }
}
