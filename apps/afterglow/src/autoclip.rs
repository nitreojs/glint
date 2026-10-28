//! Auto-clip (config `auto_clip`): a clip saves itself a few seconds after a chosen game event
//! (your kill, a streak, an objective you took …), so it holds the aftermath too. Events close
//! together make one clip (a team fight is one), and auto saves keep a cooldown between them.
//! [`AutoClip`] only decides when, on the wall clock events are stamped with (unix ms); the tray
//! app cuts and writes the clip.

use glint_core::events::{Event, GameEvent, Kind};
use serde::Deserialize;

/// How long after the last event of a burst its clip is saved (ms): the aftermath.
pub const AFTER_MS: i64 = 5_000;
/// The least time from one save to the next auto save (ms), in a buffer long enough: the
/// [`cooldown`].
const COOLDOWN_MS: i64 = 30_000;
/// What a clip keeps at least before the first event of its burst (ms): a burst that goes on
/// and on is saved before its start slides out of the buffer.
const LEAD_MS: i64 = 10_000;

/// What can save a clip.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// The local player's kills.
    Kills,
    /// The local player's first blood and kill streaks.
    Streaks,
    /// The local player's deaths.
    Deaths,
    /// Objectives the local player took.
    Objectives,
    /// The match's end.
    Results,
}

impl Trigger {
    pub const ALL: [Trigger; 5] = [Trigger::Kills, Trigger::Streaks, Trigger::Deaths, Trigger::Objectives, Trigger::Results];

    /// What config.toml calls it.
    pub fn name(self) -> &'static str {
        match self {
            Trigger::Kills => "kills",
            Trigger::Streaks => "streaks",
            Trigger::Deaths => "deaths",
            Trigger::Objectives => "objectives",
            Trigger::Results => "results",
        }
    }

    /// The trigger `e` is, if any: only the local player's own (a streak that ends is a death,
    /// an objective only attacked isn't taken).
    pub fn of(e: &Event) -> Option<Trigger> {
        let mine = e.mine == Some(true);
        match &e.kind {
            Kind::Kill if mine => Some(Trigger::Kills),
            Kind::Death if mine => Some(Trigger::Deaths),
            Kind::Streak(s) if mine && s != "ended" => Some(Trigger::Streaks),
            Kind::Objective(_) if mine && e.detail.as_deref() != Some("attacked") => Some(Trigger::Objectives),
            Kind::Result(_) => Some(Trigger::Results),
            _ => None,
        }
    }
}

/// Events waiting for their save.
#[derive(Debug, Default)]
struct Burst {
    /// Unix ms.
    first: i64,
    last: i64,
    /// How many of each [`Trigger::ALL`].
    counts: [u32; 5],
    /// The longest streak told, 0 for first blood alone.
    streak: u32,
    result: Option<String>,
}

impl Burst {
    fn add(&mut self, t: Trigger, e: &GameEvent) {
        if self.counts.iter().all(|&n| n == 0) {
            (self.first, self.last) = (e.at, e.at);
        }
        self.first = self.first.min(e.at);
        self.last = self.last.max(e.at);
        self.counts[t as usize] += 1;
        match &e.event.kind {
            Kind::Streak(_) => self.streak = self.streak.max(e.event.count.unwrap_or(0)),
            Kind::Result(r) => self.result = Some(r.clone()),
            _ => {}
        }
    }

    fn count(&self, t: Trigger) -> u32 {
        self.counts[t as usize]
    }

    /// What the clip's name ends with: `auto-3-kills`, `auto-victory` …, after the best of it.
    fn label(&self) -> String {
        let kills = self.count(Trigger::Kills);
        let word = match &self.result {
            Some(r) => r.chars().filter(|c| c.is_ascii_alphanumeric()).collect(),
            None if kills >= 2 => format!("{kills}-kills"),
            None if self.streak >= 2 => format!("streak-{}", self.streak),
            None if self.count(Trigger::Streaks) > 0 => "first-blood".into(),
            None if self.count(Trigger::Objectives) > 0 => "objective".into(),
            None if kills > 0 => "kill".into(),
            None => "death".into(),
        };
        format!("auto-{word}")
    }
}

