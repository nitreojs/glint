//! The list of captures, read off the UI thread: glint's and afterglow's histories and output
//! folders (each app's that ran here, the window's own always), read again whenever one changes
//! (checked every second) or the window asks. How long what moves plays comes from the history,
//! the length cache or a quick look at the file, after the list is out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use glint_core::gallery::length::Lengths;
use glint_core::config::{DEFAULT_ACCENT, parse_hex};
use glint_core::gallery::source::AMBER;
use glint_core::gallery::{self, Item, Kind, Maker, OnDisk, Shelf, Source, path_id};
use glint_core::history;
use glint_core::log::log;
use glint_core::paths::appdata;
use glint_core::record::mp4;
use glint_shell::explorer::{documents, videos};

use super::play;

/// Past this many files the folder isn't only the app's: stop looking.
const MOST: usize = 20_000;
const EVERY: Duration = Duration::from_secs(1);

pub(super) struct Loaded {
    pub items: Vec<Item>,
    /// Each app's mark, by [`Maker::ALL`].
    pub accents: [[u8; 3]; 2],
    /// How long reading took.
    pub took: Duration,
}

pub(super) struct Library {
    rx: Receiver<Loaded>,
    again: Sender<()>,
}

impl Library {
    /// Starts reading at once for the gallery of `own` (whose captures are listed even before it
    /// made any), keeping lengths in the thumbnail cache `cache`; `ctx` is set once the window
    /// exists, so it can be woken.
    pub fn start(own: Maker, cache: PathBuf, ctx: std::sync::Arc<std::sync::OnceLock<egui::Context>>) -> Library {
        let (tx, rx) = mpsc::channel();
        let (again, asked) = mpsc::channel();
        let spawned = std::thread::Builder::new().name("library".into()).spawn(move || watch(own, &cache, &tx, &asked, &ctx));
        if let Err(e) = spawned {
            log(&format!("gallery: can't read the captures: {e}"));
        }
        Library { rx, again }
    }

    /// The newest list, if one came since the last call.
    pub fn poll(&self) -> Option<Loaded> {
        self.rx.try_iter().last()
    }

    /// Read everything again now (the window came back to the front).
    pub fn refresh(&self) {
        let _ = self.again.send(());
    }
}

/// Where each app's captures are: `own`'s always, the other's once it ran here.
pub fn sources(own: Maker) -> Vec<Source> {
    let appdata = appdata();
    let user = |name: &str| std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(name);
    Maker::ALL
        .into_iter()
        .filter(|&m| m == own || m.installed(&appdata))
        .map(|m| {
            Source::read(m, &appdata, || match m {
                Maker::Glint => documents().unwrap_or_else(|| user("Documents")).join("glint"),
                Maker::Afterglow => videos().unwrap_or_else(|| m.dir(&appdata)).join("afterglow"),
            })
        })
        .collect()
}

