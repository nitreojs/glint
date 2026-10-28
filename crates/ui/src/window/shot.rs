//! Test runs' own screenshots: with `GLINT_SHOT_DIR` set, posting the registered window message
//! `glint-shot` to one of our windows saves exactly what it renders next, alpha and all, as
//! `<GLINT_SHOT_DIR>/<name>.png`. It works on a covered, unfocused window, needs no focus or
//! cursor, and `tools/shot.ps1` drives it. WPARAM is a global atom holding the name (the window
//! deletes it); 0 numbers the shots instead.
//!
//! Drag and drop the same way: `glint-drop` with a global atom holding a file's path in WPARAM
//! holds that file over the window (LPARAM 0), drops it (1) or takes it away (2), as winit's OLE
//! drop target reports a real drag (which needs the real cursor): egui's input then holds it in
//! `hovered_files` / `dropped_files` the same way.
//!
//! Modifiers the same way: `glint-key` with a virtual key in WPARAM marks it held (LPARAM 1) or
//! let go (0) in the window thread's key state, which winit reads for ctrl, shift and alt; posted
//! key messages never touch it. Post it held before the posted keys and let go after them.
//!
//! Such a window keeps the pointer where the last posted `WM_MOUSEMOVE` put it: Windows answers
//! the first move with a `WM_MOUSELEAVE` at once (the real cursor is elsewhere), which would drop
//! the posted click that follows.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use eframe::egui::{ColorImage, Context, DroppedFile, DroppedFileHandle, HoveredFile};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{GlobalDeleteAtom, GlobalGetAtomNameW};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, SetKeyboardState};
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
static DROP_MESSAGE: LazyLock<u32> = LazyLock::new(|| unsafe { RegisterWindowMessageW(w!("glint-drop")) });
/// The file `glint-drop` holds over the window, and those it dropped not yet handed to egui.
static HELD: Mutex<Option<PathBuf>> = Mutex::new(None);
static DROPPED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static KEY_MESSAGE: LazyLock<u32> = LazyLock::new(|| unsafe { RegisterWindowMessageW(w!("glint-key")) });

/// From the window procedure: whether `msg` is the shot mode's to handle (a shot asked for,
/// which is then taken next frame, a file dragged, a key marked held, or the pointer leaving).
pub(super) fn message(msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
    if DIR.is_none() {
        return false;
    }
    if msg == WM_MOUSELEAVE {
        return true;
    }
    if msg == *DROP_MESSAGE {
        let path = atom_text(wp).map(PathBuf::from);
        let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
        *held = None;
        match lp.0 {
            0 => *held = path,
            1 => DROPPED.lock().unwrap_or_else(|e| e.into_inner()).extend(path),
            _ => {}
        }
    } else if msg == *MESSAGE {
        let name = atom_name(wp).unwrap_or_else(|| format!("shot-{}", COUNT.fetch_add(1, Ordering::Relaxed) + 1));
        ASKED.lock().unwrap_or_else(|e| e.into_inner()).push(name);
    } else if msg == *KEY_MESSAGE {
        let mut state = [0u8; 256];
        if wp.0 < state.len() && unsafe { GetKeyboardState(&mut state) }.is_ok() {
            state[wp.0] = if lp.0 == 1 { 0x80 } else { 0 };
            let _ = unsafe { SetKeyboardState(&state) };
        }
        return true;
    } else {
        return false;
    }
    // a covered or idle window paints only when asked to
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
    true
}

/// Call every frame from the window's `ui`, before it reads its input: takes the shots asked for
/// and hands egui the files `glint-drop` holds or dropped. Does nothing unless `GLINT_SHOT_DIR`
/// is set.
pub fn shots(ctx: &Context) {
    let Some(dir) = DIR.as_ref() else { return };
    CTX.get_or_init(|| ctx.clone());
    let held = HELD.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let dropped = std::mem::take(&mut *DROPPED.lock().unwrap_or_else(|e| e.into_inner()));
    if held.is_some() || !dropped.is_empty() {
        ctx.input_mut(|i| {
            i.raw.hovered_files.extend(held.map(|p| HoveredFile { path: Some(p), ..Default::default() }));
            i.raw.dropped_files.extend(dropped.into_iter().map(dropped_file));
        });
    }
    let asked = std::mem::take(&mut *ASKED.lock().unwrap_or_else(|e| e.into_inner()));
    for name in asked {
        let path = dir.join(format!("{name}.png"));
        ctx.request_screenshot(move |image| {
            std::thread::spawn(move || save(&image, &path));
        });
    }
}

/// A file on disk as egui's input holds a dropped one, as egui-winit makes them.
pub fn dropped_file(path: PathBuf) -> DroppedFileHandle {
    Arc::new(OnDisk(path))
}

#[derive(Debug)]
struct OnDisk(PathBuf);

impl DroppedFile for OnDisk {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// The text the atom in `wp` holds; the atom is freed.
fn atom_text(wp: WPARAM) -> Option<String> {
    let atom = u16::try_from(wp.0).ok().filter(|&a| a != 0)?;
    let mut buf = [0u16; 256];
    let len = unsafe { GlobalGetAtomNameW(atom, &mut buf) } as usize;
    unsafe { GlobalDeleteAtom(atom) };
    Some(String::from_utf16_lossy(&buf[..len])).filter(|t| !t.is_empty())
}

/// The name the atom in `wp` holds, cut down to what's safe in a file name; the atom is freed.
fn atom_name(wp: WPARAM) -> Option<String> {
    let name: String = atom_text(wp)?
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