/// When the next auto save is due.
#[derive(Debug)]
pub struct AutoClip {
    triggers: Vec<Trigger>,
    /// What a save covers (ms).
    length: i64,
    /// No buffer to save from: nothing is heard.
    paused: bool,
    /// Events before this (unix ms) aren't in the buffer.
    since: i64,
    /// The last save of any kind: what it covered isn't saved again.
    saved: Option<i64>,
    burst: Option<Burst>,
}

impl AutoClip {
    /// Off and paused.
    pub fn new() -> AutoClip {
        AutoClip { triggers: Vec::new(), length: 60_000, paused: true, since: 0, saved: None, burst: None }
    }

    /// Saves after `triggers` while `on`, for a buffer of `length` seconds; turned off, what was
    /// waiting is dropped.
    pub fn configure(&mut self, on: bool, triggers: &[Trigger], length: u32) {
        self.triggers = if on { triggers.to_vec() } else { Vec::new() };
        self.length = length as i64 * 1000;
        if self.triggers.is_empty() {
            self.burst = None;
        }
    }

    /// The buffer stopped (paused, away) or started again at `now`: a stopped one drops what
    /// was waiting, and a new one holds nothing from before.
    pub fn pause(&mut self, paused: bool, now: i64) {
        if paused {
            self.burst = None;
        } else if self.paused {
            self.since = now;
        }
        self.paused = paused;
    }

    /// Takes `e`, heard at `now`, when it's one of the triggers chosen and not already saved or
    /// out of the buffer's reach; the trigger it is.
    pub fn hear(&mut self, e: &GameEvent, now: i64) -> Option<Trigger> {
        if self.paused {
            return None;
        }
        let t = Trigger::of(&e.event).filter(|t| self.triggers.contains(t))?;
        let from = self.since.max(now - self.length);
        if e.at < from || self.saved.is_some_and(|s| e.at <= s) {
            return None;
        }
        self.burst.get_or_insert_with(Burst::default).add(t, e);
        Some(t)
    }

    /// When the waiting events' clip is due (unix ms): [`AFTER_MS`] past the last of them, but
    /// soon enough to keep [`LEAD_MS`] before the first, and not within the cooldown.
    pub fn due(&self) -> Option<i64> {
        let b = self.burst.as_ref()?;
        let due = (b.last + AFTER_MS).min(b.first + hold(self.length));
        Some(self.saved.map_or(due, |s| due.max(s + cooldown(self.length))))
    }

    /// How many events wait.
    pub fn waiting(&self) -> u32 {
        self.burst.as_ref().map_or(0, |b| b.counts.iter().sum())
    }

    /// The label of the clip to save at `now`, once it's due; it counts as saved.
    pub fn take(&mut self, now: i64) -> Option<String> {
        if self.due()? > now {
            return None;
        }
        let label = self.burst.take()?.label();
        self.saved(now);
        Some(label)
    }

    /// A clip was saved at `now` (by hand too): what it covers is in it.
    pub fn saved(&mut self, now: i64) {
        self.saved = Some(now);
        self.burst = None;
    }
}

/// How long after a burst's first event its clip may wait, for a buffer of `length` ms: long
/// enough for the aftermath, short enough to keep [`LEAD_MS`] before it where the buffer allows.
fn hold(length: i64) -> i64 {
    (length - LEAD_MS).max(AFTER_MS)
}

/// The least time from one save to the next auto save for a buffer of `length` ms:
/// [`COOLDOWN_MS`], less in a short buffer, so a burst right after a save still keeps what's
/// before it.
pub fn cooldown(length: i64) -> i64 {
    COOLDOWN_MS.min(hold(length))
}

