use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const MARKER: &str = "\t@diagnostic ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Replay,
    Recording,
    Screenshot,
    Audio,
    Update,
    Gallery,
    Other,
}

impl Category {
    pub const ALL: [Self; 7] = [Self::Replay, Self::Recording, Self::Screenshot, Self::Audio, Self::Update, Self::Gallery, Self::Other];

    pub fn label(self) -> &'static str {
        match self {
            Self::Replay => "replay saves",
            Self::Recording => "recording",
            Self::Screenshot => "screenshots",
            Self::Audio => "audio",
            Self::Update => "updates",
            Self::Gallery => "gallery",
            Self::Other => "other",
        }
    }
}

/// Structured replay events are stored on the same line as their human-readable message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    ReplaySaved { id: u64, duration_ms: u32, elapsed_ms: u64, path: PathBuf },
    ReplayShortened { id: u64, duration_ms: u32, requested_s: u32, elapsed_ms: u64, path: PathBuf },
    ReplayTimedOut { id: u64, seconds: u64, folder: PathBuf },
    ReplayFailed { id: u64, folder: PathBuf },
}

impl Event {
    pub fn line(&self, message: &str) -> String {
        let message = message.replace(['\r', '\n'], " ");
        match serde_json::to_string(self) {
            Ok(json) => format!("{message}{MARKER}{json}"),
            Err(_) => message,
        }
    }

    fn level(&self) -> Level {
        match self {
            Self::ReplaySaved { .. } => Level::Success,
            Self::ReplayShortened { .. } => Level::Warning,
            Self::ReplayTimedOut { .. } | Self::ReplayFailed { .. } => Level::Error,
        }
    }
}

#[derive(Debug)]
pub struct Row<'a> {
    pub raw: &'a str,
    pub date: &'a str,
    pub time: &'a str,
    pub message: &'a str,
    pub level: Level,
    pub category: Category,
    pub event: Option<Event>,
}

impl<'a> Row<'a> {
    pub fn parse(raw: &'a str) -> Self {
        let (date, time, body) = timestamp(raw).unwrap_or(("", "", raw));
        let (message, event) = match body.rsplit_once(MARKER) {
            Some((message, json)) => (message, serde_json::from_str::<Event>(json).ok()),
            None => (body, None),
        };
        let (level, category) = event.as_ref().map_or_else(|| legacy(message), |event| (event.level(), Category::Replay));

        Self { raw: raw.trim_end_matches('\r'), date, time, message, level, category, event }
    }
}

fn timestamp(raw: &str) -> Option<(&str, &str, &str)> {
    let (stamp, message) = raw.strip_prefix('[')?.split_once("] ")?;
    let (date, time) = stamp.split_once('_')?;
    if date.len() != 10 || time.len() != 8
        || !date.bytes().chain(time.bytes()).all(|b| b.is_ascii_digit() || b == b'-')
    {
        return None;
    }

    Some((date, time, message))
}

fn legacy(message: &str) -> (Level, Category) {
    use Category::*;
    use Level::*;

    let begins = |prefixes: &[&str]| prefixes.iter().any(|prefix| message.starts_with(prefix));
    if let Some(save) = message.strip_prefix("auto-clip: ").unwrap_or(message).strip_prefix("save #")
        && let Some((_, outcome)) = save.split_once([' ', ':'])
    {
        let level = if outcome.starts_with("done in ") {
            Success
        } else if outcome.starts_with("failed:")
            || outcome.starts_with("failed after it timed out:")
            || outcome.starts_with("timed out after ")
            || outcome.starts_with("never finished (")
        {
            Error
        } else {
            Info
        };
        return (level, Replay);
    }
    if begins(&["the memory cap (", "save asked for while "]) {
        return (Warning, Replay);
    }
    if begins(&["auto-clip:", "buffering the last ", "can't start the buffer:"]) {
        return (if message.starts_with("can't start") { Error } else { Info }, Replay);
    }
    if begins(&["recording failed:", "start failed:"]) {
        return (Error, Recording);
    }
    if begins(&["recording:", "finished:", "video:"]) {
        return (Info, Recording);
    }
    if begins(&["screenshot:", "pin:"]) {
        return (Error, Screenshot);
    }
    if begins(&["audio:"]) {
        return (Info, Audio);
    }
    if begins(&["update check failed:"]) {
        return (Error, Update);
    }
    if begins(&["update check:", "checking for updates", "installing the downloaded ", "installed ", "update to ", "discarding the downloaded "]) {
        let error = message.starts_with("update to ") && message.split_once(" failed: ").is_some();
        return (if error { Error } else { Info }, Update);
    }
    if begins(&["gallery: can't ", "gallery: couldn't "]) {
        return (Error, Gallery);
    }
    if message.starts_with("gallery:") {
        return (Info, Gallery);
    }

    (Info, Other)
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
