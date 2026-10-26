//! The installed font families the font list offers. They're read on a thread of their own as
//! soon as an editor opens, and each family's font is loaded there too once its row is about to
//! show, so the list opens and scrolls without waiting on font files: until a family is ready
//! its row shows the name in the UI's face.

use std::sync::{Arc, Condvar, LazyLock, Mutex, Once, OnceLock};

use windows::Win32::Graphics::DirectWrite::*;
use windows::core::{BOOL, HSTRING, w};

/// Families offered first, when they're installed.
const FAVOURITES: [&str; 13] = [
    "Segoe UI",
    "Segoe UI Variable Display",
    "Arial",
    "Calibri",
    "Bahnschrift",
    "Georgia",
    "Times New Roman",
    "Consolas",
    "Cascadia Code",
    "Comic Sans MS",
    "Impact",
    "Segoe Print",
    "Ink Free",
];

/// Where a family's font stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    Unknown,
    Wanted,
    /// Loaded; a symbol font draws pictures, not letters.
    Ready { symbol: bool },
}

struct Catalog {
    names: OnceLock<Arc<Vec<Arc<str>>>>,
    /// Each family's load state, and the families asked for (the latest last).
    state: Mutex<(Vec<Load>, Vec<usize>)>,
    wake: Condvar,
}

static CATALOG: LazyLock<Catalog> =
    LazyLock::new(|| Catalog { names: OnceLock::new(), state: Mutex::new((Vec::new(), Vec::new())), wake: Condvar::new() });
static START: Once = Once::new();

/// Starts reading the families (once per process).
pub(super) fn start() {
    START.call_once(|| {
        let spawned = std::thread::Builder::new().name("glint fonts".into()).spawn(work);
        if let Err(e) = spawned {
            glint_core::log::log(&format!("font list: {e}"));
        }
    });
}

/// The installed families, favourites first, then the rest alphabetically; `None` while they're
/// still being read.
pub(super) fn names() -> Option<Arc<Vec<Arc<str>>>> {
    CATALOG.names.get().cloned()
}

/// Whether family `i` (of [`names`]) is loaded, and if so whether it's a symbol font. Asks for
/// it when it isn't.
pub(super) fn ready(i: usize) -> Option<bool> {
    let mut state = CATALOG.state.lock().ok()?;
    let (loads, queue) = &mut *state;
    match loads.get(i).copied()? {
        Load::Ready { symbol } => Some(symbol),
        Load::Wanted => None,
        Load::Unknown => {
            loads[i] = Load::Wanted;
            queue.push(i);
            CATALOG.wake.notify_one();
            None
        }
    }
}

fn work() {
    let Ok(dwrite) = (unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }) else { return };
    let Some((collection, found)) = enumerate(&dwrite) else { return };
    let families = ordered(found);
    let names: Vec<Arc<str>> = families.iter().map(|(n, _)| n.clone()).collect();
    if let Ok(mut state) = CATALOG.state.lock() {
        state.0 = vec![Load::Unknown; names.len()];
    }
    let _ = CATALOG.names.set(Arc::new(names));
    loop {
        let i = {
            let Ok(mut state) = CATALOG.state.lock() else { return };
            loop {
                if let Some(i) = state.1.pop() {
                    break i;
                }
                let Ok(next) = CATALOG.wake.wait(state) else { return };
                state = next;
            }
        };
        let symbol = load(&dwrite, &collection, &families[i]);
        if let Ok(mut state) = CATALOG.state.lock() {
            state.0[i] = Load::Ready { symbol };
        }
    }
}

/// Loads family `(name, index)`'s regular font and lays its name out once, so drawing it on the
/// UI thread finds everything ready. Returns whether it's a symbol font.
fn load(dwrite: &IDWriteFactory, collection: &IDWriteFontCollection, (name, index): &(Arc<str>, u32)) -> bool {
    unsafe {
        let Ok(family) = collection.GetFontFamily(*index) else { return false };
        let Ok(font) = family.GetFirstMatchingFont(DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL)
        else {
            return false;
        };
        let symbol = font.IsSymbolFont().as_bool();
        let _ = font.CreateFontFace();
        let format = dwrite.CreateTextFormat(
            &HSTRING::from(&**name),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            15.0,
            w!("en-us"),
        );
        if let Ok(format) = format {
            let text: Vec<u16> = name.encode_utf16().collect();
            if let Ok(layout) = dwrite.CreateTextLayout(&text, &format, 1000.0, 100.0) {
                let mut m = DWRITE_TEXT_METRICS::default();
                let _ = layout.GetMetrics(&mut m);
            }
        }
        symbol
    }
}