/// The wall clock events are stamped with (unix ms).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_793_000_000_000;
    const DEFAULT: [Trigger; 3] = [Trigger::Kills, Trigger::Streaks, Trigger::Objectives];

    fn on(triggers: &[Trigger], length: u32) -> AutoClip {
        let mut a = AutoClip::new();
        a.configure(true, triggers, length);
        a.pause(false, T0 - 60_000);
        a
    }

    fn at(s: f64, kind: Kind, mine: Option<bool>) -> GameEvent {
        GameEvent { at: T0 + (s * 1000.0) as i64, event: Event { mine, ..Event::new("deadlock", kind) } }
    }

    fn kill(s: f64) -> GameEvent {
        at(s, Kind::Kill, Some(true))
    }

    /// Feeds `events` (heard as they happen) to `a`, stepping a fake clock by 250 ms from `T0`
    /// to `T0 + until` s; when each clip was taken (s past `T0`) and its label.
    fn run(a: &mut AutoClip, events: &[GameEvent], until: f64) -> Vec<(f64, String)> {
        let mut saves = Vec::new();
        let mut next = 0;
        let mut now = T0;
        while now <= T0 + (until * 1000.0) as i64 {
            while next < events.len() && events[next].at <= now {
                a.hear(&events[next], now);
                next += 1;
            }
            if let Some(label) = a.take(now) {
                saves.push(((now - T0) as f64 / 1000.0, label));
            }
            now += 250;
        }
        saves
    }

    #[test]
    fn a_kill_saves_a_few_seconds_later() {
        let mut a = on(&DEFAULT, 60);
        assert_eq!(a.hear(&kill(0.0), T0), Some(Trigger::Kills));
        assert_eq!(a.due(), Some(T0 + AFTER_MS));
        assert_eq!(a.take(T0 + AFTER_MS - 1), None, "not before the aftermath");
        assert_eq!(a.take(T0 + AFTER_MS).as_deref(), Some("auto-kill"));
        assert_eq!((a.due(), a.take(T0 + 60_000)), (None, None), "once");
    }

    #[test]
    fn a_fight_is_one_clip() {
        let mut a = on(&DEFAULT, 60);
        let fight = [kill(0.0), kill(2.0), at(4.0, Kind::Streak("streak".into()), Some(true)), kill(7.5)];
        assert_eq!(run(&mut a, &fight, 60.0), [(12.5, "auto-3-kills".to_string())], "5 s after the last of them");
    }

    #[test]
    fn a_fight_that_goes_on_is_saved_before_its_start_leaves_the_buffer() {
        let mut a = on(&DEFAULT, 60);
        let fight: Vec<GameEvent> = (0..40).map(|i| kill(i as f64 * 3.0)).collect();
        let saves = run(&mut a, &fight, 200.0);
        let times: Vec<f64> = saves.iter().map(|(t, _)| *t).collect();
        // each holds 10 s before its first kill; the one after the fight waits out the cooldown
        assert_eq!(times, [50.0, 101.0, 131.0]);
        for (i, w) in times.windows(2).enumerate() {
            assert!(w[1] - w[0] >= 30.0, "save {i}: {times:?}");
        }
    }

    #[test]
    fn saves_keep_a_cooldown_and_what_waits_still_gets_one() {
        let mut a = on(&DEFAULT, 60);
        let kills = [kill(0.0), kill(10.0), kill(12.0)];
        assert_eq!(run(&mut a, &kills, 90.0), [(5.0, "auto-kill".to_string()), (35.0, "auto-2-kills".to_string())]);
    }

    #[test]
    fn a_short_buffer_shortens_the_cooldown_to_keep_whats_before() {
        let kills = [kill(0.0), kill(6.0)];
        let saved_at = |length| run(&mut on(&DEFAULT, length), &kills, 60.0).into_iter().map(|(t, _)| t).collect::<Vec<_>>();
        assert_eq!(saved_at(60), [5.0, 35.0], "the full cooldown");
        assert_eq!(saved_at(30), [5.0, 25.0], "20 s: the second kill keeps 11 s of the 30 before it");
        assert_eq!(saved_at(10), [5.0, 11.0]);
        assert_eq!(cooldown(5_000), AFTER_MS);
        let mut short = on(&DEFAULT, 5);
        short.hear(&kill(0.0), T0);
        short.hear(&kill(4.0), T0 + 4_000);
        assert_eq!(short.due(), Some(T0 + 5_000), "a 5 s buffer still holds the first kill");
    }

    #[test]
    fn only_the_chosen_and_your_own_count() {
        let mut a = on(&DEFAULT, 60);
        let streak = |kind: &str, mine| at(0.0, Kind::Streak(kind.into()), mine);
        let objective = |detail: Option<&str>, mine| GameEvent { event: Event { detail: detail.map(Into::into), ..at(0.0, Kind::Objective("enemy guardian".into()), mine).event }, ..kill(0.0) };
        let ignored = [
            at(0.0, Kind::Kill, Some(false)),
            at(0.0, Kind::Kill, None),
            at(0.0, Kind::Death, Some(true)),
            at(0.0, Kind::Assist, Some(true)),
            at(0.0, Kind::Phase("PostGame".into()), None),
            streak("streak", Some(false)),
            streak("ended", Some(true)),
            objective(Some("destroyed"), None),
            objective(Some("attacked"), Some(true)),
            at(0.0, Kind::Result("victory".into()), Some(true)),
        ];
        for e in &ignored {
            assert_eq!(a.hear(e, T0), None, "{e:?}");
        }
        assert_eq!(a.due(), None);
        assert_eq!(a.hear(&streak("first_blood", Some(true)), T0), Some(Trigger::Streaks));
        assert_eq!(a.hear(&objective(Some("destroyed"), Some(true)), T0), Some(Trigger::Objectives));

        let mut picky = on(&[Trigger::Deaths, Trigger::Results], 60);
        assert_eq!(picky.hear(&kill(0.0), T0), None);
        assert_eq!(picky.hear(&at(0.0, Kind::Death, Some(true)), T0), Some(Trigger::Deaths));
        assert_eq!(picky.hear(&at(1.0, Kind::Result("victory".into()), None), T0), Some(Trigger::Results));
        assert_eq!(picky.take(T0 + 6_000).as_deref(), Some("auto-victory"));
    }

    #[test]
    fn paused_hears_nothing_and_drops_what_waited() {
        let mut a = on(&DEFAULT, 60);
        a.hear(&kill(0.0), T0);
        a.pause(true, T0 + 1_000);
        assert_eq!(a.due(), None, "pausing drops the waiting clip");
        assert_eq!(a.hear(&kill(2.0), T0 + 2_000), None, "nothing is heard while paused");
        a.pause(false, T0 + 3_000);
        assert_eq!(a.hear(&kill(2.5), T0 + 3_200), None, "from before the buffer started again");
        assert_eq!(a.hear(&kill(3.5), T0 + 3_600), Some(Trigger::Kills));
        assert_eq!(a.take(T0 + 8_500).as_deref(), Some("auto-kill"));
    }

    #[test]
    fn what_a_save_covered_or_the_buffer_lost_isnt_saved() {
        let mut a = on(&DEFAULT, 60);
        a.hear(&kill(0.0), T0);
        a.saved(T0 + 1_000);
        assert_eq!(a.due(), None, "saved by hand: the kill is in that clip");
        assert_eq!(a.hear(&kill(0.5), T0 + 1_500), None, "told late, but covered");
        assert_eq!(a.hear(&kill(-70.0), T0 + 1_500), None, "older than the buffer reaches");
    }

    #[test]
    fn turned_off_nothing_saves() {
        let mut a = on(&DEFAULT, 60);
        a.hear(&kill(0.0), T0);
        a.configure(false, &DEFAULT, 60);
        assert_eq!((a.due(), a.hear(&kill(1.0), T0 + 1_000)), (None, None));
        a.configure(true, &[], 60);
        assert_eq!(a.hear(&kill(2.0), T0 + 2_000), None, "with no event picked");
    }

    #[test]
    fn labels_name_the_best_of_the_clip() {
        let label = |events: &[GameEvent]| {
            let mut a = on(&Trigger::ALL, 60);
            events.iter().for_each(|e| assert!(a.hear(e, T0).is_some(), "{e:?}"));
            a.take(T0 + 60_000).unwrap()
        };
        let streak = |n| GameEvent { event: Event { count: Some(n), ..at(0.0, Kind::Streak("streak".into()), Some(true)).event }, ..kill(0.0) };
        assert_eq!(label(&[kill(0.0), streak(4)]), "auto-streak-4");
        assert_eq!(label(&[kill(0.0), kill(1.0), streak(4)]), "auto-2-kills");
        assert_eq!(label(&[at(0.0, Kind::Streak("first_blood".into()), Some(true))]), "auto-first-blood");
        assert_eq!(label(&[kill(0.0), at(0.0, Kind::Objective("midboss".into()), Some(true))]), "auto-objective");
        assert_eq!(label(&[at(0.0, Kind::Death, Some(true))]), "auto-death");
        assert_eq!(label(&[kill(0.0), at(1.0, Kind::Result("de/feat".into()), Some(true))]), "auto-defeat");
    }
}
