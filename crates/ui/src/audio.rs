//! What a recording hears, as one list: each source with its icon and its name, whether it's on
//! and which tracks it feeds, its volume and the rest a click away. "add" searches desktop audio,
//! microphones and running apps; rows drag (or alt+arrow) into order; under them each track shows
//! the sources that feed it. glint's and afterglow's settings both show it.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, Context, CursorIcon, Id, Key, Layout, Modifiers, Painter, Pos2, Rect,
    Response, Role, Sense, Shape, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, UiBuilder, WidgetInfo, pos2,
    vec2,
};
use eframe::egui::accesskit::Action;
use glint_core::config::audio::{
    AudioSource, MAX_TRACKS, MAX_VOLUME, SourceKind, add_source, app_name, exe_stem, move_source, remove_source,
    set_track,
};
use glint_core::record::tracks::plan;
use glint_media::record::{App, Microphone, microphones, running_apps};
use glint_shell::icon::{Icon, exe_icon};
use glint_shell::identity::description;

use crate::widgets::{self, Kind, Look, Palette, anim, ease, glide, hint, medium, mix, mono, mono_medium, row, sans, snap};

const HEADER_H: f32 = 40.0;
const GAP: f32 = 6.0;
const ICON: f32 = 20.0;
const ICON_PX: u32 = 32;
/// Where a row's name and its details start.
const TEXT_X: f32 = 52.0;
const CELL: f32 = 24.0;
const PICK_H: f32 = 32.0;
const FOLD: f32 = 0.18;
const SETTLE: f32 = 0.22;

// ---- what's around to record ------------------------------------------------------------------

/// An app's own name and icon, once looked up.
#[derive(Default)]
struct AppLook {
    name: String,
    icon: Option<TextureHandle>,
}

struct Described {
    key: String,
    name: String,
    icon: Option<Icon>,
}

struct Devices {
    apps: Vec<App>,
    scan: Option<Receiver<Vec<App>>>,
    mics: Vec<Microphone>,
    default_mic: Option<String>,
    /// By exe, lower case; kept once seen, so an app that quit keeps its icon.
    looks: HashMap<String, AppLook>,
    described: (Sender<Described>, Receiver<Described>),
}

impl Devices {
    fn empty() -> Devices {
        Devices {
            apps: Vec::new(),
            scan: None,
            mics: Vec::new(),
            default_mic: None,
            looks: HashMap::new(),
            described: channel(),
        }
    }

    fn load(ctx: &Context) -> Devices {
        let mut d = Devices::empty();
        d.refresh(ctx);
        d
    }

    fn refresh(&mut self, ctx: &Context) {
        (self.mics, self.default_mic) = microphones();
        if self.scan.is_none() {
            let ctx = ctx.clone();
            self.scan = Some(running_apps(move || ctx.request_repaint()));
        }
    }

