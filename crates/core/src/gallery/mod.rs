//! What the gallery window lists: every capture glint and afterglow know of (their histories, and
//! the files in their output folders the histories lost track of), newest first, narrowed by
//! [`filter`] and search, grouped by day; [`grid`] lays it out, [`thumb`] names its thumbnail
//! cache, [`length`] says how long what moves plays, and [`source`] finds each app's captures.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::history::{Entry, Local};

pub mod filter;
pub mod grid;
pub mod length;
pub mod source;
pub mod thumb;

pub use source::{Maker, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Shot,
    Recording,
    Gif,
}

impl Kind {
    /// What a file is by its extension; `None` for what glint doesn't make.
    pub fn of(path: &Path) -> Option<Kind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" => Some(Kind::Shot),
            "mp4" => Some(Kind::Recording),
            "gif" => Some(Kind::Gif),
            _ => None,
        }
    }

    /// It moves: it previews by playing.
    pub fn moves(self) -> bool {
        self != Kind::Shot
    }
}

/// A file found on disk: its size and last write in nanoseconds since the unix epoch.
#[derive(Clone, Debug, PartialEq)]
pub struct OnDisk {
    pub path: PathBuf,
    pub size: u64,
    pub modified: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Stable across reloads: the path's [`path_id`].
    pub id: u64,
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    /// When it was made, unix seconds.
    pub time: u64,
    /// Last write in nanoseconds: with `size`, when it changed.
    pub modified: u64,
    pub url: Option<String>,
    /// The app it shows.
    pub app: Option<String>,
    /// That app's exe, for its icon.
    pub exe: Option<String>,
    /// How long it plays, once known; never for screenshots.
    pub duration_ms: Option<u32>,
    pub maker: Maker,
    /// The name, its folder below the output folder and the app, lowercased: what search looks in.
    haystack: String,
}

/// A path as Windows compares them: `\` separators, case-insensitive.
fn normalized(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// FNV-1a, stable across runs and builds (unlike `std`'s hasher).
pub(crate) fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

pub fn path_id(path: &Path) -> u64 {
    fnv(normalized(path).into_bytes())
}

/// What a history line adds to a file on disk.
#[derive(Default)]
struct Known {
    time: Option<u64>,
    url: Option<String>,
    app: Option<String>,
    exe: Option<String>,
    duration_ms: Option<u32>,
}

impl Item {
    fn new(file: OnDisk, kind: Kind, known: Known, source: &Source) -> Item {
        let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let below = file.path.parent().and_then(|p| p.strip_prefix(&source.root).ok());
        let folder = below.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        // a capture in the folder the pattern names after the app shows that app
        let app = known.app.or_else(|| {
            let k = source.app_folder?;
            let named = below?.components().nth(k)?.as_os_str().to_string_lossy().into_owned();
            // what a capture of no one app's window goes into
            (!["other", "screen"].contains(&named.as_str())).then_some(named)
        });
        let haystack = [name.as_str(), folder.as_str(), app.as_deref().unwrap_or("")].join("\n").to_lowercase();
        let time = known.time.unwrap_or(file.modified / 1_000_000_000);
        Item {
            id: path_id(&file.path),
            path: file.path,
            kind,
            size: file.size,
            time,
            modified: file.modified,
            url: known.url,
            exe: known.exe.filter(|_| app.is_some()),
            app,
            duration_ms: known.duration_ms.filter(|_| kind.moves()),
            maker: source.maker,
            haystack,
        }
    }

    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Holds every one of the search's `words`.
    pub fn holds(&self, words: &[String]) -> bool {
        words.iter().all(|w| self.haystack.contains(w.as_str()))
    }
}

/// One app's captures as read: its history (oldest first, as [`crate::history::all_in`] gives
/// it) and the files found in its folder.
pub struct Shelf<'a> {
    pub source: &'a Source,
    pub history: Vec<Entry>,
    pub found: Vec<OnDisk>,
}

