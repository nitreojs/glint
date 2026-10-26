//! Deadlock's console log: with `-condebug` in its launch options the game writes everything its
//! console shows to `game\citadel\console.log`, made anew at each start, each line stamped with
//! the local time to the second (`10/26 18:55:31 Killer: hero_hornet, …`).
//!
//! What it gives depends on where the game runs. Match phases (`ChangeGameState: …`) are logged
//! everywhere. Kills, respawns and bounties come from the server's side, so they're only there
//! when the server is this PC (the hideout, bot matches); an online match logs none, and nothing
//! there tells which hero is the local player's.
//!
//! afterglow's addon (`mods/deadlock`) fills that in: a script in the HUD writes the local
//! player's kills, deaths and assists, the kill feed, kill streaks, objectives destroyed or under
//! attack, announcements, the match result and a summary as JSON lines
//! (`[PanoramaScript] [AFTERGLOW]{"v":2,"event":"kill",…}`, [`Addon`]). Once it's heard from, the
//! server's kill and respawn lines are left out: the addon reports them too, with whose they are.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex_lite::{Captures, Regex};
use serde::Deserialize;

use super::tail::Tail;
use super::{Event, Game, GameEvent, Kind, Now, Source, Stamp};

pub const NAME: &str = "deadlock";

pub const GAME: Game = Game { name: NAME, exe: "deadlock.exe", open };

/// What a line can be past slack in its time stamp: the run is what the game starts with.
const SLACK_MS: i64 = 2_000;

pub const ADDON_TAG: &str = "[AFTERGLOW]";
/// [`Addon::v`]: the newest this build reads (2 added `streak`, `result`, `summary`, `announce` and
/// objectives' `state`); every older one reads too.
pub const ADDON_VERSION: u32 = 2;
/// How far the addon's own clock may be from the line's stamp and still be believed (ms).
const CLIENT_SLACK_MS: i64 = 3_000;
/// A feed entry naming the player and the addon's own kill or death for it come this close (ms).
const SAME_MS: i64 = 3_000;

fn open(exe: &Path, started: i64, player: Option<&str>) -> Option<Box<dyn Source>> {
    let mut source = LogSource::new(log_path(exe)?, started);
    source.session.player = player.map(str::to_string);
    Some(Box::new(source))
}

/// The console log of the game started from `exe` (`…\Deadlock\game\bin\win64\deadlock.exe`).
pub fn log_path(exe: &Path) -> Option<PathBuf> {
    Some(exe.parent()?.parent()?.parent()?.join("citadel").join("console.log"))
}

/// What a line says, as far as events go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Kill { killer: String, victim: String, assists: u32 },
    /// The total bounty of the kill logged next.
    Bounty(u32),
    Respawn(String),
    /// The match's state (`GameInProgress`): from the server (`ChangeGameState`), or as the
    /// client heard it (`OnGameStateChanged`, the only one when the server is elsewhere).
    Phase { name: String, server: bool },
    /// A map is loading: a new session.
    Map(String),
    /// A bot joined as player `id` (on this PC's server).
    Bot(u32),
    /// Player `id` (on this PC's server) plays `hero`.
    Hero { id: u32, hero: String },
    Addon(Box<Addon>),
}

/// A line of afterglow's addon. `killer`/`victim`/`name` are the names the HUD shows.
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(default)]
pub struct Addon {
    pub v: u32,
    /// `hello`, `phase`, `kill`, `death`, `assist`, `respawn`, `feed`, `objective`, `error`; since
    /// 2 `streak`, `announce`, `result`, `summary`.
    pub event: String,
    /// When it happened, by the game's clock (unix ms).
    pub t: Option<i64>,
    pub killer: Option<String>,
    pub victim: Option<String>,
    pub assists: Option<u32>,
    /// A feed entry the local player is in: `killer` or `victim`.
    pub me: Option<String>,
    /// An objective's message (1), an announcement, the match result as the HUD words it.
    pub text: Option<String>,
    pub sub: Option<String>,
    /// The objective: `guardian`, `walker`, `patron`, `midboss`, `idol` … (1: a feed class).
    pub what: Option<String>,
    /// What befell the objective: `destroyed`, `attacked`, `returned` …
    pub state: Option<String>,
    /// `ours`, `theirs`, `neutral`.
    pub side: Option<String>,
    pub lane: Option<Lane>,
    pub mine: Option<bool>,
    /// A streak's kind (`first_blood`, `streak`, `ended`).
    pub kind: Option<String>,
    pub count: Option<u32>,
    /// Whose streak, or the local player's name in a summary.
    pub name: Option<String>,
    pub hero: Option<String>,
    /// `victory`, `defeat`, `abandoned`, `unknown`.
    pub result: Option<String>,
    /// The local player's totals so far, on `kill` and `death` from the top bar's counters.
    pub kills: Option<u32>,
    pub deaths: Option<u32>,
    /// What told a `kill` or `death`: `kda` the top bar's counters, which the game refreshes in
    /// bursts (several kills seconds late at once), `dead` the player's portrait going dark.
    pub src: Option<String>,
    pub level: Option<u32>,
    pub souls: Option<String>,
    pub best_streak: Option<u32>,
}

/// Where an objective stands: the top bar's map numbers lanes, the objective's own panel names
/// their colour.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum Lane {
    Number(u32),
    Colour(String),
}

