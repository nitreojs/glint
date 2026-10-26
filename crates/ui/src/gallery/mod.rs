//! The gallery both apps open with `--gallery [file]`: every capture the app made, newest first
//! and grouped by day, as a grid of thumbnails. Hovering a tile for half a second grows it into a
//! bigger preview (recordings and gifs play in it, muted); a click opens it, the context menu and
//! the keys do the rest, and the gear opens the app's settings. The app says where its captures
//! come from and which kinds it makes ([`Setup`]), and what opening one does and which lines of
//! its own the menu has ([`Host`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event, Id, Key, Modifiers, Pos2, Rect, Ui, UiBuilder, pos2, vec2};
use glint_core::gallery::grid::{Grid, Spec, Step};
use glint_core::gallery::thumb::{self, Meta};
use glint_core::gallery::{self, Day, Filter, Item, Kind, path_id};
use glint_core::history::Local;
use glint_core::icon::icon_rgba;
use glint_core::log::log;
use glint_core::paths::app_dir_for;
use glint_shell::clipboard;
use glint_shell::time::local_time;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

use crate::widgets::{self, BAR_H, Look, TitleAction};
use crate::window;

mod decode;
mod library;
mod overlay;
mod play;
mod textures;
mod thumbs;
mod view;

pub use library::list;
use library::Library;
use play::Playback;
use textures::{Texture, Textures};
use thumbs::{Done, Job, Pool};
use view::Flip;

pub const GALLERY_ARG: &str = "--gallery";
/// What starts an app's settings window.
const SETTINGS_ARG: &str = "--settings";
const WINDOW: [f32; 2] = [1120.0, 760.0];
const MIN_WINDOW: [f32; 2] = [640.0, 480.0];
/// The toolbar under the title bar: the filter, the search box and the gear.
const TOOL_H: f32 = 52.0;
/// Tiles are at least this wide (ctrl+wheel changes it), 16:10.
const TILE: f32 = 220.0;
const TILE_RANGE: (f32, f32) = (140.0, 440.0);
/// How long a tile is hovered before it grows.
const HOVER: f64 = 0.5;
/// How long a deletion can be undone before the file goes to the recycle bin.
const UNDO: f64 = 6.0;
/// New textures a frame at most, so a burst of thumbnails doesn't stall one.
const UPLOADS_PER_FRAME: usize = 3;
/// Textures kept at least; far-away ones beyond this go.
const KEEP_TEXTURES: usize = 240;

/// What an app's gallery is made of.
pub struct Setup {
    /// The app's name: the window is "<app> gallery", its thumbnails are cached in
    /// `%LOCALAPPDATA%\<app>\thumbs` and its size is kept in `%APPDATA%\<app>\gallery.json`.
    pub app: &'static str,
    pub accent: [u8; 3],
    /// What the app makes; with more than one, the toolbar filters by kind.
    pub kinds: &'static [Kind],
    /// The app's history of captures (`history.jsonl`).
    pub history: PathBuf,
    /// The folder captures are saved in and how many folders deep they go; read again whenever
    /// the window comes back to the front (the settings may have moved it).
    pub folder: fn() -> (PathBuf, usize),
    /// The settings window's title: the gear brings it forward, or starts `<exe> --settings`.
    pub settings: &'static str,
    /// What an empty gallery says.
    pub empty: &'static str,
}

/// A line of the app's own in an item's menu.
pub struct Extra {
    pub key: u32,
    pub name: &'static str,
    /// Right under the first line; otherwise right above delete.
    pub top: bool,
}

/// What work on an item ends with: the toast's text, and a link it offers to open.
pub struct Note {
    pub text: String,
    pub link: Option<String>,
}

/// Work on one item running in the background (an upload); its tile shows a moving bar until it
/// ends. An error is the toast's text.
pub type Work = JoinHandle<Result<Note, String>>;

/// What the app does with its items, on the window's thread.
pub trait Host {
    /// The menu's first line for `item`: what a click and enter do.
    fn open_label(&self, _item: &Item) -> &'static str {
        "open"
    }

    fn open(&mut self, item: &Item) -> Result<(), String>;

    /// The app's own lines in `item`'s menu; `busy` while work on it runs.
    fn extras(&self, _item: &Item, _busy: bool) -> Vec<Extra> {
        Vec::new()
    }

    /// The extra `key` was picked on `item`; work it starts runs with a bar on the tile.
    fn pick(&mut self, _key: u32, _item: &Item, _owner: HWND) -> Result<Option<Work>, String> {
        Ok(None)
    }

    /// The window came back to the front: the settings may have changed since.
    fn refocus(&mut self) {}
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
struct Saved {
    rect: Option<[i32; 4]>,
    tile: Option<f32>,
}

