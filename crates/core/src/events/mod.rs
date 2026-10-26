//! Game events (kills, deaths, match phases …) from sources that follow a running game, kept for
//! a while and saved with the clips they happened in, for the trimmer to show as markers.
//!
//! [`GameEvent::at`] is wall-clock time (unix ms): sources only know the wall clock. A saved
//! clip's events ([`Payload`]) are in 100 ns units on its timeline (zero = the first frame), like
//! its clicks; [`Ring::marks`] converts, given the clip's wall-clock start
//! ([`crate::record::replay::Clip::wall`]).

use std::collections::VecDeque;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::record::mp4;

pub mod addon;
pub mod deadlock;
pub mod steam;
pub mod tail;

/// moov/udta box holding a clip's game events as JSON ([`Payload`]).
pub const BOX: [u8; 4] = *b"glev";
/// [`Payload::version`] this build writes and the newest it reads.
pub const VERSION: u32 = 1;

/// 100 ns units in a millisecond.
const MS: i64 = 10_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Kill,
    Death,
    Assist,
    Respawn,
    /// The match moved on: `GameInProgress`, `PostGame` …, as the game names it.
    Phase(String),
    Objective(String),
    Custom(String),
}

/// What happened, without when.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// [`Game::name`] of the game it happened in.
    pub game: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub victim: Option<String>,
    /// Who it's about when that's one hero (the one who respawned).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hero: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assists: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounty: Option<u32>,
    /// The local player's own (their kill, their death, their respawn); `None`: can't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mine: Option<bool>,
}

impl Event {
    pub fn new(game: &str, kind: Kind) -> Event {
        Event { game: game.into(), kind, killer: None, victim: None, hero: None, assists: None, bounty: None, mine: None }
    }
}

/// An event as a source reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameEvent {
    /// Unix time (ms).
    pub at: i64,
    pub event: Event,
}

/// An event on a clip's timeline.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    /// 100 ns units from the clip's first frame.
    pub t: i64,
    #[serde(flatten)]
    pub event: Event,
}

/// What the `glev` box holds.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    pub version: u32,
    pub events: Vec<Mark>,
}

/// The wall clock as sources need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    /// Unix time (ms).
    pub unix_ms: i64,
    /// Local time minus UTC (s), for sources that stamp in local time.
    pub offset: i64,
}

