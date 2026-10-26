//! The installed font families the font list offers. They're read on a thread of their own as
//! soon as an editor opens, and each family's font is loaded there too once its row is about to
//! show, so the list opens and scrolls without waiting on font files: until a family is ready
//! its row shows the name in the UI's face. Windows that asked to be woken ([`wake`]) get a
//! message whenever the families arrive or another family is ready, so they repaint without
//! waiting for input.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, Once, OnceLock};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
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
pub(crate) fn start() {
    START.call_once(|| {
        let spawned = std::thread::Builder::new().name("glint fonts".into()).spawn(work);
        if let Err(e) = spawned {
            glint_core::log::log(&format!("font list: {e}"));
        }
    });
}

/// A window to post `msg` to when the families change; `posted` until it says it's seen it.
struct Waker {
    hwnd: isize,
    msg: u32,
    posted: AtomicBool,
}

static WAKERS: Mutex<Vec<Arc<Waker>>> = Mutex::new(Vec::new());

/// From now on `hwnd` gets `msg` posted when the families arrive or a family is ready; one
/// message at a time, the next once it calls [`woken`].
pub(crate) fn wake(hwnd: HWND, msg: u32) {
    if let Ok(mut wakers) = WAKERS.lock() {
        wakers.retain(|w| w.hwnd != hwnd.0 as isize);
        wakers.push(Arc::new(Waker { hwnd: hwnd.0 as isize, msg, posted: AtomicBool::new(false) }));
    }
}

/// `hwnd` handled its message: the next change posts another.
pub(crate) fn woken(hwnd: HWND) {
    if let Ok(wakers) = WAKERS.lock() {
        wakers.iter().filter(|w| w.hwnd == hwnd.0 as isize).for_each(|w| w.posted.store(false, Ordering::Release));
    }
}

/// `hwnd` no longer wants the messages (it's going away).
pub(crate) fn unwake(hwnd: HWND) {
    if let Ok(mut wakers) = WAKERS.lock() {
        wakers.retain(|w| w.hwnd != hwnd.0 as isize);
    }
}

/// Tells the windows that asked that the families changed.
fn notify() {
    let Ok(wakers) = WAKERS.lock() else { return };
    for w in wakers.iter() {
        if !w.posted.swap(true, Ordering::AcqRel) {
            let _ = unsafe { PostMessageW(Some(HWND(w.hwnd as _)), w.msg, WPARAM(0), LPARAM(0)) };
        }
    }
}

/// Tests stand in for a cold font cache: `GLINT_TEST_FONT_DELAY_MS` slows the font thread.
fn test_delay(part: u32) {
    #[cfg(test)]
    if let Some(ms) = std::env::var("GLINT_TEST_FONT_DELAY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms / part as u64));
    }
    #[cfg(not(test))]
    let _ = part;
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
    // the families are read through a factory of the thread's own, so reading them never holds up
    // the UI thread's text; the fonts are laid out once through the shared one, which keeps them
    let Ok(dwrite) = (unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) }) else { return };
    let Ok(own) = (unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_ISOLATED) }) else { return };
    test_delay(1);
    let Some((collection, found)) = enumerate(&own) else { return };
    let families = ordered(found);
    let names: Vec<Arc<str>> = families.iter().map(|(n, _)| n.clone()).collect();
    if let Ok(mut state) = CATALOG.state.lock() {
        state.0 = vec![Load::Unknown; names.len()];
    }
    let _ = CATALOG.names.set(Arc::new(names));
    notify();
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
        test_delay(20);
        let symbol = load(&dwrite, &collection, &families[i]);
        if let Ok(mut state) = CATALOG.state.lock() {
            state.0[i] = Load::Ready { symbol };
        }
        notify();
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

    #[test]
    fn changes_post_once_until_seen() {
        const MSG: u32 = 0x8000 + 77;
        let hwnd = probe::window();
        wake(hwnd, MSG);
        notify();
        notify();
        assert_eq!(probe::count(hwnd, MSG), 1, "one message until it's seen");
        woken(hwnd);
        notify();
        assert_eq!(probe::count(hwnd, MSG), 1, "seen: the next change posts again");
        unwake(hwnd);
        notify();
        assert_eq!(probe::count(hwnd, MSG), 0, "gone: no more");
        probe::close(hwnd);
    }
}

/// A message-only window for tests to be woken through: never shown, never focused.
#[cfg(test)]
pub(super) mod probe {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, WINDOW_EX_STYLE, WINDOW_STYLE,
    };
    use windows::core::w;

    pub(in super::super) fn window() -> HWND {
        unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), None, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, None, None)
        }
        .expect("a message-only window")
    }

    /// How many `msg`s were waiting for `hwnd` (taken off its queue).
    pub(in super::super) fn count(hwnd: HWND, msg: u32) -> usize {
        let mut n = 0;
        let mut m = MSG::default();
        while unsafe { PeekMessageW(&mut m, Some(hwnd), msg, msg, PM_REMOVE) }.as_bool() {
            n += 1;
        }
        n
    }

    pub(in super::super) fn close(hwnd: HWND) {
        let _ = unsafe { DestroyWindow(hwnd) };
    }
}