impl Saved {
    fn load(path: &Path) -> Saved {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_default()).unwrap_or_default()
    }

    fn store(&self, path: &Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
}

/// Opens `path` in the app Windows has for it.
pub fn open_file(path: &Path) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(path.as_os_str()), None, None, SW_SHOWNORMAL);
    }
}

fn title(app: &str) -> String {
    format!("{app} gallery")
}

/// Starts this exe with `arg` (and `file`).
fn spawn(arg: &str, file: Option<&Path>) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg(arg);
    if let Some(p) = file {
        cmd.arg(p);
    }
    cmd.spawn().map(|_| ())
}

/// Brings `app`'s gallery forward, or starts it (on `select`, when given).
pub fn open(app: &str, select: Option<&Path>) {
    if let Some(h) = window::find(&title(app)) {
        return window::bring_forward(h);
    }
    if let Err(e) = spawn(GALLERY_ARG, select) {
        log(&format!("can't open the gallery: {e}"));
    }
}

pub fn run(setup: Setup, host: impl Host + 'static, select: Option<PathBuf>) {
    let title = title(setup.app);
    if let Some(h) = window::find(&title) {
        return window::bring_forward(h);
    }
    let started = Instant::now();
    let ctx_cell = Arc::new(OnceLock::new());
    // the list is read while the window comes up
    let library = Library::start(setup.history.clone(), setup.folder, setup.kinds, ctx_cell.clone());
    let saved_at = app_dir_for(setup.app).join("gallery.json");
    let saved = Saved::load(&saved_at);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size(WINDOW)
            .with_min_inner_size(MIN_WINDOW)
            .with_resizable(true)
            .with_decorations(false)
            .with_icon(egui::IconData { rgba: icon_rgba(64, setup.accent), width: 64, height: 64 }),
        centered: saved.rect.is_none(),
        ..Default::default()
    };
    let _ = eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            crate::theme::fonts(&cc.egui_ctx);
            let _ = ctx_cell.set(cc.egui_ctx.clone());
            let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(HWND(h.hwnd.get() as _)),
                _ => None,
            };
            let rounded = hwnd.is_some_and(|h| window::frameless(h, true));
            if let (Some(h), Some(r)) = (hwnd, saved.rect) {
                window::place(h, r);
            }
            let mut app = Gallery::new(cc, setup, Box::new(host), library, saved, saved_at, started)?;
            (app.hwnd, app.rounded, app.select) = (hwnd, rounded, select.as_deref().map(path_id));
            Ok(Box::new(app))
        }),
    );
}

/// A thumbnail on the GPU.
struct Tex {
    tex: Texture,
    /// When it came, for its fade-in.
    at: f64,
    /// The frame it was last drawn in.
    used: u64,
}

/// The grown preview of one item.
struct Preview {
    id: u64,
    /// Opened with the keyboard: stays until space / esc, follows the focus.
    pinned: bool,
    /// The tile it grows out of, on screen, as last seen.
    tile: Rect,
    /// Where it's drawn now, for hovering.
    rect: Rect,
    /// The picture at its shown size (screenshots), or the playing frame.
    full: Option<Texture>,
    /// When `full` came, for its fade over the thumbnail.
    full_at: f64,
    play: Option<Playback>,
    /// The grow motion: since when, from and to (0 in its tile, 1 grown).
    grow: (f64, f32, f32),
    closing: bool,
}

impl Preview {
    /// How far it has grown, 0..=1.
    fn progress(&self, now: f64) -> f32 {
        let (start, from, to) = self.grow;
        let k = if widgets::motion() { ((now - start) as f32 / view::GROW).clamp(0.0, 1.0) } else { 1.0 };
        from + (to - from) * k
    }
}

struct Menu {
    id: u64,
    at: Pos2,
}

#[derive(Clone)]
enum Action {
    Undo,
    Link(String),
}