    fn poll(&mut self, ctx: &Context) {
        if let Some(rx) = &self.scan {
            match rx.try_recv() {
                Ok(apps) => {
                    self.describe(ctx, &apps);
                    self.apps = apps;
                    self.scan = None;
                }
                Err(TryRecvError::Disconnected) => self.scan = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        while let Ok(d) = self.described.1.try_recv() {
            let icon = d.icon.map(|i| {
                let image = ColorImage::from_rgba_unmultiplied([i.size as usize; 2], &i.rgba);
                ctx.load_texture(format!("app-icon-{}", d.key), image, TextureOptions::LINEAR)
            });
            self.looks.insert(d.key, AppLook { name: d.name, icon });
        }
    }

    /// Looks up the names and icons of apps not seen before, off the UI thread.
    fn describe(&mut self, ctx: &Context, apps: &[App]) {
        let todo: Vec<(String, String)> = (apps.iter())
            .filter_map(|a| Some((app_name(&a.exe), a.path.clone()?)))
            .filter(|(key, _)| !self.looks.contains_key(key))
            .collect();
        if todo.is_empty() {
            return;
        }
        for (key, _) in &todo {
            self.looks.insert(key.clone(), AppLook::default());
        }
        let tx = self.described.0.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("glint-app-looks".into()).spawn(move || {
            for (key, path) in todo {
                let name = description(&path).map(|d| d.trim().to_string());
                let name = name.filter(|d| !d.is_empty() && d.chars().count() <= 40).unwrap_or_default();
                let icon = exe_icon(&path, ICON_PX);
                if tx.send(Described { key, name, icon }).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        if spawned.is_err() {
            glint_core::log::log("audio: couldn't start looking up app icons");
        }
    }

    /// What an exe calls itself, or its name without ".exe".
    fn app_title(&self, exe: &str) -> String {
        match self.looks.get(&app_name(exe)) {
            Some(l) if !l.name.is_empty() => l.name.clone(),
            _ => exe_stem(exe).to_string(),
        }
    }

    fn icon(&self, exe: &str) -> Option<&TextureHandle> {
        self.looks.get(&app_name(exe))?.icon.as_ref()
    }

    fn mic(&self, device: &str) -> Option<&Microphone> {
        let id = if device.is_empty() { self.default_mic.as_deref()? } else { device };
        self.mics.iter().find(|m| m.id == id)
    }

    /// The name the recording gives a source's track.
    fn label(&self, s: &AudioSource) -> String {
        s.label(self.mic(&s.device).map(|m| m.label.as_str()))
    }

    /// The name a source's row shows, and a word about it.
    fn title(&self, s: &AudioSource) -> Title {
        let plain = |name: String| Title { name, note: None, warn: false };
        match s.kind {
            SourceKind::Desktop if app_name(&s.exclude).is_empty() => plain("desktop audio".into()),
            SourceKind::Desktop => Title { note: Some(format!("without {}", s.app_label())), ..plain("desktop audio".into()) },
            SourceKind::App if app_name(&s.app).is_empty() => Title { note: Some("no app named".into()), warn: true, ..plain("app".into()) },
            SourceKind::App => plain(s.app_label()),
            SourceKind::Mic => match (s.device.is_empty(), self.mic(&s.device)) {
                (true, m) => Title {
                    note: Some("default".into()),
                    ..plain(m.map_or_else(|| "microphone".into(), |m| m.label.clone()))
                },
                (false, Some(m)) => plain(m.label.clone()),
                (false, None) => Title { note: Some("not connected".into()), warn: true, ..plain("microphone".into()) },
            },
        }
    }
}

struct Title {
    name: String,
    note: Option<String>,
    warn: bool,
}

// ---- icons ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Glyph {
    Speaker,
    Mic,
    App(String),
}

impl Glyph {
    fn of(s: &AudioSource) -> Glyph {
        match s.kind {
            SourceKind::Desktop => Glyph::Speaker,
            SourceKind::Mic => Glyph::Mic,
            SourceKind::App => Glyph::App(s.app.clone()),
        }
    }
}

/// `text` on one line, cut short with "…" to fit `width`.
fn fitted(painter: &Painter, text: &str, font: egui::FontId, color: Color32, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping { max_width: width.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    painter.layout_job(job)
}

fn arc(c: Pos2, r: f32, from: f32, to: f32) -> Vec<Pos2> {
    (0..=12).map(|i| from + (to - from) * i as f32 / 12.0).map(|a| c + vec2(a.cos(), a.sin()) * r).collect()
}

/// The app's own icon; desktop audio and microphones get a drawn one on a tile, and so does an
/// app whose icon isn't known (its first letter).
fn paint_glyph(painter: &Painter, rect: Rect, glyph: &Glyph, devices: &Devices, p: &Palette, alpha: f32) {
    let k = rect.width() / 20.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * k;
    let ink = Stroke::new(1.3 * k.max(0.8), p.t1.gamma_multiply(alpha));
    if let Glyph::App(exe) = glyph
        && let Some(tex) = devices.icon(exe)
    {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(tex.id(), rect, uv, Color32::WHITE.gamma_multiply(alpha));
        return;
    }
    painter.rect_filled(rect, 5.0 * k, p.ctlh.gamma_multiply(alpha));
    match glyph {
        Glyph::App(exe) => {
            let letter = exe_stem(exe).chars().find(|c| c.is_alphanumeric()).map_or('?', |c| c.to_ascii_uppercase());
            painter.text(rect.center(), Align2::CENTER_CENTER, letter, medium(11.0 * k), p.t1.gamma_multiply(alpha));
        }
        Glyph::Speaker => {
            let cone = vec![at(4.0, 8.0), at(7.0, 8.0), at(11.0, 5.0), at(11.0, 15.0), at(7.0, 12.0), at(4.0, 12.0)];
            painter.add(Shape::closed_line(cone, ink));
            for r in [3.0, 6.0] {
                painter.add(Shape::line(arc(at(11.0, 10.0), r * k, -0.85, 0.85), ink));
            }
        }
        Glyph::Mic => {
            painter.rect_stroke(Rect::from_min_max(at(8.0, 3.0), at(12.0, 11.5)), 2.0 * k, ink, StrokeKind::Middle);
            painter.add(Shape::line(arc(at(10.0, 9.5), 4.5 * k, 0.0, std::f32::consts::PI), ink));
            painter.line_segment([at(10.0, 14.0), at(10.0, 16.5)], ink);
            painter.line_segment([at(7.5, 16.5), at(12.5, 16.5)], ink);
        }
    }
}

// ---- the picker -------------------------------------------------------------------------------

/// Where a picker puts what's picked: a new source, or the app a desktop source leaves out.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
enum Target {
    #[default]
    Add,
    Exclude(u64),
}

#[derive(Clone, Default)]
struct Picker {
    target: Target,
    query: String,
    /// The highlighted choice, which enter picks.
    at: usize,
    field: Option<Id>,
    focus: bool,
}

#[derive(Default)]
struct Pickers {
    open: Option<Picker>,
    /// The picker as it was last open, drawn while it folds away.
    last: Picker,
    /// Esc closed one this frame: it was still the picker's, not the window's.
    held: bool,
}

impl Pickers {
    fn open(&mut self, target: Target, devices: &mut Devices, ctx: &Context) {
        self.open = Some(Picker { target, focus: true, ..Picker::default() });
        devices.refresh(ctx);
    }

    fn is_open(&self, target: Target) -> bool {
        self.open.as_ref().is_some_and(|p| p.target == target)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Pick {
    Desktop,
    /// A device id; empty follows Windows' default microphone.
    Mic(String),
    App { exe: String, name: String },
}

#[derive(Clone, Debug)]
struct Choice {
    pick: Pick,
    name: String,
    note: String,
    glyph: Glyph,
    audio: bool,
}

/// What a picker lists for `query`: desktop audio and microphones (when adding), then running
/// apps, those playing sound first, then the query itself as an exe that isn't running. What's
/// already in the list isn't offered again.
fn choices(devices: &Devices, sources: &[AudioSource], target: Target, query: &str) -> Vec<Choice> {
    let q = query.trim().to_lowercase();
    let hit = |words: &[&str]| q.is_empty() || words.iter().any(|w| w.to_lowercase().contains(&q));
    let adding = target == Target::Add;
    let mut out = Vec::new();
    if adding {
        let has = |kind, device: Option<&str>| {
            sources.iter().any(|s| s.kind == kind && device.is_none_or(|d| s.device == d))
        };
        if !has(SourceKind::Desktop, None) && hit(&["desktop audio"]) {
            out.push(Choice {
                pick: Pick::Desktop,
                name: "desktop audio".into(),
                note: String::new(),
                glyph: Glyph::Speaker,
                audio: false,
            });
        }
        let default = devices.default_mic.as_deref();
        if let Some(m) = devices.mic("")
            && !has(SourceKind::Mic, Some(""))
            && hit(&[m.label.as_str(), m.name.as_str(), "microphone", "default"])
        {
            out.push(Choice { pick: Pick::Mic(String::new()), name: m.label.clone(), note: "default".into(), glyph: Glyph::Mic, audio: false });
        }
        for m in devices.mics.iter().filter(|m| Some(m.id.as_str()) != default) {
            if !has(SourceKind::Mic, Some(m.id.as_str())) && hit(&[m.label.as_str(), m.name.as_str(), "microphone"]) {
                out.push(Choice { pick: Pick::Mic(m.id.clone()), name: m.label.clone(), note: String::new(), glyph: Glyph::Mic, audio: false });
            }
        }
    }
    let added = |exe: &str| adding && sources.iter().any(|s| s.kind == SourceKind::App && app_name(&s.app) == app_name(exe));
    let mut apps: Vec<Choice> = (devices.apps.iter())
        .filter(|a| !added(&a.exe))
        .filter_map(|a| {
            let name = devices.app_title(&a.exe);
            if !hit(&[name.as_str(), a.exe.as_str()]) {
                return None;
            }
            let note = if name.to_lowercase() == exe_stem(&a.exe).to_lowercase() { String::new() } else { a.exe.to_lowercase() };
            let pick = Pick::App { exe: a.exe.clone(), name: name.clone() };
            // sound a windowless child plays is recorded with the app that started it
            let audio = a.audio || !a.via.is_empty();
            Some(Choice { pick, name, note, glyph: Glyph::App(a.exe.clone()), audio })
        })
        .collect();
    apps.sort_by(|a, b| b.audio.cmp(&a.audio).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out.extend(apps);
    let typed = query.trim();
    if !typed.is_empty() && !typed.contains(['\\', '/']) {
        let exe = if typed.to_lowercase().ends_with(".exe") { typed.to_string() } else { format!("{typed}.exe") };
        if !devices.apps.iter().any(|a| app_name(&a.exe) == app_name(&exe)) && !added(&exe) {
            let pick = Pick::App { exe: exe.clone(), name: String::new() };
            out.push(Choice { pick, name: exe.clone(), note: "not running".into(), glyph: Glyph::App(exe), audio: false });
        }
    }
    out
}

/// The name worth keeping for an app: only one that says more than its exe.
fn own_name(exe: &str, name: &str) -> String {
    if name.trim().is_empty() || name.trim() == exe_stem(exe) { String::new() } else { name.trim().to_string() }
}

fn source_of(pick: &Pick) -> AudioSource {
    match pick {
        Pick::Desktop => AudioSource::new(SourceKind::Desktop, 0),
        Pick::Mic(device) => AudioSource { device: device.clone(), ..AudioSource::new(SourceKind::Mic, 0) },
        Pick::App { exe, name } => {
            AudioSource { app: exe.clone(), name: own_name(exe, name), ..AudioSource::new(SourceKind::App, 0) }
        }
    }
}

/// A search field over the choices; the index of the one picked (click or enter), and whether
/// it was closed (esc or "cancel").
fn picker_ui(ui: &mut Ui, look: &Look, pk: &mut Picker, list: &[Choice], devices: &Devices, live: bool) -> (Option<usize>, bool) {
    let p = look.p;
    let focused = live && pk.field.is_some_and(|id| ui.memory(|m| m.has_focus(id)));
    let key = |k| focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, k));
    let (down, up, enter) = (key(Key::ArrowDown), key(Key::ArrowUp), key(Key::Enter));
    // egui lets go of the field's focus on esc before anything sees it
    let esc = live && ui.input(|i| i.key_pressed(Key::Escape));
    let last = list.len().saturating_sub(1);
    if down {
        pk.at = (pk.at + 1).min(last);
    }
    if up {
        pk.at = pk.at.saturating_sub(1);
    }
    pk.at = pk.at.min(last);
    let mut close = esc;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let cancel = ui.painter().layout_no_wrap("cancel".into(), medium(13.0), p.t1).size().x + 16.0;
        let w = (ui.available_width() - cancel - 8.0).max(80.0);
        let placeholder = match pk.target {
            Target::Add => "search, or type name.exe",
            Target::Exclude(_) => "which app? search or type name.exe",
        };
        let field = ui.allocate_ui(vec2(w, 32.0), |ui| widgets::text_field(ui, look, &mut pk.query, placeholder, false)).inner;
        if field.changed() {
            pk.at = 0;
        }
        pk.field = Some(field.id);
        if pk.focus && live {
            field.request_focus();
            pk.focus = false;
        }
        close |= widgets::button(ui, look, "cancel", Kind::Text, 32.0, live).clicked();
    });
    let mut picked = None;
    let rows = list.len().max(1) as f32;
    egui::ScrollArea::vertical()
        .id_salt("choices")
        .max_height(PICK_H * rows.min(7.5))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if list.is_empty() {
                let text = if devices.scan.is_some() { "looking for apps…" } else { "nothing matches" };
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), PICK_H), Sense::hover());
                ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, text, sans(13.0), p.t3);
            }
            let moving = ui.input(|i| i.pointer.is_moving());
            for (i, c) in list.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), PICK_H), Sense::click());
                if live && moving && resp.hovered() {
                    pk.at = i;
                }
                let lit = anim(ui, resp.id.with("lit"), i == pk.at, 0.06);
                let painter = ui.painter();
                painter.rect_filled(r, 5, p.hov.gamma_multiply(lit));
                let icon = Rect::from_center_size(pos2(r.left() + 8.0 + ICON / 2.0, r.center().y), vec2(ICON, ICON));
                paint_glyph(painter, icon, &c.glyph, devices, &p, 1.0);
                let clip = painter.with_clip_rect(Rect::from_x_y_ranges(r.left()..=r.right() - 28.0, r.y_range()).intersect(painter.clip_rect()));
                let name = painter.layout_no_wrap(c.name.clone(), sans(13.0), p.t1);
                let x = icon.right() + 10.0;
                let after = x + name.size().x + 8.0;
                clip.galley(pos2(x, r.center().y - name.size().y / 2.0), name, p.t1);
                if !c.note.is_empty() {
                    clip.text(pos2(after, r.center().y), Align2::LEFT_CENTER, &c.note, sans(12.0), p.t3);
                }
                if c.audio {
                    painter.text(pos2(r.right() - 10.0, r.center().y), Align2::RIGHT_CENTER, "♪", sans(13.0), p.t2);
                }
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, live, &c.name));
                if (down || up) && i == pk.at {
                    resp.scroll_to_me(None);
                }
                if live && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    picked = Some(i);
                }
            }
        });
    if enter && !list.is_empty() {
        picked = Some(pk.at);
    }
    (picked.filter(|_| live), close && live)
}

