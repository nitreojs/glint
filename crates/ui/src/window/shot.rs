//! Test runs' own screenshots: with `GLINT_SHOT_DIR` set, posting the registered window message
//! `glint-shot` to one of our windows saves exactly what it renders next, alpha and all, as
//! `<GLINT_SHOT_DIR>/<name>.png`. It works on a covered, unfocused window, needs no focus or
//! cursor, and `tools/shot.ps1` drives it. WPARAM is a global atom holding the name (the window
//! deletes it); 0 numbers the shots instead.
//!
//! Such a window keeps the pointer where the last posted `WM_MOUSEMOVE` put it: Windows answers
//! the first move with a `WM_MOUSELEAVE` at once (the real cursor is elsewhere), which would drop
//! the posted click that follows.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use eframe::egui::{ColorImage, Context};
use windows::Win32::Foundation::WPARAM;
use windows::Win32::System::DataExchange::{GlobalDeleteAtom, GlobalGetAtomNameW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::WindowsAndMessaging::RegisterWindowMessageW;
use windows::core::w;

/// Where shots go; `None` outside test runs, which leaves all of this idle.
static DIR: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var_os("GLINT_SHOT_DIR").filter(|d| !d.is_empty()).map(PathBuf::from));
static MESSAGE: LazyLock<u32> = LazyLock::new(|| unsafe { RegisterWindowMessageW(w!("glint-shot")) });
/// The window's context, to wake it when a shot is asked for.
static CTX: OnceLock<Context> = OnceLock::new();
/// Names asked for and not yet requested from egui.
static ASKED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static COUNT: AtomicU32 = AtomicU32::new(0);

/// From the window procedure: whether `msg` is the shot mode's to handle (a shot asked for,
/// which is then taken next frame, or the pointer leaving).
pub(super) fn message(msg: u32, wp: WPARAM) -> bool {
    if DIR.is_none() {
        return false;
    }
    if msg == WM_MOUSELEAVE {
        return true;
    }
    if msg != *MESSAGE {
        return false;
    }
    let name = atom_name(wp).unwrap_or_else(|| format!("shot-{}", COUNT.fetch_add(1, Ordering::Relaxed) + 1));
    ASKED.lock().unwrap_or_else(|e| e.into_inner()).push(name);
    // a covered or idle window paints only when asked to
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
    true
}

/// Call every frame from the window's `ui`: takes the shots asked for. Does nothing unless
/// `GLINT_SHOT_DIR` is set.
pub fn shots(ctx: &Context) {
    let Some(dir) = DIR.as_ref() else { return };
    CTX.get_or_init(|| ctx.clone());
    let asked = std::mem::take(&mut *ASKED.lock().unwrap_or_else(|e| e.into_inner()));
    for name in asked {
        let path = dir.join(format!("{name}.png"));
        ctx.request_screenshot(move |image| {
            std::thread::spawn(move || save(&image, &path));
        });
    }
}

/// The name the atom in `wp` holds, cut down to what's safe in a file name; the atom is freed.
fn atom_name(wp: WPARAM) -> Option<String> {
    let atom = u16::try_from(wp.0).ok().filter(|&a| a != 0)?;
    let mut buf = [0u16; 256];
    let len = unsafe { GlobalGetAtomNameW(atom, &mut buf) } as usize;
    unsafe { GlobalDeleteAtom(atom) };
    let name: String = String::from_utf16_lossy(&buf[..len])
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect();
    let name = name.trim_matches('.');
    (!name.is_empty()).then(|| name.to_owned())
}

/// Writes the shot whole or not at all, so a script waiting for the file never reads half of it.
fn save(image: &Arc<ColorImage>, path: &std::path::Path) {
    let [w, h] = image.size;
    let pixels: Vec<u32> = image
        .pixels
        .iter()
        .map(|p| {
            let [r, g, b, a] = p.to_srgba_unmultiplied();
            glint_media::image::straight(u32::from_be_bytes([a, r, g, b]))
        })
        .collect();
    let png = match glint_media::image::encode(&pixels, w as u32, h as u32, true, glint_media::image::ImageFormat::Png, 100)
    {
        Ok(png) => png,
        Err(e) => {
            eprintln!("glint-shot {}: {e}", path.display());
            return;
        }
    };
    let part = path.with_extension("png.part");
    if std::fs::write(&part, png).and_then(|()| std::fs::rename(&part, path)).is_err() {
        eprintln!("glint-shot: can't write {}", path.display());
        let _ = std::fs::remove_file(&part);
    }
}