/// A local time stamp to the second, without a year (`10/26 18:55:31`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar.
pub fn days(year: i64, month: u32, day: u32) -> i64 {
    let (m, d) = (month as i64, day as i64);
    let y = if m <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

impl Stamp {
    /// Seconds from 1970 to the stamp in `year`, on the clock it was stamped with.
    fn seconds(self, year: i64) -> i64 {
        days(year, self.month, self.day) * 86_400 + (self.hour * 3600 + self.minute * 60 + self.second) as i64
    }
}

impl Now {
    /// When a line stamped `stamp` that arrived now was written (unix ms). A stamp of this second
    /// or the one before (stamps are cut to the second, and the line may have waited for a poll)
    /// means now, which is finer; an older one (a backlog read late) means that second. The
    /// year is the one that puts the stamp closest to now.
    pub fn place(self, stamp: Stamp) -> i64 {
        let local = self.unix_ms.div_euclid(1000) + self.offset;
        let year = 1970 + local.div_euclid(31_556_952);
        let closest = (year - 1..=year + 1).map(|y| stamp.seconds(y)).min_by_key(|s| (local - s).abs());
        let lag = local - closest.expect("three years");
        if lag <= 1 { self.unix_ms } else { self.unix_ms - lag * 1000 }
    }
}

/// Something that hears about a game's events: a log it follows, an endpoint the game posts to,
/// an API it asks. Polled a few times a second while its game runs.
pub trait Source: Send {
    /// Appends the events since the last poll to `out`, oldest first.
    fn poll(&mut self, now: Now, out: &mut Vec<GameEvent>) -> Result<(), String>;
}

/// A game events can come from: the process that tells it's running, and its source.
pub struct Game {
    /// What [`Event::game`] says.
    pub name: &'static str,
    /// The executable's file name, any case.
    pub exe: &'static str,
    /// The source for the game running from `exe`, started at `started` (unix ms), played by
    /// `player` (the name others see, when known); `None` when this install can't give events.
    pub open: Open,
}

/// [`Game::open`].
pub type Open = fn(exe: &Path, started: i64, player: Option<&str>) -> Option<Box<dyn Source>>;

/// Every game a watcher looks for.
pub const GAMES: &[Game] = &[deadlock::GAME];

/// The events of the last `keep`, oldest first.
#[derive(Debug, Default)]
pub struct Ring {
    /// ms
    keep: i64,
    list: VecDeque<GameEvent>,
}

impl Ring {
    pub fn new(keep_ms: i64) -> Ring {
        Ring { keep: keep_ms, list: VecDeque::new() }
    }

    pub fn set_keep(&mut self, keep_ms: i64) {
        self.keep = keep_ms;
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Adds `e` in time order and forgets what's older than `keep` before `now` (unix ms).
    pub fn push(&mut self, e: GameEvent, now: i64) {
        let from = now - self.keep;
        if e.at >= from {
            let i = self.list.partition_point(|x| x.at <= e.at);
            self.list.insert(i, e);
        }
        while self.list.front().is_some_and(|x| x.at < from) {
            self.list.pop_front();
        }
    }

    /// The events of a clip whose first frame was at `wall` (unix ms) and that plays for
    /// `duration` (100 ns units), on its timeline.
    pub fn marks(&self, wall: i64, duration: i64) -> Vec<Mark> {
        let end = wall + duration.div_euclid(MS);
        let from = self.list.partition_point(|x| x.at < wall);
        let to = self.list.partition_point(|x| x.at <= end);
        self.list.range(from..to).map(|e| Mark { t: ((e.at - wall) * MS).min(duration), event: e.event.clone() }).collect()
    }
}

/// Stores `payload` in the finalized MP4 at `path` (moov/udta/glev).
pub fn save(path: &Path, payload: &Payload) -> Result<(), String> {
    let json = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    mp4::set_movie_box(path, &BOX, &json)
}

/// The game events saved in the MP4 at `path`; `None` when it has none.
pub fn load(path: &Path) -> Result<Option<Payload>, String> {
    let Some(json) = mp4::movie_box(path, &BOX)? else { return Ok(None) };
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let v: Versioned = serde_json::from_slice(&json).map_err(|e| format!("game events: {e}"))?;
    if v.version > VERSION {
        return Err(format!("game events were saved by a newer glint (format {})", v.version));
    }
    serde_json::from_slice(&json).map(Some).map_err(|e| format!("game events: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 10_000_000;

    fn at(ms: i64, kind: Kind) -> GameEvent {
        GameEvent { at: ms, event: Event::new("test", kind) }
    }

    #[test]
    fn days_count_from_1970() {
        assert_eq!(days(1970, 1, 1), 0);
        assert_eq!(days(2000, 3, 1), 11_017);
        assert_eq!(days(2026, 10, 26), 20_752);
        assert_eq!(days(1969, 12, 31), -1);
    }

    #[test]
    fn stamps_place_on_arrival_unless_read_late() {
        // 2026-10-26 18:55:31.400 at UTC+3
        let offset = 3 * 3600;
        let unix = (days(2026, 10, 26) * 86_400 + 18 * 3600 + 55 * 60 + 31 - offset) * 1000 + 400;
        let now = Now { unix_ms: unix, offset };
        let stamp = |month, day, hour, minute, second| Stamp { month, day, hour, minute, second };
        assert_eq!(now.place(stamp(10, 26, 18, 55, 31)), unix, "this second: now");
        assert_eq!(now.place(stamp(10, 26, 18, 55, 30)), unix, "the one before: now");
        assert_eq!(now.place(stamp(10, 26, 18, 55, 25)), unix - 6000, "a backlog: its second");
        assert_eq!(now.place(stamp(10, 26, 18, 55, 33)), unix, "ahead of the clock: now");
        assert_eq!(now.place(stamp(10, 25, 18, 55, 31)), unix - 86_400_000);

        let new_year = Now { unix_ms: (days(2027, 1, 1) * 86_400 + 5 - offset) * 1000, offset };
        assert_eq!(new_year.place(stamp(12, 31, 23, 59, 55)), new_year.unix_ms - 10_000, "last year's");
    }

    #[test]
    fn the_ring_keeps_its_window_in_order_and_cuts_clip_spans() {
        let mut r = Ring::new(60_000);
        r.push(at(1_000, Kind::Kill), 1_000);
        r.push(at(5_000, Kind::Death), 5_000);
        r.push(at(3_000, Kind::Respawn), 5_000);
        r.push(at(-70_000, Kind::Assist), 5_000);
        assert_eq!(r.list.iter().map(|e| e.at).collect::<Vec<_>>(), [1_000, 3_000, 5_000], "in order, the stale one dropped");
        r.push(at(62_000, Kind::Kill), 62_000);
        assert_eq!(r.list.front().map(|e| e.at), Some(3_000), "a minute back from the newest");

        // a clip whose first frame was at 2.5 s, 3 s long
        let marks = r.marks(2_500, 3 * S);
        assert_eq!(marks.iter().map(|m| (m.t, m.event.kind.clone())).collect::<Vec<_>>(), [(S / 2, Kind::Respawn), (5 * S / 2, Kind::Death)]);
        assert!(r.marks(6_000, S).is_empty());
        r.set_keep(1_000);
        r.push(at(62_500, Kind::Death), 62_500);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn events_survive_a_trip_through_the_file() {
        let path = std::env::temp_dir().join(format!("glint_events_{}.mp4", std::process::id()));
        let atom = |kind: &[u8], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
        let moov = atom(b"moov", &atom(b"mvhd", &[0; 100]));
        std::fs::write(&path, [atom(b"ftyp", b"isom"), atom(b"mdat", &[7; 64]), moov].concat()).unwrap();
        assert_eq!(load(&path), Ok(None));
        let kill = Event { killer: Some("hero_hornet".into()), victim: Some("hero_atlas".into()), assists: Some(1), bounty: Some(416), mine: Some(true), ..Event::new("deadlock", Kind::Kill) };
        let payload = Payload {
            version: VERSION,
            events: vec![Mark { t: S, event: kill }, Mark { t: 2 * S, event: Event::new("deadlock", Kind::Phase("PostGame".into())) }],
        };
        save(&path, &payload).unwrap();
        let back = load(&path);
        let json = String::from_utf8(mp4::movie_box(&path, &BOX).unwrap().unwrap()).unwrap();
        mp4::set_movie_box(&path, &BOX, json.replace("\"version\":1", "\"version\":2").as_bytes()).unwrap();
        let refused = load(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(
            json,
            r#"{"version":1,"events":[{"t":10000000,"game":"deadlock","kind":"kill","killer":"hero_hornet","victim":"hero_atlas","assists":1,"bounty":416,"mine":true},{"t":20000000,"game":"deadlock","kind":{"phase":"PostGame"}}]}"#
        );
        assert_eq!(back, Ok(Some(payload)));
        assert_eq!(refused, Err("game events were saved by a newer glint (format 2)".into()));
    }
}