struct Toast {
    text: String,
    action: Option<(&'static str, Action)>,
    at: f64,
    warn: bool,
}

/// Work the app runs on an item.
struct Busy {
    id: u64,
    work: Option<Work>,
}

/// Deleted items waiting for their undo to run out. One undo brings all of them back, so they all
/// wait for the latest deletion's.
#[derive(Default)]
struct Deletions {
    items: Vec<Item>,
    last: f64,
}

impl Deletions {
    fn add(&mut self, item: Item, now: f64) {
        self.items.push(item);
        self.last = now;
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn holds(&self, id: u64) -> bool {
        self.items.iter().any(|i| i.id == id)
    }

    /// Keeps the undo open a little longer (its toast is under the pointer).
    fn hold(&mut self, now: f64) {
        if !self.items.is_empty() {
            self.last = self.last.max(now - UNDO + 0.5);
        }
    }

    /// When they go to the recycle bin.
    fn due_at(&self) -> Option<f64> {
        (!self.items.is_empty()).then_some(self.last + UNDO)
    }

    /// Brings everything back; the id of the last one deleted.
    fn undo(&mut self) -> Option<u64> {
        let last = self.items.last().map(|i| i.id);
        self.items.clear();
        last
    }

    /// What goes to the recycle bin now: everything once the undo ran out, or with `all`.
    fn due(&mut self, now: f64, all: bool) -> Vec<Item> {
        if all || self.due_at().is_some_and(|at| now >= at) { std::mem::take(&mut self.items) } else { Vec::new() }
    }
}

struct Gallery {
    setup: Setup,
    host: Box<dyn Host>,
    hwnd: Option<HWND>,
    rounded: bool,
    light: Option<bool>,
    keyboard: bool,
    saved: Saved,
    saved_at: PathBuf,
    /// The thumbnail cache.
    cache: PathBuf,
    started: Instant,
    /// Open time is logged once the first view has its thumbnails.
    reported: bool,
    now: f64,
    frame: u64,
    focused: bool,

    library: Library,
    loaded: bool,
    items: Vec<Item>,
    /// Each item's local time, made once per list.
    locals: Vec<Option<Local>>,
    filter: Filter,
    query: String,
    /// Indices into `items`: what the filter and search let through, newest first.
    shown: Vec<usize>,
    days: Vec<Day>,
    grid: Grid,
    spec: Option<Spec>,
    /// The list or the filter changed: the grid has to be laid out again.
    dirty: bool,
    flip: Flip,

    pool: Pool,
    textures: Textures,
    tex: HashMap<u64, Tex>,
    meta: HashMap<u64, Meta>,
    failed: HashSet<u64>,

    /// The tile under the pointer and since when.
    hover: Option<(u64, f64)>,
    preview: Option<Preview>,
    fading: Option<Preview>,
    focus: Option<u64>,
    /// Scroll so this item is in view.
    scroll_to: Option<u64>,
    /// A keyboard preview waiting for its tile to come into view.
    follow: Option<u64>,
    /// The file the gallery was opened on, until the list holds it.
    select: Option<u64>,
    /// Drawn tiles on screen this frame.
    drawn: HashMap<u64, Rect>,
    /// The part of the window the grid scrolls in.
    panel: Rect,
    search_id: Option<Id>,
    menu: Option<Menu>,
    toast: Option<Toast>,
    pending: Deletions,
    busy: Vec<Busy>,
    copying: Option<JoinHandle<Result<(), String>>>,
}

impl Gallery {
    fn new(
        cc: &eframe::CreationContext<'_>,
        setup: Setup,
        host: Box<dyn Host>,
        library: Library,
        saved: Saved,
        saved_at: PathBuf,
        started: Instant,
    ) -> Result<Gallery, String> {
        let gl = cc.gl.clone().ok_or("the gallery draws with OpenGL, and there's none")?;
        let cache = thumbs::cache_dir(setup.app);
        Ok(Gallery {
            pool: Pool::start(&cc.egui_ctx, cache.clone()),
            textures: Textures::new(gl),
            setup,
            host,
            hwnd: None,
            rounded: false,
            light: None,
            keyboard: false,
            saved,
            saved_at,
            cache,
            started,
            reported: false,
            now: 0.0,
            frame: 0,
            focused: true,
            library,
            loaded: false,
            items: Vec::new(),
            locals: Vec::new(),
            filter: Filter::All,
            query: String::new(),
            shown: Vec::new(),
            days: Vec::new(),
            grid: Grid::default(),
            spec: None,
            dirty: true,
            flip: Flip::default(),
            tex: HashMap::new(),
            meta: HashMap::new(),
            failed: HashSet::new(),
            hover: None,
            preview: None,
            fading: None,
            focus: None,
            scroll_to: None,
            follow: None,
            select: None,
            drawn: HashMap::new(),
            panel: Rect::NOTHING,
            search_id: None,
            menu: None,
            toast: None,
            pending: Deletions::default(),
            busy: Vec::new(),
            copying: None,
        })
    }

