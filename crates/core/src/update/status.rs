//! What an app's tray process last learned about updates, written to `status.json` in its update
//! folder for its settings window to show.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::version::Version;

pub const STATUS: &str = "status.json";

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Status {
    /// Local time of the last finished check, `2026-10-21 14:32`.
    #[serde(default)]
    pub checked: Option<String>,
    #[serde(default)]
    pub state: State,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum State {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available {
        version: String,
    },
    Downloading {
        version: String,
        /// How much of the exe has arrived, 0-100.
        #[serde(default)]
        percent: u8,
    },
    Ready {
        version: String,
    },
    Failed {
        error: String,
    },
}

impl State {
    /// A check or download is under way.
    pub fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading { .. })
    }

    /// The newer version this state is about, if any.
    pub fn version(&self) -> Option<&str> {
        match self {
            State::Available { version } | State::Downloading { version, .. } | State::Ready { version } => Some(version),
            _ => None,
        }
    }
}

impl Status {
    /// The status at `path`; anything unreadable is [`State::Unknown`].
    pub fn load_from(path: &Path) -> Status {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    /// Writes the status to `path` through a temporary file, so readers never see half of it.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        let tmp = path.with_extension("json.tmp");
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&tmp, serde_json::to_string(self).unwrap_or_default()))
            .and_then(|_| std::fs::rename(&tmp, path))
            .map_err(|e| format!("can't write {}: {e}", path.display()))
    }

    /// Left by an earlier run of a build at `current` and no longer true: a check that never
    /// finished, a version that isn't newer anymore, or a ready update whose file is gone.
    pub fn stale(&self, current: &Version, ready_file_exists: impl Fn(&str) -> bool) -> bool {
        let newer = |v: &str| Version::parse(v).is_some_and(|v| v > *current);
        match &self.state {
            State::Checking | State::Downloading { .. } => true,
            State::Available { version } => !newer(version),
            State::Ready { version } => !newer(version) || !ready_file_exists(version),
            State::Unknown | State::UpToDate | State::Failed { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_round_trip_and_older_files_still_read() {
        let dir = std::env::temp_dir().join(format!("glint-core-status-{}", std::process::id()));
        let path = dir.join(STATUS);
        let s = Status { checked: Some("2026-10-21 14:32".into()), state: State::Downloading { version: "0.2.0".into(), percent: 42 } };
        s.save_to(&path).unwrap();
        assert_eq!(Status::load_from(&path), s);
        // written before downloads had a percentage
        std::fs::write(&path, r#"{"checked":null,"state":{"kind":"downloading","version":"0.2.0"}}"#).unwrap();
        assert_eq!(Status::load_from(&path).state, State::Downloading { version: "0.2.0".into(), percent: 0 });
        std::fs::write(&path, "nope").unwrap();
        assert_eq!(Status::load_from(&path), Status::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_statuses_are_spotted() {
        let current = Version::parse("0.2.0").unwrap();
        let at = |state| Status { checked: None, state };
        let v = |s: &str| s.to_string();
        assert!(at(State::Checking).stale(&current, |_| true));
        assert!(at(State::Downloading { version: v("0.3.0"), percent: 5 }).stale(&current, |_| true));
        assert!(at(State::Available { version: v("0.2.0") }).stale(&current, |_| true));
        assert!(!at(State::Available { version: v("0.3.0") }).stale(&current, |_| true));
        assert!(!at(State::Ready { version: v("0.3.0") }).stale(&current, |_| true));
        assert!(at(State::Ready { version: v("0.3.0") }).stale(&current, |_| false));
        assert!(!at(State::Failed { error: v("x") }).stale(&current, |_| false));
        assert_eq!(at(State::Ready { version: v("0.3.0") }).state.version(), Some("0.3.0"));
        assert!(at(State::Checking).state.busy() && !at(State::UpToDate).state.busy());
    }
}