/// What a line of the addon becomes, and whether the kill feed told it (see [`Session::feed`]).
type Told = fn(Addon, Option<&str>) -> Option<(Event, bool)>;

/// One entry per event of the addon's that's an event here; the rest (`hello`, `phase`, `error`)
/// aren't.
const ADDON_EVENTS: &[(&str, Told)] = &[
    ("kill", |a, _| Some((a.own(Kind::Kill), false))),
    ("death", |a, _| Some((a.own(Kind::Death), false))),
    ("assist", |a, _| Some((a.own(Kind::Assist), false))),
    ("respawn", |a, _| Some((a.own(Kind::Respawn), false))),
    ("feed", Addon::feed),
    ("objective", |a, player| a.objective(player).map(|e| (e, false))),
    ("streak", |a, player| Some((a.streak(player), false))),
    ("announce", |a, _| a.announce().map(|e| (e, false))),
    ("result", |a, _| a.result().map(|e| (e, false))),
    ("summary", |a, _| Some((a.summary(), false))),
];

/// Whether `who` is `player` as the HUD writes names (the kill feed in capitals).
fn named(who: Option<&str>, player: Option<&str>) -> bool {
    who.zip(player).is_some_and(|(w, p)| w.trim().eq_ignore_ascii_case(p.trim()))
}

impl Addon {
    /// The addon's line in `text` (after the stamp); `None` for other lines and for lines of a
    /// newer addon.
    pub fn parse(text: &str) -> Option<Addon> {
        let json = &text[text.find(ADDON_TAG)? + ADDON_TAG.len()..];
        serde_json::from_str::<Addon>(json).ok().filter(|a| (1..=ADDON_VERSION).contains(&a.v))
    }

    /// What it says, as an event; `None` for what isn't one (`hello`, the local player's own feed
    /// entries, which come as `kill` and `death` too). `player` is the local player's name: a
    /// feed entry or streak naming them is theirs even when the addon couldn't tell (the hideout
    /// has no top bar to find them on).
    fn event(self, player: Option<&str>) -> Option<(Event, bool)> {
        let (_, told) = ADDON_EVENTS.iter().find(|(name, _)| *name == self.event)?;
        told(self, player)
    }

    fn own(self, kind: Kind) -> Event {
        Event { killer: self.killer, victim: self.victim, assists: self.assists, mine: Some(true), ..Event::new(NAME, kind) }
    }

    fn feed(self, player: Option<&str>) -> Option<(Event, bool)> {
        if self.me.is_some() {
            return None;
        }
        let (kind, mine) = match (named(self.killer.as_deref(), player), named(self.victim.as_deref(), player)) {
            (_, true) => (Kind::Death, true),
            (true, _) => (Kind::Kill, true),
            _ => (Kind::Kill, false),
        };
        Some((Event { killer: self.killer, victim: self.victim, assists: self.assists, mine: Some(mine), ..Event::new(NAME, kind) }, mine))
    }

    fn objective(self, player: Option<&str>) -> Option<Event> {
        let mine = (self.mine == Some(true) || named(self.killer.as_deref(), player)).then_some(true).or(self.mine);
        let Some(state) = self.state else {
            let which = self.text.or_else(|| self.victim.clone()).or(self.what)?;
            return Some(Event { killer: self.killer, victim: self.victim, mine, ..Event::new(NAME, Kind::Objective(which)) });
        };
        let what = self.what?.replace('_', " ");
        let side = match self.side.as_deref() {
            Some("ours") => Some("our"),
            Some("theirs") => Some("enemy"),
            _ => None,
        };
        let colour = match &self.lane {
            Some(Lane::Colour(c)) => Some(c.as_str()),
            _ => None,
        };
        let mut which = [side, colour, Some(what.as_str())].into_iter().flatten().collect::<Vec<_>>().join(" ");
        if let Some(Lane::Number(n)) = self.lane {
            which += &format!(" (lane {n})");
        }
        Some(Event { killer: self.killer, victim: self.victim, mine, detail: Some(state), ..Event::new(NAME, Kind::Objective(which)) })
    }

    fn streak(self, player: Option<&str>) -> Event {
        let mine = self.mine == Some(true) || named(self.name.as_deref(), player);
        let kind = Kind::Streak(self.kind.unwrap_or_else(|| "streak".into()));
        Event { killer: self.name, hero: self.hero, count: self.count, mine: Some(mine), ..Event::new(NAME, kind) }
    }

    fn announce(self) -> Option<Event> {
        let words = [self.text, self.sub].into_iter().flatten().collect::<Vec<_>>().join(" · ");
        (!words.is_empty()).then(|| Event { detail: Some(words), ..Event::new(NAME, Kind::Custom("announce".into())) })
    }

    fn result(self) -> Option<Event> {
        Some(Event { detail: self.text, mine: Some(true), ..Event::new(NAME, Kind::Result(self.result?)) })
    }