/// Every installed family's name and its index in the system collection.
fn enumerate(dwrite: &IDWriteFactory) -> Option<(IDWriteFontCollection, Vec<(String, u32)>)> {
    let mut names = Vec::new();
    unsafe {
        let mut collection = None;
        dwrite.GetSystemFontCollection(&mut collection, false).ok()?;
        let collection = collection?;
        for i in 0..collection.GetFontFamilyCount() {
            let Ok(family) = collection.GetFontFamily(i) else { continue };
            let Ok(localized) = family.GetFamilyNames() else { continue };
            let (mut index, mut exists) = (0, BOOL(0));
            let _ = localized.FindLocaleName(w!("en-us"), &mut index, &mut exists);
            if !exists.as_bool() {
                index = 0;
            }
            let Ok(len) = localized.GetStringLength(index) else { continue };
            let mut buf = vec![0u16; len as usize + 1];
            if localized.GetString(index, &mut buf).is_ok() {
                names.push((String::from_utf16_lossy(&buf[..len as usize]), i));
            }
        }
        Some((collection, names))
    }
}

/// `families` with the favourites that are there first, then the rest alphabetically.
fn ordered(mut families: Vec<(String, u32)>) -> Vec<(Arc<str>, u32)> {
    families.sort_by_key(|(n, _)| n.to_lowercase());
    families.dedup_by(|a, b| a.0 == b.0);
    let favourite = |n: &str| FAVOURITES.iter().position(|f| *f == n);
    let mut first: Vec<&(String, u32)> = families.iter().filter(|(n, _)| favourite(n).is_some()).collect();
    first.sort_by_key(|(n, _)| favourite(n));
    let rest = families.iter().filter(|(n, _)| favourite(n).is_none());
    first.into_iter().chain(rest).map(|(n, i)| (Arc::from(n.as_str()), *i)).collect()
}

/// Where the families in `names` matching `query` (any case, anywhere in the name) sit, in
/// order; names starting with it come first.
pub(super) fn search(names: &[Arc<str>], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..names.len()).collect();
    }
    let (mut starts, mut inside) = (Vec::new(), Vec::new());
    for (i, n) in names.iter().enumerate() {
        let lower = n.to_lowercase();
        if lower.starts_with(&q) {
            starts.push(i);
        } else if lower.contains(&q) {
            inside.push(i);
        }
    }
    starts.extend(inside);
    starts
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn favourites_and_prefixes_come_first() {
        let found = ["Zapf", "Arial", "Segoe UI", "Consolas", "Segoe Script", "Calibri Light"];
        let families = ordered(found.iter().enumerate().map(|(i, n)| (n.to_string(), i as u32)).collect());
        let names: Vec<&str> = families.iter().map(|(n, _)| &**n).collect();
        assert_eq!(names, ["Segoe UI", "Arial", "Consolas", "Calibri Light", "Segoe Script", "Zapf"]);
        assert_eq!(families[0].1, 2, "each keeps its index in the collection");
        let all: Vec<Arc<str>> = ["Segoe UI", "Calibri", "Consolas", "Segoe Script"].map(Arc::from).to_vec();
        assert_eq!(search(&all, "SC"), [3]);
        assert_eq!(search(&all, "c"), [1, 2, 3], "prefix matches first");
        assert_eq!(search(&all, " "), [0, 1, 2, 3]);
    }

    #[test]
    fn families_load_in_the_background() {
        start();
        let until = Instant::now() + Duration::from_secs(10);
        let names = loop {
            if let Some(n) = names() {
                break n;
            }
            assert!(Instant::now() < until, "the families never arrived");
            std::thread::sleep(Duration::from_millis(5));
        };
        let i = names.iter().position(|n| &**n == "Segoe UI").expect("Segoe UI is installed");
        while ready(i).is_none() {
            assert!(Instant::now() < until, "Segoe UI never loaded");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(ready(i), Some(false), "not a symbol font");
    }
}