/// The picker for `target`, opening and folding where it stands.
fn picker_slot(ui: &mut Ui, look: &Look, pickers: &mut Pickers, target: Target, devices: &Devices, sources: &[AudioSource]) -> Option<Option<Pick>> {
    let on = pickers.is_open(target);
    let id = Id::unique(("audio-picker", format!("{target:?}")));
    let result = widgets::reveal(ui, id, on, |ui| {
        let pk = match &mut pickers.open {
            Some(pk) if on => pk,
            _ if pickers.last.target == target => &mut pickers.last,
            _ => return None,
        };
        ui.add_space(4.0);
        let list = choices(devices, sources, target, &pk.query);
        let (picked, close) = ui.push_id(id, |ui| picker_ui(ui, look, pk, &list, devices, on)).inner;
        match picked {
            Some(i) => Some(Some(list[i].pick.clone())),
            None if close => Some(None),
            None => None,
        }
    });
    if let Some(open) = pickers.open.as_ref().filter(|_| on) {
        pickers.last = open.clone();
    }
    result.flatten()
}

// ---- the sources' rows ------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Drag {
    key: u64,
    /// Where the pointer holds the row, from its top.
    grab: f32,
    /// Where it would land: its index among the others.
    to: usize,
}

/// Which source row is which from frame to frame, so rows keep their state while they move,
/// added ones open, removed ones fold and moved ones glide to their new place.
#[derive(Default)]
struct SourceRows {
    keys: Vec<u64>,
    next: u64,
    listed: widgets::Listed<u64, AudioSource>,
    heights: HashMap<u64, f32>,
    /// Where each row was drawn last frame, from the list's top.
    tops: HashMap<u64, f32>,
    order: Vec<u64>,
    open: HashSet<u64>,
    drag: Option<Drag>,
    dropped: Option<u64>,
    /// Where the list started last frame, and how tall it was.
    top: f32,
    total: f32,
}