/// Every capture the `shelves` hold, once each, newest first: the histories' entries whose files
/// are still there, then the found files no history points at. A capture both histories hold is
/// the one that wrote it first (the other only uploaded or trimmed it in place); a file in both
/// folders is the app's whose folder is deeper. `stat` gives a file's size and last write, `None`
/// once it's gone.
pub fn collect(shelves: Vec<Shelf>, stat: impl Fn(&Path) -> Option<(u64, u64)>) -> Vec<Item> {
    // by path: the entry, filled in from later lines elsewhere, and whose it is
    let mut entries: Vec<(Entry, &Source)> = Vec::new();
    let mut at: HashMap<u64, usize> = HashMap::new();
    for shelf in &shelves {
        for e in &shelf.history {
            let Some(path) = &e.path else { continue };
            if Kind::of(path).is_none() {
                continue;
            }
            match at.get(&path_id(path)) {
                Some(&i) => {
                    let (first, by) = &mut entries[i];
                    let (first, other) = if e.time < first.time {
                        let old = std::mem::replace(first, e.clone());
                        *by = shelf.source;
                        (first, old)
                    } else {
                        (first, e.clone())
                    };
                    first.url = first.url.take().or(other.url);
                    if first.app.is_none() {
                        (first.app, first.exe) = (other.app, other.exe);
                    }
                    first.duration_ms = first.duration_ms.or(other.duration_ms);
                }
                None => {
                    at.insert(path_id(path), entries.len());
                    entries.push((e.clone(), shelf.source));
                }
            }
        }
    }
    let mut items = Vec::with_capacity(entries.len() + shelves.iter().map(|s| s.found.len()).sum::<usize>());
    // newest first, so equal times keep the history's order once sorted
    for (e, source) in entries.into_iter().rev() {
        let Some(path) = e.path else { continue };
        let Some(kind) = Kind::of(&path) else { continue };
        let Some((size, modified)) = stat(&path) else { continue };
        let known = Known { time: Some(e.time), url: e.url, app: e.app, exe: e.exe, duration_ms: e.duration_ms };
        items.push(Item::new(OnDisk { path, size, modified }, kind, known, source));
    }
    let mut found: Vec<(OnDisk, &Source)> = Vec::new();
    let mut found_at: HashMap<u64, usize> = HashMap::new();
    let depth = |s: &Source| s.root.components().count();
    for shelf in &shelves {
        for f in &shelf.found {
            let id = path_id(&f.path);
            if Kind::of(&f.path).is_none() || at.contains_key(&id) {
                continue;
            }
            match found_at.get(&id) {
                Some(&i) if depth(shelf.source) > depth(found[i].1) => found[i].1 = shelf.source,
                Some(_) => {}
                None => {
                    found_at.insert(id, found.len());
                    found.push((f.clone(), shelf.source));
                }
            }
        }
    }
    for (f, source) in found {
        let Some(kind) = Kind::of(&f.path) else { continue };
        items.push(Item::new(f, kind, Known::default(), source));
    }
    // stable: entries made in the same second keep their order
    items.sort_by_key(|i| std::cmp::Reverse(i.time));
    items
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Shots,
    Recordings,
    Gifs,
}

impl Filter {
    pub fn admits(self, kind: Kind) -> bool {
        matches!(
            (self, kind),
            (Filter::All, _) | (Filter::Shots, Kind::Shot) | (Filter::Recordings, Kind::Recording) | (Filter::Gifs, Kind::Gif)
        )
    }
}

/// What's typed in the search box as lowercase words, each of which has to match.
pub fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// A local calendar date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Date {
    pub year: u16,
    pub month: u16,
    pub day: u16,
}

impl Date {
    pub fn of(t: Local) -> Date {
        Date { year: t.year, month: t.month, day: t.day }
    }

