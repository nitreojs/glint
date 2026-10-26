//! What the gallery window lists: every capture glint knows of (its history, and the files in the
//! output folder the history lost track of), newest first, filtered by kind and search, grouped by
//! day; [`grid`] lays it out and [`thumb`] names its thumbnail cache.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::history::{Entry, Local};

pub mod grid;
pub mod thumb;

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
    pub app: Option<String>,
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

impl Item {
    fn new(file: OnDisk, kind: Kind, time: u64, url: Option<String>, app: Option<String>, root: &Path) -> Item {
        let name = file.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let folder = file
            .path
            .parent()
            .and_then(|p| p.strip_prefix(root).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let haystack = [name.as_str(), folder.as_str(), app.as_deref().unwrap_or("")].join("\n").to_lowercase();
        Item {
            id: path_id(&file.path),
            path: file.path,
            kind,
            size: file.size,
            time,
            modified: file.modified,
            url,
            app,
            haystack,
        }
    }

    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Shown for `filter` and holds every one of the search's `words`.
    pub fn matches(&self, filter: Filter, words: &[String]) -> bool {
        filter.admits(self.kind) && words.iter().all(|w| self.haystack.contains(w.as_str()))
    }
}

/// The history's entries (oldest first) whose files are still there, then the `found` files nothing
/// in it points at, newest first. `stat` gives a file's size and last write, `None` once it's gone;
/// `root` is the output folder, whose subfolders search looks in.
pub fn collect(history: &[Entry], found: Vec<OnDisk>, root: &Path, stat: impl Fn(&Path) -> Option<(u64, u64)>) -> Vec<Item> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for e in history.iter().rev() {
        let Some(path) = &e.path else { continue };
        let Some(kind) = Kind::of(path) else { continue };
        if !seen.insert(path_id(path)) {
            continue;
        }
        let Some((size, modified)) = stat(path) else { continue };
        let file = OnDisk { path: path.clone(), size, modified };
        items.push(Item::new(file, kind, e.time, e.url.clone(), e.app.clone(), root));
    }
    for f in found {
        let Some(kind) = Kind::of(&f.path) else { continue };
        if seen.insert(path_id(&f.path)) {
            let time = f.modified / 1_000_000_000;
            items.push(Item::new(f, kind, time, None, None, root));
        }
    }
    // stable: entries made in the same second keep the history's order
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
    fn number(self) -> i64 {
        let (m, d) = (self.month as i64, self.day as i64);
        let y = self.year as i64 - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
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
        Entry { kind, path: Some(PathBuf::from(path)), url: None, size: 1, time, app: app.map(str::to_string) }
    }

    fn disk(path: &str, secs: u64) -> OnDisk {
        OnDisk { path: PathBuf::from(path), size: 7, modified: secs * 1_000_000_000 + 5 }
    }

    /// The output folder the tests list, in this platform's own path form.
    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\caps" } else { "/caps" })
    }

    /// `rel` ('/'-separated) inside [`root`].
    fn at(rel: &str) -> String {
        rel.split('/').fold(root(), |p, s| p.join(s)).to_string_lossy().into_owned()
    }

    #[test]
    fn history_and_found_files_merge_newest_first_without_doubles() {
        let root = root();
        let history = [
            entry(&at("a.png"), 100, Some("Telegram Desktop")),
            entry(&at("gone.png"), 150, None),
            entry(&at("b.mp4"), 200, None),
            entry(&at("notes.txt"), 250, None),
        ];
        let found = vec![disk(&at("A.PNG"), 999), disk(&at("old/c.gif"), 50), disk(&at("d.jpeg"), 300)];
        let stat = |p: &Path| (!p.ends_with("gone.png")).then_some((42, 9));
        let items = collect(&history, found, &root, stat);
        let names: Vec<String> = items.iter().map(Item::name).collect();
        assert_eq!(names, ["d.jpeg", "b.mp4", "a.png", "c.gif"]);
        let kinds: Vec<Kind> = items.iter().map(|i| i.kind).collect();
        assert_eq!(kinds, [Kind::Shot, Kind::Recording, Kind::Shot, Kind::Gif]);
        // the history's own time and size from disk; found files by their last write
        assert_eq!((items[2].time, items[2].size, items[2].modified), (100, 42, 9));
        assert_eq!((items[0].time, items[0].size), (300, 7));
        assert_eq!(items[2].id, path_id(Path::new(&at("A.PNG"))));
        #[cfg(windows)]
        assert_eq!(items[2].id, path_id(Path::new(r"c:/CAPS/a.png")), "either slash, any case");
    }

    #[test]
    fn filter_and_search_look_at_kind_name_folder_and_app() {
        let root = root();
        let history = [entry(&at("Discord/clip 1.mp4"), 1, None), entry(&at("glint_1.png"), 2, Some("Telegram Desktop"))];
        let found = vec![disk(&at("party.gif"), 0)];
        let items = collect(&history, found, &root, |_| Some((1, 1)));
        let shown = |filter: Filter, q: &str| -> Vec<String> {
            let w = words(q);
            items.iter().filter(|i| i.matches(filter, &w)).map(Item::name).collect()
        };
        assert_eq!(shown(Filter::All, ""), ["glint_1.png", "clip 1.mp4", "party.gif"]);
        assert_eq!(shown(Filter::Shots, ""), ["glint_1.png"]);
        assert_eq!(shown(Filter::Recordings, ""), ["clip 1.mp4"]);
        assert_eq!(shown(Filter::Gifs, ""), ["party.gif"]);
        assert_eq!(shown(Filter::All, "TELEGRAM"), ["glint_1.png"]);
        assert_eq!(shown(Filter::All, "discord clip"), ["clip 1.mp4"]);
        assert_eq!(shown(Filter::All, "discord png"), Vec::<String>::new());
        assert_eq!(shown(Filter::Shots, "discord"), Vec::<String>::new());
        assert_eq!(shown(Filter::All, "  .GIF "), ["party.gif"]);
        // the output folder itself isn't searched
        assert_eq!(shown(Filter::All, "caps"), Vec::<String>::new());
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
    }
}