impl SourceRows {
    /// Keys for `n` sources; a list that changed size some other way (reset, cancel) gets new ones.
    fn sync(&mut self, n: usize) {
        if self.keys.len() != n {
            self.keys = (self.next..self.next + n as u64).collect();
            self.next += n as u64;
            self.drag = None;
        }
    }

    fn push(&mut self) {
        self.keys.push(self.next);
        self.next += 1;
    }

    fn height(&self, key: u64) -> f32 {
        self.heights.get(&key).copied().unwrap_or(HEADER_H)
    }

    fn reorder(&mut self, sources: &mut Vec<AudioSource>, names: &mut Vec<String>, from: usize, to: usize) {
        if from == to || to >= sources.len() {
            return;
        }
        move_source(sources, names, from, to);
        let key = self.keys.remove(from);
        self.keys.insert(to, key);
    }
}

fn presence_id(key: &u64) -> Id {
    Id::unique(("audio-source-in", *key))
}

fn offset_id(key: u64) -> Id {
    Id::unique(("audio-source-offset", key))
}

#[derive(Default)]
struct RowOut {
    header: Option<Response>,
    remove: bool,
    exclude: Option<Option<Pick>>,
    open_exclude: bool,
}

/// One source: its header (grip, icon, name, volume and tracks at a glance, on/off, chevron)
/// and, opened, its volume, what's particular to its kind and "remove".
#[allow(clippy::too_many_arguments)]
fn source_row(
    ui: &mut Ui,
    look: &Look,
    s: &mut AudioSource,
    key: u64,
    open: bool,
    lift: f32,
    devices: &Devices,
    pickers: &mut Pickers,
    sources: &[AudioSource],
    live: bool,
) -> RowOut {
    let p = look.p;
    let mut out = RowOut::default();
    let left = ui.max_rect().left();
    let top = ui.cursor().top();
    let width = ui.max_rect().width();
    let shadow = ui.painter().add(Shape::Noop);
    let card = ui.painter().add(Shape::Noop);
    let head = Rect::from_min_size(pos2(left, top), vec2(width, HEADER_H));
    let sense = if live { Sense::click_and_drag() } else { Sense::hover() };
    let resp = ui.interact(head, ui.scope_id().with("head"), sense);
    let hov = anim(ui, resp.id.with("hov"), live && resp.hovered(), 0.08);
    let opened = ease(anim(ui, resp.id.with("open"), open, 0.16));
    let title = devices.title(s);
    let y = head.center().y;
    let painter = ui.painter().clone();

    for (dx, dy) in [(0.0, -4.0), (0.0, 0.0), (0.0, 4.0), (4.0, -4.0), (4.0, 0.0), (4.0, 4.0)] {
        painter.circle_filled(pos2(left + 10.0 + dx, y + dy), 1.1, mix(p.t4, p.t2, hov.max(lift)));
    }
    let dim = if s.enabled { 1.0 } else { 0.45 };
    let icon = Rect::from_center_size(pos2(left + 22.0 + ICON / 2.0, y), vec2(ICON, ICON));
    paint_glyph(&painter, icon, &Glyph::of(s), devices, &p, dim);

    // from the right: chevron, switch, then the volume and tracks it records at
    let chevron = pos2(head.right() - 14.0, y);
    let flip = 1.0 - 2.0 * opened;
    let points = vec![chevron + vec2(-4.0, -2.0 * flip), chevron + vec2(0.0, 2.0 * flip), chevron + vec2(4.0, -2.0 * flip)];
    painter.add(Shape::line(points, Stroke::new(1.5, mix(p.t3, p.t1, hov))));
    let switch = Rect::from_min_size(pos2(head.right() - 70.0, y - 10.0), vec2(46.0, 20.0));
    let mut badges = switch.left() - 10.0;
    let numbers: Vec<u32> = s.tracks.iter().copied().filter(|n| (1..=MAX_TRACKS).contains(n)).collect();
    if numbers.is_empty() {
        let r = painter.text(pos2(badges, y), Align2::RIGHT_CENTER, "no track", sans(12.0), p.warn);
        badges = r.left() - 8.0;
    }
    for &n in numbers.iter().rev() {
        let chip = Rect::from_min_max(pos2(badges - 16.0, y - 8.0), pos2(badges, y + 8.0));
        painter.rect_stroke(chip, 3, Stroke::new(1.0, p.line3.gamma_multiply(dim)), StrokeKind::Inside);
        painter.text(chip.center(), Align2::CENTER_CENTER, n.to_string(), mono(10.0), p.t2.gamma_multiply(dim));
        badges = chip.left() - 3.0;
    }
    if s.volume != 100 {
        let text = if s.volume == 0 { "muted".to_string() } else { format!("{}%", s.volume) };
        let r = painter.text(pos2(badges - 5.0, y), Align2::RIGHT_CENTER, text, mono(11.0), p.t3);
        badges = r.left() - 3.0;
    }
    let room = badges - 6.0;
    let name = fitted(&painter, &title.name, medium(13.0), mix(p.t3, p.t1, dim), room - left - TEXT_X);
    let after = left + TEXT_X + name.size().x + 8.0;
    painter.galley(pos2(left + TEXT_X, y - name.size().y / 2.0), name, p.t1);
    if let Some(note) = &title.note {
        let note = painter.layout_no_wrap(note.clone(), sans(12.0), if title.warn { p.warn } else { p.t3 });
        // a word about it, when there's room for all of it
        if after + note.size().x <= room {
            painter.galley(pos2(after, y - note.size().y / 2.0), note, p.t3);
        }
    }

    let mut toggle = ui.new_child(UiBuilder::new().id_salt("on").max_rect(switch).layout(Layout::left_to_right(Align::Center)));
    let on = widgets::toggle(&mut toggle, look, &mut s.enabled, "");
    ui.ctx().accesskit_node_builder(on.id, |n| n.set_label(format!("record {}", title.name)));

    resp.widget_info(|| WidgetInfo::labeled(Role::Button, live, &title.name));
    ui.ctx().accesskit_node_builder(resp.id, |n| n.set_expanded(open));
    if look.keyboard && resp.has_focus() {
        painter.rect_stroke(head.expand(2.0), 8.0, Stroke::new(2.0, p.focus), StrokeKind::Middle);
    }
    ui.advance_cursor_after_rect(head);

    let more = ui.scope_id().with("more");
    widgets::reveal(ui, more, open, |ui| {
        let from = ui.cursor().top();
        let inner = Rect::from_min_max(pos2(left + TEXT_X, from), pos2(head.right() - 14.0, ui.max_rect().bottom()));
        let mut d = ui.new_child(UiBuilder::new().id_salt("details").max_rect(inner).layout(Layout::top_down(Align::Min)));
        d.spacing_mut().item_spacing = vec2(8.0, 8.0);
        details(&mut d, look, s, key, devices, pickers, sources, live, &mut out);
        let bottom = d.min_rect().bottom() + 12.0;
        ui.advance_cursor_after_rect(Rect::from_min_max(pos2(left, from), pos2(head.right(), bottom)));
    });

    let rect = Rect::from_min_max(head.min, pos2(head.right(), ui.min_rect().bottom().max(head.bottom())));
    let fill = mix(mix(p.field, p.ctl, hov * 0.7), p.ctl, lift.max(opened * 0.5));
    let ring = mix(p.line2, p.line3, lift);
    painter.set(card, egui::epaint::RectShape::new(rect, 6, fill, Stroke::new(1.0, ring), StrokeKind::Inside));
    if lift > 0.0 {
        let fall = rect.translate(vec2(0.0, 4.0 * lift)).expand(2.0 * lift);
        painter.set(shadow, Shape::rect_filled(fall, 8, Color32::from_black_alpha((70.0 * lift) as u8)));
    }
    if live {
        out.header = Some(if lift > 0.0 {
            resp.on_hover_and_drag_cursor(CursorIcon::Grabbing)
        } else {
            resp.on_hover_cursor(CursorIcon::PointingHand)
        });
    }
    out
}