    fn owner(&self) -> HWND {
        self.hwnd.unwrap_or_default()
    }

    fn tile_width(&self) -> f32 {
        self.saved.tile.unwrap_or(TILE).clamp(TILE_RANGE.0, TILE_RANGE.1)
    }

    fn item(&self, id: u64) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Where `id` is among the shown items.
    fn index(&self, id: u64) -> Option<usize> {
        self.shown.iter().position(|&i| self.items[i].id == id)
    }

    fn shown_item(&self, index: usize) -> Option<&Item> {
        self.shown.get(index).map(|&i| &self.items[i])
    }

    /// Filters and groups the list again; what moves, comes or goes animates.
    fn refilter(&mut self) {
        let words = gallery::words(&self.query);
        self.shown = (0..self.items.len())
            .filter(|&i| !self.pending.holds(self.items[i].id) && self.items[i].matches(self.filter, &words))
            .collect();
        // grouped by position in `items`, whose local times are made already
        self.days = gallery::days(self.shown.iter().map(|&i| i as u64), |i| self.locals[i as usize]);
        self.dirty = true;
    }

    fn set_items(&mut self, loaded: library::Loaded) {
        let first = !self.loaded;
        self.loaded = true;
        self.locals = loaded.items.iter().map(|i| local_time(i.time)).collect();
        // a file that changed gets a new thumbnail
        for it in &loaded.items {
            if let Some(old) = self.items.iter().find(|o| o.id == it.id)
                && (old.size, old.modified) != (it.size, it.modified)
            {
                self.tex.remove(&it.id);
                self.meta.remove(&it.id);
                self.failed.remove(&it.id);
            }
        }
        let fresh = loaded.items.first().map(|i| i.id).filter(|id| !first && self.item(*id).is_none());
        self.items = loaded.items;
        self.refilter();
        if first {
            log(&format!(
                "gallery: {} captures in {} ms (listed in {} ms)",
                self.items.len(),
                self.started.elapsed().as_millis(),
                loaded.took.as_millis()
            ));
            let live = self.items.iter().map(|i| thumb::key(&i.path, i.size, i.modified)).collect();
            let cache = self.cache.clone();
            let _ = std::thread::Builder::new().name("sweep".into()).spawn(move || thumbs::sweep(&cache, live));
        }
        if let Some(id) = self.select.filter(|id| self.index(*id).is_some()) {
            self.select = None;
            self.focus = Some(id);
            self.scroll_to = Some(id);
            self.keyboard = true;
        } else if let Some(id) = fresh {
            // something was just captured: it's what comes into focus
            self.focus = Some(id);
            self.scroll_to = Some(id);
        }
    }