    /// Days since 1970-01-01 (proleptic gregorian).
    pub fn number(self) -> i64 {
        let (m, d) = (self.month as i64, self.day as i64);
        let y = self.year as i64 - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The date `n` days after 1970-01-01.
    pub fn from_number(n: i64) -> Date {
        let z = n + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + i64::from(month <= 2);
        Date { year: year as u16, month: month as u16, day: day as u16 }
    }

    /// 0 on a monday, 6 on a sunday.
    pub fn weekday(self) -> i64 {
        (self.number() + 3).rem_euclid(7)
    }

    /// How many days its month has.
    pub fn days_in_month(self) -> u16 {
        let next = if self.month == 12 { Date { year: self.year + 1, month: 1, day: 1 } } else { Date { month: self.month + 1, day: 1, ..self } };
        (next.number() - Date { day: 1, ..self }.number()) as u16
    }

    /// `today`, `yesterday`, the weekday within the week, then `oct 3` (and the year when it's another).
    pub fn label(self, today: Date) -> String {
        const WEEKDAYS: [&str; 7] = ["thursday", "friday", "saturday", "sunday", "monday", "tuesday", "wednesday"];
        let n = self.number();
        match today.number() - n {
            0 => "today".into(),
            1 => "yesterday".into(),
            2..=6 => WEEKDAYS[n.rem_euclid(7) as usize].into(),
            _ if self.year == today.year => format!("{} {}", crate::history::month(self.month).to_lowercase(), self.day),
            _ => format!("{} {}, {}", crate::history::month(self.month).to_lowercase(), self.day, self.year),
        }
    }
}

/// A run of items made on one day.
#[derive(Clone, Debug, PartialEq)]
pub struct Day {
    pub date: Option<Date>,
    pub items: Range<usize>,
}

/// Splits a newest-first list into days by the local date of each `times` entry.
pub fn days(times: impl IntoIterator<Item = u64>, local: impl Fn(u64) -> Option<Local>) -> Vec<Day> {
    let mut out: Vec<Day> = Vec::new();
    for (i, t) in times.into_iter().enumerate() {
        let date = local(t).map(Date::of);
        match out.last_mut() {
            Some(d) if d.date == date => d.items.end = i + 1,
            _ => out.push(Day { date, items: i..i + 1 }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Kind as HistoryKind;

    fn entry(path: &str, time: u64, app: Option<&str>) -> Entry {
        let kind = if path.ends_with(".png") { HistoryKind::Shot } else { HistoryKind::Recording };
        Entry { time, ..Entry::new(kind, Some(Path::new(path)), 1).with_app(app, None) }
    }

    fn disk(path: &str, secs: u64) -> OnDisk {
        OnDisk { path: PathBuf::from(path), size: 7, modified: secs * 1_000_000_000 + 5 }
    }

    /// A folder the tests list, in this platform's own path form.
    fn base(name: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" }).join(name)
    }

    fn root() -> PathBuf {
        base("caps")
    }

    /// `rel` ('/'-separated) inside `root`.
    fn under(root: &Path, rel: &str) -> String {
        rel.split('/').fold(root.to_path_buf(), |p, s| p.join(s)).to_string_lossy().into_owned()
    }

    fn at(rel: &str) -> String {
        under(&root(), rel)
    }

    fn source(maker: Maker, root: PathBuf) -> Source {
        Source { maker, history: PathBuf::new(), root, depth: 3, app_folder: None, accent: [0; 3] }
    }

    /// One app's captures, listed alone.
    fn alone(history: &[Entry], found: Vec<OnDisk>, stat: impl Fn(&Path) -> Option<(u64, u64)>) -> Vec<Item> {
        let glint = source(Maker::Glint, root());
        collect(vec![Shelf { source: &glint, history: history.to_vec(), found }], stat)
    }

    #[test]
    fn history_and_found_files_merge_newest_first_without_doubles() {
        let history = [
            entry(&at("a.png"), 100, Some("Telegram Desktop")),
            entry(&at("gone.png"), 150, None),
            entry(&at("b.mp4"), 200, None),
            entry(&at("notes.txt"), 250, None),
        ];
        let found = vec![disk(&at("A.PNG"), 999), disk(&at("old/c.gif"), 50), disk(&at("d.jpeg"), 300)];
        let stat = |p: &Path| (!p.ends_with("gone.png")).then_some((42, 9));
        let items = alone(&history, found, stat);
        let names: Vec<String> = items.iter().map(Item::name).collect();
        assert_eq!(names, ["d.jpeg", "b.mp4", "a.png", "c.gif"]);
        let kinds: Vec<Kind> = items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [Kind::Shot, Kind::Recording, Kind::Shot, Kind::Gif]);
        // the history's own time and size from disk; found files by their last write
        assert_eq!((items[2].time, items[2].size, items[2].modified), (100, 42, 9));
        assert_eq!((items[0].time, items[0].size), (300, 7));
        assert_eq!(items[2].id, path_id(Path::new(&at("A.PNG"))));
        assert!(items.iter().all(|i| i.maker == Maker::Glint));
        #[cfg(windows)]
        assert_eq!(items[2].id, path_id(Path::new(r"c:/CAPS/a.png")), "either slash, any case");
    }

    #[test]
    fn two_apps_make_one_list_and_each_capture_keeps_who_made_it() {
        let (g_root, a_root) = (base("glint videos"), base("afterglow videos"));
        let g = |rel: &str| under(&g_root, rel);
        let a = |rel: &str| under(&a_root, rel);
        let glint = Source { app_folder: Some(0), ..source(Maker::Glint, g_root.clone()) };
        // afterglow's folder sits inside glint's too: what's in it is afterglow's
        let inner = g_root.join("replays");
        let afterglow = Source { app_folder: Some(0), ..source(Maker::Afterglow, a_root.clone()) };
        let afterglow_inner = Source { root: inner.clone(), ..afterglow.clone() };
        let measured = Entry { exe: Some(r"C:\cs2\cs2.exe".into()), ..entry(&a("cs.mp4"), 300, Some("Counter-Strike 2")) }.with_duration(Some(61_000));
        let glint_history = vec![
            entry(&g("shot.png"), 100, None),
            // glint uploaded afterglow's clip later: it stays afterglow's, made when afterglow saved it
            Entry { time: 900, ..entry(&a("cs.mp4"), 900, None).with_url("https://x/cs.mp4") },
            // the trimmer's export of it is glint's
            entry(&a("cs_trimmed.mp4"), 950, None).with_duration(Some(12_000)),
        ];
        let afterglow_history = vec![measured, entry(&a("gone.mp4"), 310, None)];
        let shelves = vec![
            Shelf {
                source: &glint,
                history: glint_history,
                found: vec![disk(&g("shot.png"), 100), disk(&g("Discord/old.png"), 50), disk(&under(&inner, "r1.mp4"), 60)],
            },
            Shelf { source: &afterglow, history: afterglow_history, found: vec![disk(&a("cs.mp4"), 300), disk(&a("other/clip.mp4"), 40)] },
            Shelf { source: &afterglow_inner, history: Vec::new(), found: vec![disk(&under(&inner, "R1.MP4"), 60)] },
        ];
        let items = collect(shelves, |p| (!p.ends_with("gone.mp4")).then_some((10, 20)));
        let got: Vec<(String, Maker)> = items.iter().map(|i| (i.name(), i.maker)).collect();
        assert_eq!(
            got,
            [
                ("cs_trimmed.mp4".into(), Maker::Glint),
                ("cs.mp4".into(), Maker::Afterglow),
                ("shot.png".into(), Maker::Glint),
                ("r1.mp4".into(), Maker::Afterglow),
                ("old.png".into(), Maker::Glint),
                ("clip.mp4".into(), Maker::Afterglow),
            ]
        );
        let cs = &items[1];
        assert_eq!(cs.time, 300, "made when afterglow saved it, not when glint uploaded it");
        assert_eq!(cs.url.as_deref(), Some("https://x/cs.mp4"));
        assert_eq!((cs.app.as_deref(), cs.exe.as_deref(), cs.duration_ms), (Some("Counter-Strike 2"), Some(r"C:\cs2\cs2.exe"), Some(61_000)));
        assert_eq!(items[0].duration_ms, Some(12_000));
        // a found file in an app's folder shows that app; "other" is no app's
        assert_eq!(items[4].app.as_deref(), Some("Discord"));
        assert_eq!(items[5].app, None);
        assert_eq!(items[2].duration_ms, None);
        let words = words("discord");
        assert_eq!(items.iter().filter(|i| i.holds(&words)).map(Item::name).collect::<Vec<_>>(), ["old.png"]);
    }

    #[test]
    fn search_looks_at_name_folder_and_app() {
        let history = [entry(&at("Discord/clip 1.mp4"), 1, None), entry(&at("glint_1.png"), 2, Some("Telegram Desktop"))];
        let found = vec![disk(&at("party.gif"), 0)];
        let items = alone(&history, found, |_| Some((1, 1)));
        let shown = |q: &str| -> Vec<String> {
            let w = words(q);
            items.iter().filter(|i| i.holds(&w)).map(Item::name).collect()
        };
        assert_eq!(shown(""), ["glint_1.png", "clip 1.mp4", "party.gif"]);
        assert_eq!(shown("TELEGRAM"), ["glint_1.png"]);
        assert_eq!(shown("discord clip"), ["clip 1.mp4"]);
        assert_eq!(shown("discord png"), Vec::<String>::new());
        assert_eq!(shown("  .GIF "), ["party.gif"]);
        // the output folder itself isn't searched
        assert_eq!(shown("caps"), Vec::<String>::new());
    }

    #[test]
    fn days_split_on_the_local_date() {
        let local = |t: u64| Some(Local { year: 2026, month: 10, day: (t / 100) as u16, hour: 0, minute: 0 });
        let d = days([2599, 2510, 2400, 2300, 2350], local);
        let date = |day| Some(Date { year: 2026, month: 10, day });
        assert_eq!(
            d,
            [Day { date: date(25), items: 0..2 }, Day { date: date(24), items: 2..3 }, Day { date: date(23), items: 3..5 }]
        );
        assert!(days([], local).is_empty());
        assert_eq!(days([5, 6], |_| None), [Day { date: None, items: 0..2 }]);
    }

    #[test]
    fn day_labels_get_less_precise_with_age() {
        let today = Date { year: 2026, month: 10, day: 25 }; // a sunday
        let at = |year, month, day| Date { year, month, day }.label(today);
        assert_eq!(at(2026, 10, 25), "today");
        assert_eq!(at(2026, 10, 24), "yesterday");
        assert_eq!(at(2026, 10, 23), "friday");
        assert_eq!(at(2026, 10, 19), "monday");
        assert_eq!(at(2026, 10, 18), "oct 18");
        assert_eq!(at(2026, 3, 1), "mar 1");
        assert_eq!(at(2025, 12, 31), "dec 31, 2025");
        let new_year = Date { year: 2027, month: 1, day: 1 };
        assert_eq!(Date { year: 2026, month: 12, day: 31 }.label(new_year), "yesterday");
        assert_eq!(Date { year: 2024, month: 2, day: 29 }.number() + 1, Date { year: 2024, month: 3, day: 1 }.number());
        assert_eq!(Date { year: 1970, month: 1, day: 1 }.number(), 0);
        for n in [-1, 0, 59, 11_016, 20_386, 20_751, 47_482] {
            assert_eq!(Date::from_number(n).number(), n);
        }
        assert_eq!(Date::from_number(20_751), Date { year: 2026, month: 10, day: 25 });
        assert_eq!((today.weekday(), Date { year: 2026, month: 10, day: 19 }.weekday()), (6, 0));
        assert_eq!(Date { year: 2024, month: 2, day: 9 }.days_in_month(), 29);
        assert_eq!(Date { year: 2026, month: 12, day: 31 }.days_in_month(), 31);
    }
}