fn volume_word(v: u32) -> &'static str {
    match v {
        0 => "% · muted",
        1..=99 => "% · quieter",
        100 => "%",
        _ => "% · louder",
    }
}

#[allow(clippy::too_many_arguments)]
fn details(
    ui: &mut Ui,
    look: &Look,
    s: &mut AudioSource,
    key: u64,
    devices: &Devices,
    pickers: &mut Pickers,
    sources: &[AudioSource],
    live: bool,
    out: &mut RowOut,
) {
    let p = look.p;
    let word = volume_word(s.volume);
    widgets::slider(ui, look, &mut s.volume, 0..=MAX_VOLUME, word, "0", "200 %");
    match s.kind {
        SourceKind::Desktop => {
            let target = Target::Exclude(key);
            if app_name(&s.exclude).is_empty() {
                if !pickers.is_open(target) && widgets::link(ui, look, "leave out an app").clicked() {
                    out.open_exclude = true;
                }
            } else {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (icon, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                    let glyph = Glyph::App(s.exclude.clone());
                    paint_glyph(ui.painter(), Rect::from_center_size(icon.center(), vec2(16.0, 16.0)), &glyph, devices, &p, 1.0);
                    ui.label(egui::RichText::new(format!("{} is left out", s.app_label())).font(sans(13.0)).color(p.t2));
                    if widgets::link(ui, look, "put back").clicked() {
                        out.exclude = Some(None);
                    }
                });
            }
            if let Some(picked) = picker_slot(ui, look, pickers, target, devices, sources) {
                out.exclude = Some(picked);
                if out.exclude == Some(None) {
                    // closed without picking: nothing changes
                    out.exclude = None;
                    pickers.open = None;
                }
            }
        }
        SourceKind::App => {
            hint(ui, format!("{} and what it starts", app_name(&s.app)), p.t3);
        }
        SourceKind::Mic => {
            let default = match devices.mic("") {
                Some(m) => format!("default ({})", m.label),
                None => "default device".into(),
            };
            let mut items = vec![default.clone()];
            items.extend(devices.mics.iter().map(|m| m.label.clone()));
            let current = match devices.mic(&s.device) {
                _ if s.device.is_empty() => default,
                Some(m) => m.label.clone(),
                None => "not connected".into(),
            };
            let w = ui.available_width();
            match widgets::dropdown(ui, look, &current, &items, w) {
                Some(0) => s.device.clear(),
                Some(k) => s.device = devices.mics[k - 1].id.clone(),
                None => {}
            }
        }
    }
    if widgets::link(ui, look, "remove").clicked() && live {
        out.remove = true;
    }
}