    fn poll(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(l) = self.library.poll() {
            self.set_items(l);
        }
        // every texture this frame may take is made before anything is uploaded: the thumbnails,
        // a screenshot's preview and a playing preview's first frame
        self.textures.reserve(frame, UPLOADS_PER_FRAME + 2);
        let mut uploaded = 0;
        while uploaded < UPLOADS_PER_FRAME {
            let Some(done) = self.pool.poll() else { break };
            match done {
                Done::Thumb { id, image, meta } => {
                    self.meta.insert(id, meta);
                    match image {
                        Some(image) => {
                            if let Some(tex) = self.textures.upload(&image, true) {
                                self.tex.insert(id, Tex { tex, at: self.now, used: self.frame });
                            }
                            uploaded += 1;
                        }
                        None => {
                            self.failed.insert(id);
                        }
                    }
                }
                Done::Full { id, image } => {
                    if let (Some(pv), Some(image)) = (&mut self.preview, image)
                        && pv.id == id
                    {
                        pv.full = self.textures.upload(&image, false);
                        pv.full_at = self.now;
                    }
                }
            }
        }
        if uploaded == UPLOADS_PER_FRAME {
            ctx.request_repaint();
        }
        self.poll_busy(ctx);
        if let Some(t) = self.copying.take_if(|t| t.is_finished()) {
            match t.join().unwrap_or_else(|_| Err("the copy crashed".into())) {
                Ok(()) => self.say("copied, paste it anywhere", None, false),
                Err(e) => self.say(&format!("couldn't copy: {e}"), None, true),
            }
        } else if self.copying.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if self.pending.due_at().is_some_and(|at| self.now >= at) {
            self.commit(false);
        }
        if !self.pending.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn say(&mut self, text: &str, action: Option<(&'static str, Action)>, warn: bool) {
        self.toast = Some(Toast { text: text.to_string(), action, at: self.now, warn });
    }

    // ---- actions --------------------------------------------------------------------------------

    fn open_item(&mut self, id: u64) {
        let Some(item) = self.item(id).cloned() else { return };
        if let Err(e) = self.host.open(&item) {
            self.say(&e, None, true);
        }
    }

    /// Screenshots go on the clipboard as pictures, the rest (or with `file`) as files.
    fn copy(&mut self, id: u64, file: bool) {
        let Some(item) = self.item(id).cloned() else { return };
        if item.kind != Kind::Shot || file {
            match clipboard::copy_files(self.hwnd, std::slice::from_ref(&item.path)) {
                Ok(()) => self.say("copied, paste it anywhere", None, false),
                Err(e) => self.say(&format!("couldn't copy: {e}"), None, true),
            }
            return;
        }
        if self.copying.is_some() {
            return;
        }
        let owner = self.owner().0 as isize;
        self.copying = Some(std::thread::spawn(move || {
            let _com = glint_media::clip::media::Com::init();
            let wic = decode::Wic::new()?;
            let (px, _) = wic.file(&item.path, |w, h| (w, h))?;
            let alpha = px.alpha();
            clipboard::copy_image(Some(HWND(owner as _)), &px.argb(), px.w, px.h, alpha)
        }));
    }

    fn copy_link(&mut self, url: &str) {
        match clipboard::copy_text(self.hwnd, url) {
            Ok(()) => self.say("link copied", None, false),
            Err(e) => self.say(&format!("couldn't copy the link: {e}"), None, true),
        }
    }

    fn is_busy(&self, id: u64) -> bool {
        self.busy.iter().any(|b| b.id == id)
    }

    /// One of the app's own menu lines.
    fn pick(&mut self, key: u32, id: u64) {
        let Some(item) = self.item(id).cloned() else { return };
        if self.is_busy(id) {
            return;
        }
        match self.host.pick(key, &item, self.owner()) {
            Ok(Some(work)) => self.busy.push(Busy { id, work: Some(work) }),
            Ok(None) => {}
            Err(e) => self.say(&e, None, true),
        }
    }

    fn poll_busy(&mut self, ctx: &egui::Context) {
        if self.busy.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        let mut i = 0;
        while i < self.busy.len() {
            let Some(work) = self.busy[i].work.take_if(|t| t.is_finished()) else {
                i += 1;
                continue;
            };
            self.busy.remove(i);
            match work.join().unwrap_or_else(|_| Err("it crashed".into())) {
                Ok(Note { text, link }) => self.say(&text, link.map(|url| ("open", Action::Link(url))), false),
                Err(e) => self.say(&e, None, true),
            }
        }
    }

    fn delete(&mut self, id: u64) {
        let Some(item) = self.item(id).cloned() else { return };
        let at = self.index(id);
        if self.preview.as_ref().is_some_and(|p| p.id == id) {
            self.close_preview();
        }
        self.pending.add(item, self.now);
        self.refilter();
        if self.focus == Some(id) {
            self.focus = at.and_then(|i| self.shown_item(i.min(self.shown.len().saturating_sub(1)))).map(|i| i.id);
        }
        let n = self.pending.len();
        let text = if n == 1 { "deleted".to_string() } else { format!("deleted {n}") };
        self.say(&text, Some(("undo", Action::Undo)), false);
    }

    fn undo(&mut self) {
        let Some(back) = self.pending.undo() else { return };
        self.refilter();
        self.focus = Some(back);
        self.scroll_to = Some(back);
        self.toast = None;
    }

    /// Sends the deletions to the recycle bin once their undo ran out (or all of them, now).
    fn commit(&mut self, all: bool) {
        let gone = self.pending.due(self.now, all);
        if gone.is_empty() {
            return;
        }
        // gone from the list now, not only once the folder is read again
        let ids: HashSet<u64> = gone.iter().map(|i| i.id).collect();
        let (items, locals) = (std::mem::take(&mut self.items), std::mem::take(&mut self.locals));
        (self.items, self.locals) = items.into_iter().zip(locals).filter(|(i, _)| !ids.contains(&i.id)).unzip();
        self.refilter();
        let paths: Vec<PathBuf> = gone.into_iter().map(|i| i.path).collect();
        let recycle = move || {
            for p in &paths {
                if let Err(e) = glint_shell::explorer::recycle(HWND::default(), p) {
                    log(&format!("gallery: can't delete {}: {e}", p.display()));
                }
            }
        };
        if all {
            recycle();
        } else {
            std::thread::spawn(recycle);
        }
        if matches!(self.toast, Some(Toast { action: Some((_, Action::Undo)), .. })) {
            self.toast = None;
        }
    }

    fn close_preview(&mut self) {
        if let Some(mut pv) = self.preview.take() {
            // shrinks back from wherever it got to
            pv.grow = (self.now, pv.progress(self.now), 0.0);
            pv.closing = true;
            pv.play = None;
            self.fading = Some(pv);
        }
    }

    /// Grows `id`'s tile into its preview.
    fn open_preview(&mut self, ctx: &egui::Context, id: u64, pinned: bool) {
        self.close_preview();
        let Some(tile) = self.drawn.get(&id).copied() else { return };
        let grow = (self.now, 0.0, 1.0);
        let pv = Preview { id, pinned, tile, rect: tile, full: None, full_at: 0.0, play: None, grow, closing: false };
        self.preview = Some(pv);
        let Some(item) = self.item(id).cloned() else { return };
        let meta = self.meta.get(&id).copied().unwrap_or_default();
        let target = view::preview_rect(tile, meta, self.panel, ctx.pixels_per_point());
        let ppp = ctx.pixels_per_point();
        let px = ((target.width() * ppp).round() as u32, (target.height() * ppp).round() as u32);
        // without the thumbnail's size yet, the preview's box is all there is to go by
        let px = if meta.width > 0 { (px.0.min(meta.width), px.1.min(meta.height)) } else { px };
        match item.kind {
            Kind::Shot => self.pool.full(id, item.path, px),
            Kind::Recording | Kind::Gif => {
                let size = ((px.0 & !1).max(2), (px.1 & !1).max(2));
                let gif = item.kind == Kind::Gif;
                if let Some(pv) = &mut self.preview {
                    pv.play = Some(Playback::start(item.path, gif, size, meta.duration_ms, ctx));
                }
            }
        }
    }

    /// Hover opens and closes previews; scrolling or a menu holds them off.
    fn hover(&mut self, ctx: &egui::Context, at: Option<Pos2>, over: Option<u64>) {
        let scrolling = ctx.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO);
        if self.menu.is_some() {
            return;
        }
        if let Some(pv) = &self.preview {
            let inside = at.is_some_and(|p| pv.rect.expand(6.0).contains(p) || pv.tile.contains(p));
            if !pv.pinned && (!inside || scrolling) {
                self.close_preview();
                self.hover = over.map(|id| (id, self.now));
            }
            return;
        }
        match (over, self.hover) {
            (None, _) => self.hover = None,
            (Some(id), Some((h, _))) if h != id || scrolling => self.hover = Some((id, self.now)),
            (Some(id), None) => self.hover = Some((id, self.now)),
            (Some(id), Some((_, since))) => {
                let left = HOVER - (self.now - since);
                if left <= 0.0 {
                    self.open_preview(ctx, id, false);
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64(left));
                }
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        let typing = self.search_id.is_some_and(|id| ctx.memory(|m| m.has_focus(id)));
        let none = Modifiers::NONE;
        let cmd = Modifiers::COMMAND;
        let (esc, find, undo) = ctx.input_mut(|i| {
            (i.consume_key(none, Key::Escape), i.consume_key(cmd, Key::F), !typing && i.consume_key(cmd, Key::Z))
        });
        if find && let Some(id) = self.search_id {
            ctx.memory_mut(|m| m.request_focus(id));
        }
        if undo {
            self.undo();
        }
        if esc {
            if self.menu.is_some() {
                self.menu = None;
            } else if self.preview.is_some() {
                self.close_preview();
            } else if typing {
                if self.query.is_empty() {
                    ctx.memory_mut(|m| m.surrender_focus(self.search_id.unwrap_or(Id::NULL)));
                } else {
                    self.query.clear();
                    self.refilter();
                }
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let widget_focused = ctx.memory(|m| m.focused().is_some());
        if typing {
            let down = ctx.input_mut(|i| i.consume_key(none, Key::ArrowDown) | i.consume_key(none, Key::Enter));
            if down && !self.shown.is_empty() {
                ctx.memory_mut(|m| m.surrender_focus(self.search_id.unwrap_or(Id::NULL)));
                self.focus = self.shown_item(0).map(|i| i.id);
                self.scroll_to = self.focus;
                self.keyboard = true;
            }
            return;
        }
        if widget_focused || self.shown.is_empty() {
            return;
        }
        let (step, home, end, enter, space, delete, copy) = ctx.input_mut(|i| {
            let step = [(Key::ArrowLeft, Step::Left), (Key::ArrowRight, Step::Right), (Key::ArrowUp, Step::Up), (Key::ArrowDown, Step::Down)]
                .into_iter()
                .find(|(k, _)| i.consume_key(none, *k))
                .map(|(_, s)| s);
            (
                step,
                i.consume_key(none, Key::Home),
                i.consume_key(none, Key::End),
                i.consume_key(none, Key::Enter),
                i.consume_key(none, Key::Space),
                i.consume_key(none, Key::Delete),
                i.events.iter().any(|e| matches!(e, Event::Copy)),
            )
        });
        let current = self.focus.and_then(|id| self.index(id));
        let target = match (step, current) {
            (Some(_), None) => Some(0),
            (Some(s), Some(i)) => Some(self.grid.step(i, s)),
            _ if home => Some(0),
            _ if end => Some(self.shown.len() - 1),
            _ => None,
        };
        if let Some(t) = target {
            let id = self.shown_item(t).map(|i| i.id);
            self.focus = id;
            self.scroll_to = id;
            self.keyboard = true;
            if self.preview.as_ref().is_some_and(|p| p.pinned) && let Some(id) = id {
                // a pinned preview follows the focus, once its tile is drawn there
                self.close_preview();
                self.follow = Some(id);
            }
        }
        let Some(id) = self.focus.filter(|id| self.index(*id).is_some()) else { return };
        if enter {
            self.open_item(id);
        }
        if space {
            if self.preview.as_ref().is_some_and(|p| p.id == id) {
                self.close_preview();
            } else {
                self.scroll_to = Some(id);
                self.close_preview();
                self.follow = Some(id);
            }
        }
        if delete {
            self.delete(id);
        }
        if copy {
            self.copy(id, false);
        }
    }

    /// Drops textures far out of view once there are many.
    fn evict(&mut self, visible: usize) {
        let keep = KEEP_TEXTURES.max(visible * 4);
        if self.tex.len() <= keep {
            return;
        }
        let mut by_age: Vec<(u64, u64)> = self.tex.iter().map(|(id, t)| (t.used, *id)).collect();
        by_age.sort_unstable();
        let drop = self.tex.len() - keep * 4 / 5;
        for (used, id) in by_age.into_iter().take(drop) {
            if used + 2 < self.frame {
                self.tex.remove(&id);
            }
        }
    }

    /// Asks for the thumbnails of what's in view first, then a screen above and below.
    fn want(&mut self, order: impl Iterator<Item = usize>) {
        let jobs = order
            .filter_map(|i| self.shown_item(i))
            .filter(|it| !self.tex.contains_key(&it.id) && !self.failed.contains(&it.id))
            .map(|it| Job { id: it.id, key: thumb::key(&it.path, it.size, it.modified), path: it.path.clone(), kind: it.kind })
            .collect();
        self.pool.want(jobs);
    }

    /// The gear: the app's settings window forward, or started.
    fn open_settings(&mut self) {
        if let Some(h) = window::find(self.setup.settings) {
            return window::bring_forward(h);
        }
        if let Err(e) = spawn(SETTINGS_ARG, None) {
            self.say(&format!("can't open settings: {e}"), None, true);
        }
    }
}

impl Drop for Gallery {
    fn drop(&mut self) {
        self.commit(true);
        self.saved.store(&self.saved_at);
    }
}

impl eframe::App for Gallery {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.now = ctx.input(|i| i.time);
        self.frame += 1;
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let p = widgets::palette(light, self.setup.accent);
        if self.light != Some(light) {
            self.light = Some(light);
            crate::theme::visuals(&ctx, &p, light);
        }
        ctx.input(|i| {
            if i.pointer.any_pressed() {
                self.keyboard = false;
            } else if [Key::Tab, Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown].into_iter().any(|k| i.key_pressed(k)) {
                self.keyboard = true;
            }
        });
        if let Some(r) = self.hwnd.and_then(window::placement) {
            self.saved.rect = Some(r);
        }
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if focused && !self.focused {
            self.library.refresh();
            self.host.refocus();
        }
        self.focused = focused;
        self.poll(&ctx, frame);
        if self.menu.is_none() {
            self.keys(&ctx);
        }

        let look = Look { p, keyboard: self.keyboard };
        let active = widgets::anim(ui, Id::unique("gallery-active"), focused, 0.15);
        let radius: u8 = if self.rounded { 8 } else { 0 };
        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.side);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, self.setup.app, "gallery", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            TitleAction::None => {}
        }
        let tools = Rect::from_min_size(pos2(full.left(), bar.bottom()), vec2(full.width(), TOOL_H));
        self.toolbar(ui, &look, tools);
        let panel = Rect::from_min_max(pos2(full.left(), tools.bottom()), full.max);
        let corners = egui::CornerRadius { nw: 8, ne: 8, sw: radius, se: radius };
        ui.painter().rect_filled(panel, corners, p.surf);
        self.panel = panel;
        let mut grid_ui = ui.new_child(UiBuilder::new().max_rect(panel));
        self.grid_view(&mut grid_ui, &look);
        self.draw_previews(&ctx, &look);
        self.menu_popup(&ctx, &look);
        self.show_toast(&ctx, &look);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_state_tolerates_missing_fields() {
        let s: Saved = serde_json::from_str(r#"{"rect":[1,2,3,4]}"#).unwrap();
        assert_eq!(s, Saved { rect: Some([1, 2, 3, 4]), tile: None });
        let s: Saved = serde_json::from_str(r#"{"tile":300,"old":true}"#).unwrap();
        assert_eq!(s.tile, Some(300.0));
    }

    #[test]
    fn one_undo_brings_back_every_deletion_until_the_last_ones_time_runs_out() {
        let files = ["a.png", "b.png", "c.png"].map(|n| gallery::OnDisk { path: PathBuf::from(format!(r"C:\caps\{n}")), size: 1, modified: 0 });
        let items = gallery::collect(&[], files.to_vec(), Path::new(r"C:\caps"), |_| None);
        let (a, b, c) = (items[0].clone(), items[1].clone(), items[2].clone());
        let mut d = Deletions::default();
        assert_eq!((d.due_at(), d.undo()), (None, None));
        d.add(a.clone(), 10.0);
        d.add(b.clone(), 14.0);
        assert!(d.holds(a.id) && d.holds(b.id) && !d.holds(c.id));
        // a's own six seconds are up, but the toast says "deleted 2" and undo still covers it
        assert_eq!(d.due_at(), Some(14.0 + UNDO));
        assert!(d.due(16.5, false).is_empty());
        assert_eq!(d.undo(), Some(b.id));
        assert!(d.is_empty() && d.due(100.0, false).is_empty());
        d.add(c.clone(), 30.0);
        // hovering the toast keeps it undoable
        d.hold(35.8);
        assert!(d.due(36.2, false).is_empty());
        assert_eq!(d.due(36.3, false), [c]);
        assert!(d.is_empty());
        // closing the window sends what's left at once
        d.add(a.clone(), 50.0);
        assert_eq!(d.due(50.1, true), [a]);
        assert_eq!(d.len(), 0);
    }
}