    fn summary(self) -> Event {
        let n = |v: Option<u32>| v.map_or("?".to_string(), |v| v.to_string());
        let mut words = vec![format!("{}/{}/{}", n(self.kills), n(self.deaths), n(self.assists))];
        words.extend(self.level.map(|l| format!("level {l}")));
        words.extend(self.souls.map(|s| format!("{s} souls")));
        words.extend(self.best_streak.map(|s| format!("best streak {s}")));
        words.extend(self.hero.clone());
        let detail = Some(words.join(" · "));
        Event { hero: self.hero, assists: self.assists, mine: Some(true), detail, ..Event::new(NAME, Kind::Custom("summary".into())) }
    }
}

type Build = fn(&Captures) -> Option<Line>;

/// One entry per kind of line: the text it starts with (checked first, it's cheap), the pattern
/// for the text after the time stamp, and what it means.
const LINES: &[(&str, &str, Build)] = &[
    ("Killer: ", r"^Killer: (\S+), Dying: (\S+), Number of Assisters: (\d+)", |c| {
        Some(Line::Kill { killer: c[1].into(), victim: c[2].into(), assists: c[3].parse().ok()? })
    }),
    ("GameTime: ", r"^GameTime: [\d.]+, Base Bounty: \d+, Total Bounty: (\d+)", |c| Some(Line::Bounty(c[1].parse().ok()?))),
    ("hero_", r"^(hero_\w+) respawned$", |c| Some(Line::Respawn(c[1].into()))),
    ("ChangeGameState: ", r"^ChangeGameState: (\w+) \(\d+\)", |c| Some(Line::Phase { name: c[1].into(), server: true })),
    ("OnGameStateChanged: ", r"^OnGameStateChanged: (\w+) \(\d+\)", |c| Some(Line::Phase { name: c[1].into(), server: false })),
    ("Loading map ", r#"^Loading map "([^"]+)""#, |c| Some(Line::Map(c[1].into()))),
    ("Created bot ", r"^Created bot (\d+)/", |c| Some(Line::Bot(c[1].parse().ok()?))),
    ("[Server] Loaded hero ", r"^\[Server\] Loaded hero (\d+)/(hero_\w+)", |c| {
        Some(Line::Hero { id: c[1].parse().ok()?, hero: c[2].into() })
    }),
];

static TABLE: LazyLock<Vec<(&str, Regex, Build)>> =
    LazyLock::new(|| LINES.iter().map(|&(start, re, build)| (start, Regex::new(re).expect("valid pattern"), build)).collect());

/// What the text of a line (after its time stamp) says; `None` for the rest.
pub fn parse(text: &str) -> Option<Line> {
    match TABLE.iter().find(|(start, ..)| text.starts_with(start)) {
        Some((_, re, build)) => build(&re.captures(text)?),
        None => Addon::parse(text).map(|a| Line::Addon(Box::new(a))),
    }
}

/// The time stamp in front of a line (`10/26 18:55:31 `) and the text after it; `None` for a line
/// without one (the rest of a message that spans lines).
pub fn split(line: &str) -> Option<(Stamp, &str)> {
    let b = line.as_bytes();
    let shape = b.len() >= 14 && b[2] == b'/' && b[5] == b' ' && b[8] == b':' && b[11] == b':' && b.get(14).is_none_or(|&c| c == b' ');
    if !shape {
        return None;
    }
    let n = |at: usize| {
        let (hi, lo) = (b[at], b[at + 1]);
        (hi.is_ascii_digit() && lo.is_ascii_digit()).then(|| (hi - b'0') as u32 * 10 + (lo - b'0') as u32)
    };
    let stamp = Stamp { month: n(0)?, day: n(3)?, hour: n(6)?, minute: n(9)?, second: n(12)? };
    Some((stamp, line.get(15..).unwrap_or("")))
}

/// Turns lines into events, remembering what earlier lines told: the bots, the local player's
/// hero, the phase, the bounty of the coming kill, whether the addon runs.
#[derive(Debug, Default)]
pub struct Session {
    bots: Vec<u32>,
    me: Option<String>,
    phase: Option<String>,
    /// The server's phases are logged: the client's echoes of them come late and are skipped.
    server: bool,
    bounty: Option<(Stamp, u32)>,
    /// The addon was heard from in this run of the game.
    addon: bool,
    /// The name the local player plays under, from Steam.
    player: Option<String>,
    /// The local player's recent kills and deaths told as they happened (unix ms, and whether a
    /// feed entry told it), so one told both by the feed and by the portrait counts once.
    own: [Vec<(i64, bool)>; 2],
    /// The local player's kills and deaths told so far: a counter's total only adds what's past it.
    told: [u32; 2],
}

impl Session {
    /// The local player's hero, once a line told it.
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    pub fn addon(&self) -> bool {
        self.addon
    }

    /// The event in a line stamped `stamp` that arrived at `at` (unix ms, [`Now::place`]).
    pub fn feed(&mut self, stamp: Stamp, at: i64, text: &str) -> Option<GameEvent> {
        let event = match parse(text)? {
            Line::Addon(addon) => {
                self.addon = true;
                let at = addon.t.filter(|t| (t - at).abs() <= CLIENT_SLACK_MS).unwrap_or(at);
                let totals = [addon.kills, addon.deaths].map(|t| t.filter(|_| addon.src.as_deref() == Some("kda")));
                let (event, by_feed) = (*addon).event(self.player.as_deref())?;
                let slot = match (&event.kind, event.mine) {
                    (Kind::Kill, Some(true)) => Some(0),
                    (Kind::Death, Some(true)) => Some(1),
                    _ => None,
                };
                if let Some(i) = slot {
                    match totals[i] {
                        // the counters lag and come in bursts: they only fill in what the feed missed
                        Some(total) if total <= self.told[i] => return None,
                        Some(_) => {}
                        None => {
                            // one told the other way a moment ago is this one; two told the same way are two
                            let own = &mut self.own[i];
                            own.retain(|(t, _)| at - t <= SAME_MS);
                            if let Some(k) = own.iter().position(|&(_, feed)| feed != by_feed) {
                                own.remove(k);
                                return None;
                            }
                            own.push((at, by_feed));
                        }
                    }
                    self.told[i] += 1;
                }
                return Some(GameEvent { at, event });
            }
            Line::Map(_) => {
                // a new match: the counters start over
                *self = Session { addon: self.addon, player: self.player.take(), ..Session::default() };
                return None;
            }
            Line::Bot(id) => {
                self.bots.push(id);
                return None;
            }
            // on this PC's server every player who isn't a bot is the local one
            Line::Hero { id, hero } => {
                if !self.bots.contains(&id) {
                    self.me = Some(hero);
                }
                return None;
            }
            Line::Bounty(b) => {
                self.bounty = Some((stamp, b));
                return None;
            }
            Line::Phase { name, server } => {
                self.server |= server;
                if server != self.server || self.phase.as_ref() == Some(&name) {
                    return None;
                }
                self.phase = Some(name.clone());
                Event::new(NAME, Kind::Phase(name))
            }
            Line::Respawn(_) | Line::Kill { .. } if self.addon => return None,
            Line::Respawn(hero) => {
                let mine = self.me.as_ref().map(|m| *m == hero);
                Event { hero: Some(hero), mine, ..Event::new(NAME, Kind::Respawn) }
            }
            Line::Kill { killer, victim, assists } => {
                let bounty = self.bounty.take().filter(|(s, _)| *s == stamp).map(|(_, b)| b);
                let (kind, mine) = match self.me.as_deref() {
                    Some(me) if victim == me => (Kind::Death, Some(true)),
                    Some(me) => (Kind::Kill, Some(killer == me)),
                    None => (Kind::Kill, None),
                };
                Event { killer: Some(killer), victim: Some(victim), assists: Some(assists), bounty, mine, ..Event::new(NAME, kind) }
            }
        };
        Some(GameEvent { at, event })
    }
}

/// The game's console log as a [`Source`].
pub struct LogSource {
    tail: Tail,
    session: Session,
    lines: Vec<String>,
    /// Lines from before (unix ms) are an earlier run's, in a file this run hasn't replaced yet.
    since: i64,
}

impl LogSource {
    /// Reads `path` from its start (so the session is known even when the game was running
    /// first), for a game started at `started` (unix ms).
    pub fn new(path: PathBuf, started: i64) -> LogSource {
        LogSource { tail: Tail::new(path), session: Session::default(), lines: Vec::new(), since: started - SLACK_MS }
    }
}

impl Source for LogSource {
    fn poll(&mut self, now: Now, out: &mut Vec<GameEvent>) -> Result<(), String> {
        self.lines.clear();
        self.tail.poll(&mut self.lines)?;
        for line in &self.lines {
            let Some((stamp, text)) = split(line) else { continue };
            let at = now.place(stamp);
            if at < self.since {
                continue;
            }
            out.extend(self.session.feed(stamp, at, text));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::days;

    const FIXTURE: &str = include_str!("fixtures/deadlock-console.log");

    fn stamp(hour: u32, minute: u32, second: u32) -> Stamp {
        Stamp { month: 10, day: 26, hour, minute, second }
    }

    #[test]
    fn lines_split_off_their_stamp() {
        assert_eq!(split("10/26 18:55:31 Killer: a"), Some((stamp(18, 55, 31), "Killer: a")));
        assert_eq!(split("10/26 18:55:18 "), Some((stamp(18, 55, 18), "")));
        assert_eq!(split("10/26 18:55:18"), Some((stamp(18, 55, 18), "")));
        assert_eq!(split(" Camera Pose Node \"near_02\" has a noticeable height"), None);
        assert_eq!(split("10/2x 18:55:31 nope"), None);
        assert_eq!(split(""), None);
    }

    #[test]
    fn the_lines_seen_parse() {
        let kill = parse("Killer: hero_hornet, Dying: hero_atlas, Number of Assisters: 1");
        assert_eq!(kill, Some(Line::Kill { killer: "hero_hornet".into(), victim: "hero_atlas".into(), assists: 1 }));
        assert_eq!(parse("GameTime: 1.8, Base Bounty: 291, Total Bounty: 416, VictimValue: 600"), Some(Line::Bounty(416)));
        assert_eq!(parse("hero_atlas respawned"), Some(Line::Respawn("hero_atlas".into())));
        assert_eq!(parse("unknown respawned"), None);
        assert_eq!(parse("ChangeGameState: MatchIntro (4)"), Some(Line::Phase { name: "MatchIntro".into(), server: true }));
        assert_eq!(parse("OnGameStateChanged: GameInProgress (7)"), Some(Line::Phase { name: "GameInProgress".into(), server: false }));
        assert_eq!(parse("Loading map \"dl_hideout\""), Some(Line::Map("dl_hideout".into())));
        assert_eq!(parse("Created bot 495/hero_inferno/hero_inferno at location 1 2 3"), Some(Line::Bot(495)));
        assert_eq!(parse("[Server] Loaded hero 499/hero_hornet"), Some(Line::Hero { id: 499, hero: "hero_hornet".into() }));
        assert_eq!(parse("[SignonState] Client 0 'player' signon state SIGNONSTATE_NONE -> SIGNONSTATE_CONNECTED"), None);
        assert_eq!(parse("Cannot apply Ragdoll impulse, body part root_motion is not physicalized"), None);
    }

    #[test]
    fn the_session_knows_whose_kill_it_is() {
        let mut s = Session::default();
        let t = stamp(18, 55, 31);
        let feed = |s: &mut Session, text: &str| s.feed(t, 0, text).map(|e| e.event);
        let unknown = feed(&mut s, "Killer: hero_hornet, Dying: hero_atlas, Number of Assisters: 0").unwrap();
        assert_eq!((unknown.kind, unknown.mine), (Kind::Kill, None), "nobody known yet");
        assert_eq!(feed(&mut s, "Created bot 497/hero_gigawatt/hero_gigawatt at location 1 2 3"), None);
        assert_eq!(feed(&mut s, "[Server] Loaded hero 497/hero_atlas"), None);
        assert_eq!(s.me(), None, "a bot isn't the player");
        assert_eq!(feed(&mut s, "[Server] Loaded hero 499/hero_hornet"), None);
        assert_eq!(s.me(), Some("hero_hornet"));

        assert_eq!(feed(&mut s, "GameTime: 1.8, Base Bounty: 291, Total Bounty: 416, VictimValue: 600"), None);
        let kill = feed(&mut s, "Killer: hero_hornet, Dying: hero_atlas, Number of Assisters: 1").unwrap();
        assert_eq!((kill.kind, kill.mine, kill.bounty, kill.assists), (Kind::Kill, Some(true), Some(416), Some(1)));
        let death = feed(&mut s, "Killer: hero_atlas, Dying: hero_hornet, Number of Assisters: 0").unwrap();
        assert_eq!((death.kind, death.mine, death.bounty), (Kind::Death, Some(true), None), "the bounty went with the kill before");
        let theirs = feed(&mut s, "Killer: hero_atlas, Dying: hero_inferno, Number of Assisters: 0").unwrap();
        assert_eq!((theirs.kind, theirs.mine), (Kind::Kill, Some(false)));
        s.feed(stamp(18, 55, 20), 0, "GameTime: 1.6, Base Bounty: 1, Total Bounty: 2, VictimValue: 3");
        assert_eq!(feed(&mut s, "Killer: hero_atlas, Dying: hero_inferno, Number of Assisters: 0").unwrap().bounty, None, "a stale bounty");

        assert_eq!(feed(&mut s, "ChangeGameState: GameInProgress (7)").map(|e| e.kind), Some(Kind::Phase("GameInProgress".into())));
        assert_eq!(feed(&mut s, "ChangeGameState: GameInProgress (7)"), None, "no change");
        assert_eq!(feed(&mut s, "OnGameStateChanged: HeroSelection (3)"), None, "the client's late echo of the server");
        assert_eq!(feed(&mut s, "Loading map \"street_test\""), None);
        assert_eq!(s.me(), None, "a new map, a new session");
        let client = ["OnGameStateChanged: HeroSelection (3)", "OnGameStateChanged: GameInProgress (7)", "OnGameStateChanged: HeroSelection (3)"];
        let phases: Vec<_> = client.iter().filter_map(|l| feed(&mut s, l)).map(|e| e.kind).collect();
        let names = ["HeroSelection", "GameInProgress", "HeroSelection"].map(|n| Kind::Phase(n.into()));
        assert_eq!(phases, names, "with the server elsewhere, the client's phases are all there is");
    }

    #[test]
    fn the_addons_lines_say_whose_events_they_are() {
        let mut s = Session::default();
        let at = 1_761_494_405_250;
        let feed = |s: &mut Session, json: &str| s.feed(stamp(19, 0, 5), at, &format!("[PanoramaScript] [AFTERGLOW]{json}"));
        let event = |s: &mut Session, json: &str| feed(s, json).map(|e| e.event);

        assert_eq!(feed(&mut s, r#"{"v":3,"event":"hello","t":1761494405000}"#), None, "a newer addon's lines are left alone");
        assert!(!s.addon());
        assert_eq!(feed(&mut s, r#"{"v":1,"event":"hello","version":"1.0.0","t":1761494405000}"#), None);
        assert!(s.addon());

        let kill = feed(&mut s, r#"{"v":1,"event":"kill","kills":1,"src":"kda","t":1761494404100,"clock":"3:07","victim":"Bob"}"#).unwrap();
        assert_eq!(kill.at, 1_761_494_404_100, "the game's own clock is finer than the stamp");
        assert_eq!((kill.event.kind, kill.event.mine, kill.event.victim.as_deref()), (Kind::Kill, Some(true), Some("Bob")));
        let skewed = feed(&mut s, r#"{"v":1,"event":"kill","kills":2,"t":1761490000000}"#).unwrap();
        assert_eq!((skewed.at, skewed.event.victim), (at, None), "a clock far off the stamp isn't believed");

        let death = event(&mut s, r#"{"v":1,"event":"death","deaths":1,"src":"dead","killer":"Bob","t":1761494405000}"#).unwrap();
        assert_eq!((death.kind, death.mine, death.killer.as_deref()), (Kind::Death, Some(true), Some("Bob")));
        let assist = event(&mut s, r#"{"v":1,"event":"assist","assists":1,"victim":"Dan","t":1761494405000}"#).unwrap();
        assert_eq!((assist.kind, assist.mine, assist.victim.as_deref()), (Kind::Assist, Some(true), Some("Dan")));
        assert_eq!(event(&mut s, r#"{"v":1,"event":"respawn","t":1761494405000}"#).map(|e| (e.kind, e.mine)), Some((Kind::Respawn, Some(true))));

        let theirs = event(&mut s, r#"{"v":1,"event":"feed","killer":"Carl","victim":"Dan","assists":1,"tags":["enemyDied","hasAssists"],"t":1761494405000}"#).unwrap();
        assert_eq!((theirs.kind, theirs.mine, theirs.killer.as_deref(), theirs.assists), (Kind::Kill, Some(false), Some("Carl"), Some(1)));
        let own = r#"{"v":1,"event":"feed","killer":"alexx","victim":"Bob","assists":0,"me":"killer","tags":["enemyDied"],"t":1761494405000}"#;
        assert_eq!(event(&mut s, own), None, "the local player's own entries come as kill and death");

        // a session where Steam told the player's name; each line arrives when it says it happened
        let mut p = Session { player: Some("memories".into()), ..Session::default() };
        let mut told = |t: i64, json: String| p.feed(stamp(19, 0, 5), t, &format!("[PanoramaScript] [AFTERGLOW]{json}")).map(|e| e.event);
        let fed = |killer: &str, victim: &str, t: i64| format!(r#"{{"v":1,"event":"feed","killer":"{killer}","victim":"{victim}","assists":1,"t":{t}}}"#);
        let counted = |t: i64| format!(r#"{{"v":1,"event":"kill","src":"kda","t":{t}}}"#);
        let base = 1_761_494_400_000;

        let hideout = told(base, fed("MEMORIES", "ABRAMS", base)).unwrap();
        assert_eq!((hideout.kind, hideout.mine, hideout.victim.as_deref()), (Kind::Kill, Some(true), Some("ABRAMS")), "found by name, any case");
        assert_eq!(told(base + 900, counted(base + 900)), None, "the counter telling the same kill a moment later");
        let died = told(base + 10_000, fed("Bob", " memories ", base + 10_000)).unwrap();
        assert_eq!((died.kind, died.mine, died.killer.as_deref()), (Kind::Death, Some(true), Some("Bob")));

        let double = base + 20_000;
        let both = [
            told(double, counted(double)).is_some(),
            told(double + 400, counted(double + 400)).is_some(),
            told(double + 600, fed("memories", "X", double + 600)).is_some(),
            told(double + 900, fed("memories", "Y", double + 900)).is_some(),
        ];
        assert_eq!(both, [true, true, false, false], "a double kill told both ways is two kills");
        let later = told(base + 40_000, fed("MEMORIES", "Dan", base + 40_000)).unwrap();
        assert_eq!((later.kind, later.mine), (Kind::Kill, Some(true)), "the next kill is a new one");

        let message = event(&mut s, r#"{"v":1,"event":"objective","text":"Your team destroyed the Guardian","good":true,"t":1761494405000}"#).unwrap();
        assert_eq!(message.kind, Kind::Objective("Your team destroyed the Guardian".into()));
        let boss = event(&mut s, r#"{"v":1,"event":"objective","what":"bossDied","killer":"alexx","victim":"Guardian","mine":true,"t":1761494405000}"#).unwrap();
        assert_eq!((boss.kind, boss.mine), (Kind::Objective("Guardian".into()), Some(true)));
        assert_eq!(event(&mut s, r#"{"v":1,"event":"objective","t":1761494405000}"#), None, "an objective without words");

        for quiet in [r#"{"v":1,"event":"phase","phase":"game","hero":"Infernus"}"#, r#"{"v":1,"event":"error","msg":"x"}"#, r#"{"v":1,"event":"kill""#] {
            assert_eq!(feed(&mut s, quiet), None, "{quiet}");
        }

        let log = |s: &mut Session, text: &str| s.feed(stamp(19, 0, 5), at, text).map(|e| e.event.kind);
        assert_eq!(log(&mut s, "Killer: hero_hornet, Dying: hero_atlas, Number of Assisters: 0"), None, "the addon tells the kills");
        assert_eq!(log(&mut s, "hero_atlas respawned"), None);
        assert_eq!(log(&mut s, "Loading map \"street_test\""), None);
        assert!(s.addon(), "the addon is there for the whole run");
        assert_eq!(log(&mut s, "ChangeGameState: GameInProgress (7)"), Some(Kind::Phase("GameInProgress".into())), "the phases still come from the log");
    }

    #[test]
    fn the_second_addon_tells_streaks_objectives_and_the_end() {
        let mut s = Session { player: Some("memories".into()), ..Session::default() };
        let at = 1_761_494_405_250;
        let mut event = |json: &str| s.feed(stamp(19, 0, 5), at, &format!("[PanoramaScript] [AFTERGLOW]{json}")).map(|e| e.event);
        assert_eq!(event(r#"{"v":2,"event":"hello","version":"2.0.0","t":1761494405000}"#), None);
        assert_eq!(event(r#"{"v":2,"event":"phase","phase":"game","hero_id":12,"t":1761494405000}"#), None);

        let first = event(r#"{"v":2,"event":"streak","kind":"first_blood","name":"MEMORIES","hero":"Infernus","mine":false,"t":1761494405000}"#).unwrap();
        assert_eq!((first.kind, first.mine, first.killer.as_deref()), (Kind::Streak("first_blood".into()), Some(true), Some("MEMORIES")), "the steam name tells it's theirs");
        let streak = event(r#"{"v":2,"event":"streak","kind":"streak","count":4,"name":"Bob","hero":"Haze","mine":false,"t":1761494405000}"#).unwrap();
        assert_eq!((streak.kind, streak.count, streak.mine, streak.hero.as_deref()), (Kind::Streak("streak".into()), Some(4), Some(false), Some("Haze")));

        let mut objective = |json: &str| {
            let e = event(json).unwrap();
            let Kind::Objective(which) = e.kind else { panic!("{json}") };
            (which, e.detail.unwrap_or_default(), e.mine)
        };
        let lines = [
            (r#"{"v":2,"event":"objective","state":"destroyed","what":"guardian","side":"theirs","team":2,"lane":2,"id":"Team2Tier1_2"}"#, ("enemy guardian (lane 2)", "destroyed", None)),
            (r#"{"v":2,"event":"objective","state":"attacked","what":"guardian","side":"ours","lane":"yellow","name":"Guardian"}"#, ("our yellow guardian", "attacked", None)),
            (r#"{"v":2,"event":"objective","state":"destroyed","what":"midboss","side":"neutral","name":"Mid-Boss"}"#, ("midboss", "destroyed", None)),
            (r#"{"v":2,"event":"objective","state":"returned","what":"idol","side":"ours","killer":"alexx","mine":true}"#, ("our idol", "returned", Some(true))),
            (r#"{"v":2,"event":"objective","state":"destroyed","what":"base_guardian","side":"theirs"}"#, ("enemy base guardian", "destroyed", None)),
        ];
        for (json, (which, detail, mine)) in lines {
            assert_eq!(objective(json), (which.to_string(), detail.to_string(), mine), "{json}");
        }

        let announce = event(r#"{"v":2,"event":"announce","text":"Enemy Yellow Tier 1","sub":"DESTROYED","good":true,"src":"team"}"#).unwrap();
        assert_eq!((announce.kind, announce.detail.as_deref()), (Kind::Custom("announce".into()), Some("Enemy Yellow Tier 1 · DESTROYED")));
        let result = event(r#"{"v":2,"event":"result","result":"victory","team":1,"winner":1,"text":"The Hidden King Victory"}"#).unwrap();
        assert_eq!((result.kind, result.detail.as_deref()), (Kind::Result("victory".into()), Some("The Hidden King Victory")));
        assert_eq!(event(r#"{"v":2,"event":"result","team":1}"#), None, "a result without one");
        let summary = event(r#"{"v":2,"event":"summary","name":"alexx","hero":"Infernus","hero_id":7,"level":17,"souls":"21.4k","objective_damage":5321,"kills":2,"deaths":1,"assists":1,"best_streak":3,"src":"kda"}"#).unwrap();
        assert_eq!((summary.kind, summary.detail.as_deref()), (Kind::Custom("summary".into()), Some("2/1/1 · level 17 · 21.4k souls · best streak 3 · Infernus")));
        let bare = event(r#"{"v":2,"event":"summary","kills":0,"deaths":0,"assists":0,"src":"tally"}"#).unwrap();
        assert_eq!(bare.detail.as_deref(), Some("0/0/0"));
    }

    #[test]
    fn the_real_log_gives_its_events() {
        // the fixture is a trimmed hideout session; it arrives in one read 20 s after its last line
        let offset = 3 * 3600;
        let unix = |h: i64, m: i64, s: i64| (days(2026, 10, 26) * 86_400 + h * 3600 + m * 60 + s - offset) * 1000;
        let path = std::env::temp_dir().join(format!("glint_deadlock_{}.log", std::process::id()));
        std::fs::write(&path, FIXTURE).unwrap();
        let mut source = LogSource::new(path.clone(), unix(18, 53, 30) + 300);
        let mut out = Vec::new();
        let now = Now { unix_ms: unix(18, 57, 1) + 250, offset };
        source.poll(now, &mut out).unwrap();
        std::fs::remove_file(&path).unwrap();

        let short = |e: &GameEvent| {
            let what = match &e.event.kind {
                Kind::Phase(p) => p.clone(),
                Kind::Respawn => format!("respawn {}", e.event.hero.as_deref().unwrap_or("")),
                k => format!("{k:?} {} > {}", e.event.killer.as_deref().unwrap_or(""), e.event.victim.as_deref().unwrap_or("")),
            };
            (e.at, what, e.event.mine)
        };
        let got: Vec<_> = out.iter().map(short).collect();
        let at = |h, m, s| unix(h, m, s) + 250;
        let phase = |p: &str| (at(18, 53, 41), p.to_string(), None);
        let respawn = |at: i64, hero: &str, mine| (at, format!("respawn {hero}"), mine);
        assert_eq!(
            got,
            [
                phase("WaitingForPlayersToJoin"),
                phase("HeroSelection"),
                phase("MatchIntro"),
                phase("WaitForMapToLoad"),
                phase("PreGameWait"),
                phase("GameInProgress"),
                respawn(at(18, 53, 41), "hero_hornet", None),
                respawn(at(18, 53, 41), "hero_inferno", None),
                respawn(at(18, 53, 42), "hero_hornet", Some(true)),
                respawn(at(18, 53, 42), "hero_hornet", Some(true)),
                respawn(at(18, 53, 42), "hero_inferno", Some(false)),
                (at(18, 55, 31), "Kill hero_hornet > hero_atlas".to_string(), Some(true)),
                respawn(at(18, 55, 35), "hero_atlas", Some(false)),
            ]
        );
        assert_eq!(out[11].event.bounty, Some(416));
        assert!(!FIXTURE.contains("memories"), "the player's name is kept out of the fixture");
    }

    #[test]
    fn an_earlier_runs_lines_are_skipped() {
        let offset = 0;
        let unix = |h: i64, m: i64, s: i64| (days(2026, 10, 26) * 86_400 + h * 3600 + m * 60 + s - offset) * 1000;
        let path = std::env::temp_dir().join(format!("glint_deadlock_old_{}.log", std::process::id()));
        std::fs::write(&path, "10/26 17:00:00 ChangeGameState: PostGame (8)\n10/26 18:00:00 ChangeGameState: HeroSelection (3)\n").unwrap();
        let mut source = LogSource::new(path.clone(), unix(18, 0, 0) + 900);
        let mut out = Vec::new();
        source.poll(Now { unix_ms: unix(18, 0, 1), offset }, &mut out).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(out.iter().map(|e| e.event.kind.clone()).collect::<Vec<_>>(), [Kind::Phase("HeroSelection".into())]);
    }

    #[test]
    fn the_log_sits_next_to_the_game() {
        let exe = Path::new(r"F:\SteamLibrary\steamapps\common\Deadlock\game\bin\win64\deadlock.exe");
        let want = Path::new(r"F:\SteamLibrary\steamapps\common\Deadlock\game").join("citadel").join("console.log");
        if cfg!(windows) {
            assert_eq!(log_path(exe), Some(want));
        }
        assert_eq!(log_path(Path::new("deadlock.exe")), None);
    }

    #[test]
    fn the_counters_fill_in_only_what_the_feed_missed() {
        // from a real bot match: the feed tells kills as they happen, the top bar's counters
        // catch up seconds later in bursts (kills 6 and 7 together, 10 to 13 and a death at once)
        let mut s = Session { player: Some("memories".into()), ..Session::default() };
        let line = |s: &mut Session, json: &str| {
            let t: i64 = json.split("\"t\":").nth(1).and_then(|r| r.split([',', '}']).next()).and_then(|t| t.parse().ok()).unwrap();
            s.feed(stamp(22, 0, 0), t, &format!("[PanoramaScript] [AFTERGLOW]{json}")).map(|e| (e.event.kind, e.event.mine))
        };
        let mut told = Vec::new();
        for json in [
            r#"{"v":1,"event":"feed","killer":"memories","victim":"Rem","me":"killer","t":1793041490919}"#,
            r#"{"v":1,"event":"feed","killer":"MEMORIES","victim":"GREY TALON","assists":1,"t":1793041502219}"#,
            r#"{"v":1,"event":"kill","kills":1,"src":"kda","t":1793041509195}"#,
            r#"{"v":1,"event":"kill","kills":2,"src":"kda","t":1793041509195}"#,
            r#"{"v":1,"event":"objective","what":"bossDied","killer":"MEMORIES","victim":"GUARDIAN","mine":false,"t":1793041570189}"#,
            r#"{"v":1,"event":"feed","killer":"WALKER","victim":"MEMORIES","assists":6,"t":1793041672287}"#,
            r#"{"v":1,"event":"death","deaths":0,"src":"dead","t":1793041672391}"#,
            r#"{"v":1,"event":"feed","killer":"MEMORIES","victim":"DEADMAN DANNY","assists":2,"t":1793041731774}"#,
            r#"{"v":1,"event":"kill","kills":3,"src":"kda","t":1793041748804}"#,
            r#"{"v":1,"event":"kill","kills":4,"src":"kda","t":1793041748804}"#,
            r#"{"v":1,"event":"death","deaths":1,"src":"kda","t":1793041748804}"#,
        ] {
            told.extend(line(&mut s, json));
        }
        let kills = told.iter().filter(|(k, m)| *k == Kind::Kill && *m == Some(true)).count();
        let deaths = told.iter().filter(|(k, m)| *k == Kind::Death && *m == Some(true)).count();
        assert_eq!((kills, deaths), (4, 1), "3 from the feed, 1 only the counters saw; one death told three ways: {told:?}");
        assert!(told.contains(&(Kind::Objective("GUARDIAN".into()), Some(true))), "a guardian the feed credits to the player is theirs");
    }

}