struct Item {
    key: u64,
    copy: AudioSource,
    present: bool,
    t: f32,
}

/// The rows, laid out by hand so they can glide: each sits where the ones above end, weighed by
/// how far they've opened, plus an offset that eases to nothing after the order changes.
fn source_list(
    ui: &mut Ui,
    look: &Look,
    rows: &mut SourceRows,
    sources: &mut Vec<AudioSource>,
    names: &mut Vec<String>,
    devices: &mut Devices,
    pickers: &mut Pickers,
) {
    let ctx = ui.ctx().clone();
    rows.sync(sources.len());
    let current = rows.keys.iter().copied().zip(sources.iter().cloned()).collect();
    let mut items = Vec::new();
    for (key, copy, present) in rows.listed.update(&ctx, presence_id, current) {
        let t = glide(ui, presence_id(&key), if present { 1.0 } else { 0.0 }, FOLD);
        if !present && t <= 0.0 {
            rows.listed.folded(&key);
            rows.heights.remove(&key);
            rows.tops.remove(&key);
            continue;
        }
        items.push(Item { key, copy, present, t });
    }
    let weight: HashMap<u64, f32> = items.iter().map(|i| (i.key, i.t)).collect();
    let drag = rows.drag.filter(|d| rows.keys.contains(&d.key));
    let mut order: Vec<u64> = items.iter().map(|i| i.key).collect();
    if let Some(d) = drag {
        order.retain(|&k| k != d.key);
        order.insert(d.to.min(order.len()), d.key);
    }
    let mut natural = HashMap::new();
    let mut y = 0.0;
    let mut last_gap = 0.0;
    for &k in &order {
        natural.insert(k, y);
        last_gap = GAP * weight[&k];
        y += (rows.height(k) + GAP) * weight[&k];
    }
    let total = (y - last_gap).max(0.0);
    if order != rows.order {
        for &k in &order {
            if let Some(prev) = rows.tops.get(&k) {
                snap(&ctx, offset_id(k), prev - natural[&k]);
            }
        }
        rows.order = order.clone();
    }
    if let Some(k) = rows.dropped.take()
        && let (Some(prev), Some(now)) = (rows.tops.get(&k), natural.get(&k))
    {
        snap(&ctx, offset_id(k), prev - now);
    }

    let area = ui.available_rect_before_wrap();
    let top = ui.cursor().top();
    rows.top = top;
    rows.total = total;
    let pointer = ctx.pointer_interact_pos();
    // the lifted row is drawn last, over the others
    items.sort_by_key(|i| drag.is_some_and(|d| d.key == i.key));
    let mut outs = Vec::new();
    for item in &items {
        let k = item.key;
        let lifted = drag.is_some_and(|d| d.key == k);
        let lift = anim(ui, offset_id(k).with("lift"), lifted, 0.12);
        let at = match (drag, pointer) {
            (Some(d), Some(ptr)) if lifted => (ptr.y - d.grab - top).clamp(0.0, (total - rows.height(k)).max(0.0)),
            _ => natural[&k] + glide(ui, offset_id(k), 0.0, SETTLE),
        };
        let max = Rect::from_min_max(pos2(area.left(), top + at), pos2(area.right(), (top + at + 4000.0).max(area.bottom())));
        let mut child = ui.new_child(UiBuilder::new().id_salt(("source", k)).max_rect(max).layout(Layout::top_down(Align::Min)));
        if item.t < 1.0 {
            let shown = rows.height(k) * item.t;
            child.shrink_clip_rect(Rect::from_x_y_ranges(ui.clip_rect().x_range(), top + at..=top + at + shown));
            child.multiply_opacity(item.t);
        }
        let open = rows.open.contains(&k);
        let index = rows.keys.iter().position(|&x| x == k).filter(|_| item.present);
        let out = match index {
            Some(i) => {
                let others = sources.clone();
                source_row(&mut child, look, &mut sources[i], k, open, lift, devices, pickers, &others, true)
            }
            None => {
                let mut copy = item.copy.clone();
                source_row(&mut child, look, &mut copy, k, open, lift, devices, pickers, &[], false)
            }
        };
        let h = child.min_rect().height();
        if (h - rows.height(k)).abs() > 0.5 {
            rows.heights.insert(k, h);
            ctx.request_repaint();
        }
        rows.tops.insert(k, at);
        outs.push((k, out));
    }
    ui.advance_cursor_after_rect(Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), total)));

    for (k, out) in outs {
        let Some(i) = rows.keys.iter().position(|&x| x == k) else { continue };
        if let Some(pick) = out.exclude {
            let s = &mut sources[i];
            match pick {
                Some(Pick::App { exe, name }) => {
                    s.name = own_name(&exe, &name);
                    s.exclude = exe;
                }
                _ => {
                    s.exclude.clear();
                    s.name.clear();
                }
            }
            pickers.open = None;
        }
        if out.open_exclude {
            pickers.open(Target::Exclude(k), devices, &ctx);
        }
        if out.remove {
            remove_source(sources, names, i);
            rows.keys.remove(i);
            rows.open.remove(&k);
            if pickers.is_open(Target::Exclude(k)) {
                pickers.open = None;
            }
            break;
        }
        let Some(head) = out.header else { continue };
        let open = rows.open.contains(&k);
        let asked = ctx.input(|i| i.has_accesskit_action_request(head.id, if open { Action::Collapse } else { Action::Expand }));
        if head.clicked() || asked {
            if open {
                rows.open.remove(&k);
            } else {
                rows.open.insert(k);
            }
        }
        if head.has_focus() {
            let step = ctx.input_mut(|i| {
                i.consume_key(Modifiers::ALT, Key::ArrowDown) as i32 - i.consume_key(Modifiers::ALT, Key::ArrowUp) as i32
            });
            let to = i as i32 + step;
            if step != 0 && (0..sources.len() as i32).contains(&to) {
                rows.reorder(sources, names, i, to as usize);
                break;
            }
        }
        if let Some(ptr) = pointer {
            if head.drag_started() {
                rows.drag = Some(Drag { key: k, grab: ptr.y - (rows.top + rows.tops[&k]), to: i });
            }
            if let Some(d) = rows.drag.filter(|d| d.key == k)
                && head.dragged()
            {
                // it passes a row above once its top, as drawn, rises past that row's middle, and a
                // row below once its bottom sinks past it
                let h = rows.height(k);
                let at = (ptr.y - d.grab - rows.top).clamp(0.0, (rows.total - h).max(0.0));
                let (mut y, mut to, mut above) = (0.0, 0, true);
                for &other in &rows.keys {
                    let middle = y + rows.height(other) / 2.0;
                    y += rows.height(other) + GAP;
                    if other == k {
                        above = false;
                    } else if (above && at >= middle) || (!above && at + h > middle) {
                        to += 1;
                    }
                }
                rows.drag = Some(Drag { to, ..d });
            }
        }
        if head.drag_stopped()
            && let Some(d) = rows.drag.take().filter(|d| d.key == k)
        {
            rows.reorder(sources, names, i, d.to.min(sources.len() - 1));
            rows.dropped = Some(k);
            break;
        }
    }
}