/// How long `path` plays: an mp4's movie header, a gif's frame delays.
pub fn length(path: &Path) -> Option<u32> {
    match Kind::of(path)? {
        Kind::Recording => mp4::duration_ms(path),
        Kind::Gif => Some(play::gif_length(path)).filter(|ms| *ms > 0),
        Kind::Shot => None,
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn watch(own: Maker, cache: &Path, tx: &Sender<Loaded>, asked: &Receiver<()>, ctx: &std::sync::OnceLock<egui::Context>) {
    let mut seen = None;
    let mut sources = Vec::new();
    let mut forced = true;
    let mut lengths = Lengths::load(cache);
    let wake = || {
        if let Some(ctx) = ctx.get() {
            ctx.request_repaint();
        }
    };
    loop {
        if forced {
            // the settings may have moved a folder since, or the other app may have come
            sources = self::sources(own);
        }
        let stamp: Vec<_> = sources.iter().map(|s| (mtime(&s.history), mtime(&s.root), s.root.clone())).collect();
        if forced || seen.as_ref() != Some(&stamp) {
            seen = Some(stamp);
            let started = Instant::now();
            let mut items = list(&sources);
            let missing = lengths.fill(&mut items);
            let took = started.elapsed();
            let mut accents = [parse_hex(DEFAULT_ACCENT).unwrap_or_default(), AMBER];
            for s in &sources {
                accents[s.maker as usize] = s.accent;
            }
            if !missing.is_empty() {
                // out first, then each file it doesn't know the length of is looked at once
                if tx.send(Loaded { items: items.clone(), accents, took }).is_err() {
                    return;
                }
                wake();
                lengths.measure(&mut items, &missing, |i| length(&i.path));
                lengths.store(&items);
            }
            if tx.send(Loaded { items, accents, took }).is_err() {
                return;
            }
            wake();
        }
        forced = match asked.recv_timeout(EVERY) {
            Ok(()) => {
                while asked.try_recv().is_ok() {}
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return,
        };
    }
}

fn nanos(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

/// What the gallery lists: the captures each source's history remembers and what's in its folder
/// as deep as it goes, newest first.
pub fn list(sources: &[Source]) -> Vec<Item> {
    let shelves: Vec<Shelf> = sources
        .iter()
        .map(|s| {
            let mut found = Vec::new();
            walk(&s.root, s.depth, &mut found);
            Shelf { source: s, history: history::all_in(&s.history), found }
        })
        .collect();
    let on_disk: HashMap<u64, (u64, u64)> =
        shelves.iter().flat_map(|s| &s.found).map(|f| (path_id(&f.path), (f.size, f.modified))).collect();
    let stat = |p: &Path| match on_disk.get(&path_id(p)) {
        Some(&s) => Some(s),
        // outside the folders (one moved since, or an export saved elsewhere)
        None => std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| (m.len(), m.modified().map(nanos).unwrap_or(0))),
    };
    gallery::collect(shelves, stat)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<OnDisk>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if out.len() >= MOST {
            return;
        }
        let Ok(kind) = e.file_type() else { continue };
        let path = e.path();
        if kind.is_dir() {
            let hidden = e.file_name().to_string_lossy().starts_with('.');
            if depth > 1 && !hidden {
                walk(&path, depth - 1, out);
            }
        } else if kind.is_file()
            && Kind::of(&path).is_some()
            && let Ok(m) = e.metadata()
        {
            out.push(OnDisk { path, size: m.len(), modified: m.modified().map(nanos).unwrap_or(0) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::history::{Entry, Kind as HistoryKind};

    /// A file written `hours` ago.
    fn file(p: PathBuf, hours: u64, bytes: &[u8]) -> PathBuf {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        let at = SystemTime::now() - Duration::from_secs(hours * 3600);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(at).unwrap();
        p
    }

    /// The smallest mp4 this reads a length from: a movie header saying `ms` at 1 kHz.
    fn movie(ms: u32) -> Vec<u8> {
        let atom = |kind: &[u8; 4], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes(), kind.as_slice(), body].concat();
        let mvhd = [[0u8; 12].as_slice(), &1000u32.to_be_bytes(), &ms.to_be_bytes(), &[0; 80]].concat();
        [atom(b"ftyp", b"isom"), atom(b"mdat", &[0; 64]), atom(b"moov", &atom(b"mvhd", &mvhd))].concat()
    }

    #[test]
    fn both_apps_histories_and_folders_make_one_library() {
        let dir = std::env::temp_dir().join(format!("glint-library-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (glint_root, afterglow_root, elsewhere) = (dir.join("glint videos"), dir.join("afterglow videos"), dir.join("old folder"));
        let source = |maker, root: &Path, depth, app_folder| Source {
            maker,
            history: dir.join(maker.name()).join("history.jsonl"),
            root: root.to_path_buf(),
            depth,
            app_folder,
            accent: [0; 3],
        };
        let glint = source(Maker::Glint, &glint_root, 3, None);
        let afterglow = source(Maker::Afterglow, &afterglow_root, 2, Some(0));
        for s in [&glint, &afterglow] {
            std::fs::create_dir_all(s.history.parent().unwrap()).unwrap();
        }
        let shot = file(glint_root.join("glint_1.png"), 5, b"png");
        let _found_gif = file(glint_root.join("gifs").join("party.gif"), 4, b"gif");
        let clip = file(afterglow_root.join("Counter-Strike 2").join("clip.mp4"), 3, &movie(61_000));
        let _found_clip = file(afterglow_root.join("Discord").join("call.mp4"), 2, &movie(9_500));
        let _too_deep = file(afterglow_root.join("a").join("b").join("lost.mp4"), 0, &movie(1));
        let earlier = file(elsewhere.join("afterglow_old.mp4"), 48, &movie(30_000));
        let _notes = file(afterglow_root.join("notes.txt"), 0, b"text");
        // afterglow saved three (one deleted since); glint took a screenshot and uploaded afterglow's clip
        let add = |s: &Source, e: Entry| history::add_to(&s.history, e);
        let cs = glint_core::naming::Source { app: "Counter-Strike 2".into(), path: r"C:\cs2\cs2.exe".into(), ..Default::default() };
        add(&afterglow, Entry::new(HistoryKind::Recording, Some(&earlier), 9).with_duration(Some(30_000)));
        add(&afterglow, Entry::new(HistoryKind::Recording, Some(&clip), 9).with_source(Some(&cs)).with_duration(Some(61_000)));
        add(&afterglow, Entry::new(HistoryKind::Recording, Some(&afterglow_root.join("gone.mp4")), 9));
        add(&glint, Entry::new(HistoryKind::Shot, Some(&shot), 3));
        // the upload comes after the save
        let later = Entry::new(HistoryKind::Recording, Some(&clip), 9).with_url("https://x/clip.mp4");
        add(&glint, Entry { time: later.time + 60, ..later });

        let items = list(&[glint.clone(), afterglow.clone()]);
        let mut got: Vec<(String, Maker)> = items.iter().map(|i| (i.name(), i.maker)).collect();
        got.sort();
        assert_eq!(
            got,
            [
                ("afterglow_old.mp4".to_string(), Maker::Afterglow),
                ("call.mp4".to_string(), Maker::Afterglow),
                ("clip.mp4".to_string(), Maker::Afterglow),
                ("glint_1.png".to_string(), Maker::Glint),
                ("party.gif".to_string(), Maker::Glint),
            ],
            "each once, nothing too deep, gone or not a capture"
        );
        let by = |n: &str| items.iter().find(|i| i.name() == n).unwrap();
        assert_eq!(by("clip.mp4").url.as_deref(), Some("https://x/clip.mp4"));
        assert_eq!(by("clip.mp4").exe.as_deref(), Some(r"C:\cs2\cs2.exe"));
        assert_eq!(by("call.mp4").app.as_deref(), Some("Discord"), "from the folder named after the app");
        assert_eq!(by("call.mp4").duration_ms, None, "not known before a look");

        // the cache fills in what a look finds, and a second start doesn't look again
        let cache = dir.join("thumbs");
        std::fs::create_dir_all(&cache).unwrap();
        let mut lengths = Lengths::load(&cache);
        let mut items = items;
        let missing = lengths.fill(&mut items);
        assert_eq!(missing.len(), 2, "the found clip and gif");
        lengths.measure(&mut items, &missing, |i| length(&i.path));
        lengths.store(&items);
        assert_eq!(items.iter().find(|i| i.name() == "call.mp4").unwrap().duration_ms, Some(9_500));
        assert_eq!(items.iter().find(|i| i.name() == "party.gif").unwrap().duration_ms, None, "not a real gif");
        let mut again = list(&[glint, afterglow]);
        assert!(Lengths::load(&cache).fill(&mut again).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