// ---- tracks -----------------------------------------------------------------------------------

/// Each track the sources use: its number, its name, and a cell per source, lit when the source
/// feeds it; clicking a cell puts the source on the track or takes it off. A last, empty line
/// starts another track.
fn track_lines(
    ui: &mut Ui,
    look: &Look,
    lines: &mut widgets::Listed<u32, bool>,
    keys: &[u64],
    sources: &mut [AudioSource],
    names: &mut Vec<String>,
    devices: &Devices,
) {
    let p = look.p;
    let ctx = ui.ctx().clone();
    let live: Vec<usize> = (0..sources.len()).filter(|&i| sources[i].enabled && sources[i].records()).collect();
    let labels: Vec<String> = live.iter().map(|&i| devices.label(&sources[i])).collect();
    let refs: Vec<&AudioSource> = live.iter().map(|&i| &sources[i]).collect();
    let planned = plan(&labels, &refs, names);
    let mut numbers: Vec<u32> =
        sources.iter().flat_map(|s| s.tracks.iter().copied()).filter(|n| (1..=MAX_TRACKS).contains(n)).collect();
    numbers.sort_unstable();
    numbers.dedup();
    let last = numbers.last().copied().unwrap_or(0);
    let free = (last + 1..=MAX_TRACKS).chain(1..=last).find(|n| !numbers.contains(n));
    let mut current: Vec<(u32, bool)> = numbers.iter().map(|&n| (n, false)).collect();
    if let Some(n) = free.filter(|_| !sources.is_empty()) {
        current.push((n, true));
    }
    if names.len() < MAX_TRACKS as usize {
        names.resize(MAX_TRACKS as usize, String::new());
    }
    let line_id = |n: &u32| Id::unique(("audio-track-line", *n));
    let titles: Vec<String> = sources.iter().map(|s| devices.title(s).name).collect();
    let mut toggled = None;
    for (n, new, present) in lines.update(&ctx, line_id, current) {
        let shown = widgets::reveal(ui, line_id(&n), present, |ui| {
            let w = ui.available_width();
            let (r, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
            let painter = ui.painter();
            painter.text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, n.to_string(), mono_medium(12.0), if new { p.t4 } else { p.t2 });
            let cells = sources.len() as f32 * CELL;
            let field = Rect::from_min_max(pos2(r.left() + 20.0, r.top()), pos2((r.right() - cells - 10.0).max(r.left() + 80.0), r.bottom()));
            let track = planned.iter().find(|t| t.number == n);
            if new {
                painter.text(pos2(field.left() + 10.0, r.center().y), Align2::LEFT_CENTER, "another track", sans(13.0), p.t4);
            } else {
                let auto = match track {
                    Some(t) => t.auto.clone(),
                    None => {
                        let on: Vec<String> = (sources.iter()).filter(|s| s.tracks.contains(&n)).map(|s| devices.label(s)).collect();
                        on.join(" + ")
                    }
                };
                let mut f = ui.new_child(UiBuilder::new().id_salt(("name", n)).max_rect(field).layout(Layout::left_to_right(Align::Center)));
                widgets::text_field(&mut f, look, &mut names[n as usize - 1], &auto, false);
            }
            for (j, s) in sources.iter().enumerate() {
                let x = glide(ui, Id::unique(("audio-column", keys.get(j).copied().unwrap_or(u64::MAX - j as u64))), j as f32, SETTLE);
                let cell = Rect::from_center_size(pos2(r.right() - cells + (x + 0.5) * CELL, r.center().y), vec2(CELL - 2.0, CELL - 2.0));
                let resp = ui.interact(cell, Id::unique(("audio-cell", n, keys.get(j).copied().unwrap_or(j as u64))), Sense::click());
                let on = s.tracks.contains(&n);
                let covered = track.is_some_and(|t| live.iter().position(|&i| i == j).is_some_and(|l| t.covered.iter().any(|c| c.0 == l)));
                let h = anim(ui, resp.id.with("hov"), present && resp.hovered(), 0.08);
                let lit = anim(ui, resp.id.with("on"), on, 0.12);
                let painter = ui.painter();
                painter.rect_filled(cell, 5, mix(p.hov.gamma_multiply(h), p.ctlh, lit));
                let dim = if s.enabled { 1.0 } else { 0.5 };
                let alpha = (0.2 + 0.25 * h + 0.55 * lit) * dim * if covered { 0.6 } else { 1.0 };
                paint_glyph(painter, Rect::from_center_size(cell.center(), vec2(16.0, 16.0)), &Glyph::of(s), devices, &p, alpha);
                let what = format!("{} on track {n}", titles[j]);
                resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, present, on, &what));
                if present && resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(titles[j].as_str()).clicked() {
                    toggled = Some((j, n, !on));
                }
            }
            let note = match track {
                Some(t) => t.note(&labels),
                None if new => None,
                None if !live.iter().any(|&i| sources[i].tracks.contains(&n)) => Some("its sources are off".into()),
                None => Some("the same as an earlier track, so it's left out".into()),
            };
            if let Some(note) = note {
                ui.horizontal(|ui| {
                    ui.add_space(20.0);
                    hint(ui, note, p.t3);
                });
            }
        });
        if shown.is_none() {
            lines.folded(&n);
        }
    }
    if let Some((j, n, on)) = toggled {
        set_track(sources, names, j, n, on);
    }
}

// ---- the editor -------------------------------------------------------------------------------

/// The sources' rows with their "add" picker, and the tracks. Keeps the running apps, their
/// icons and the microphones it lists, and which row is which between frames.
#[derive(Default)]
pub struct AudioEditor {
    devices: Option<Devices>,
    rows: SourceRows,
    pickers: Pickers,
    lines: widgets::Listed<u32, bool>,
}

impl AudioEditor {
    /// A picker is open (or just closed on Esc): Esc is its, not the window's.
    pub fn busy(&self) -> bool {
        self.pickers.open.is_some() || self.pickers.held
    }

    /// Closes the picker.
    pub fn leave(&mut self) {
        self.pickers.open = None;
        self.rows.drag = None;
    }

    pub fn show(&mut self, ui: &mut Ui, look: &Look, sources: &mut Vec<AudioSource>, track_names: &mut Vec<String>) {
        let p = look.p;
        let ctx = ui.ctx().clone();
        let AudioEditor { devices, rows, pickers, lines } = self;
        let devices = devices.get_or_insert_with(|| Devices::load(&ctx));
        devices.poll(&ctx);
        let was_open = pickers.open.is_some();
        row(ui, look, "sources", (HEADER_H - 18.0) / 2.0, |ui| {
            if sources.is_empty() {
                hint(ui, "nothing is recorded yet", p.t3);
            }
            source_list(ui, look, rows, sources, track_names, devices, pickers);
            ui.add_space(if sources.is_empty() { 2.0 } else { 8.0 });
            let adding = pickers.is_open(Target::Add);
            widgets::reveal(ui, Id::unique("audio-add"), !adding, |ui| {
                if widgets::button(ui, look, "+ add", Kind::Secondary, 32.0, !adding).clicked() {
                    pickers.open(Target::Add, devices, &ctx);
                }
            });
            match picker_slot(ui, look, pickers, Target::Add, devices, sources) {
                Some(Some(pick)) => {
                    add_source(sources, track_names, source_of(&pick));
                    rows.push();
                    pickers.open = None;
                }
                Some(None) => pickers.open = None,
                None => {}
            }
        });
        pickers.held = was_open && pickers.open.is_none();
        if !sources.is_empty() {
            row(ui, look, "tracks", 7.0, |ui| track_lines(ui, look, lines, &rows.keys, sources, track_names, devices));
        }
    }
}

#[cfg(test)]
mod tests;
