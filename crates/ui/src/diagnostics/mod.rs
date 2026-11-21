//! A read-only view of both apps' current and previous logs.

mod log;

use std::path::{Path, PathBuf};
use std::process::Command;

use eframe::egui::{self, Align2, Color32, Id, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};
use glint_core::config::Config;
use glint_core::diagnostics::{Category, Event, Level};
use glint_core::format::human_duration;
use glint_core::gallery::Maker;
use glint_core::gallery::source::AMBER;
use glint_core::icon::icon_rgba;
use glint_core::paths::appdata;
use glint_shell::{clipboard, explorer};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;

use self::log::{Entry, Filter, Snapshot, Watch};
use crate::widgets::{self, BAR_H, Kind, Look, TitleAction, button, mono, sans, semibold};
use crate::{theme, window};

pub const ARG: &str = "--diagnostics";
const ROW: f32 = 34.0;
const SPLIT_WIDTH: f32 = 760.0;
const RUNS: &[(&str, &str)] = &[("current run", ""), ("previous run", "")];
const SORT: &[(&str, &str)] = &[("newest first", ""), ("oldest first", "")];
const LEVELS: [(Option<Level>, &str); 5] = [
    (None, "all levels"), (Some(Level::Error), "errors only"), (Some(Level::Warning), "warnings"),
    (Some(Level::Success), "successes"), (Some(Level::Info), "information"),
];

pub fn open(app: Maker) {
    if !window::quiet() && let Some(hwnd) = window::find(&title(app)) {
        window::bring_forward(hwnd);
        return;
    }
    if let Err(error) = std::env::current_exe().and_then(|exe| Command::new(exe).arg(ARG).spawn()) {
        glint_core::log::log(&format!("can't open diagnostics: {error}"));
    }
}

pub fn run(app: Maker) {
    let title = title(app);
    if !window::quiet() && let Some(hwnd) = window::find(&title) {
        window::bring_forward(hwnd);
        return;
    }
    let accents = [Config::load().unwrap_or_default().accent_rgb(), AMBER];
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([900.0, 550.0])
            .with_min_inner_size([530.0, 420.0])
            .with_decorations(false)
            .with_transparent(window::transparent())
            .with_active(!window::quiet())
            .with_icon(egui::IconData { rgba: icon_rgba(64, accents[app as usize]), width: 64, height: 64 }),
        centered: true,
        ..Default::default()
    };
    let _ = eframe::run_native(&title, options, Box::new(move |cc| {
        theme::fonts(&cc.egui_ctx);
        let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => Some(HWND(h.hwnd.get() as _)),
            _ => None,
        };
        if let Some(hwnd) = hwnd {
            window::frameless(hwnd, true);
        }
        Ok(Box::new(Inspector::new(hwnd, app, accents, &cc.egui_ctx)))
    }));
}

pub fn title(app: Maker) -> String {
    format!("{} diagnostics", app.name())
}

fn path(app: Maker, previous: bool) -> PathBuf {
    let suffix = if previous { ".prev.log" } else { ".log" };
    app.dir(&appdata()).join(format!("{}{suffix}", app.name()))
}

#[derive(Clone, Copy)]
enum Line {
    Day(usize),
    Event(usize),
}

struct Inspector {
    hwnd: Option<HWND>,
    started_by: Maker,
    app: Maker,
    accents: [[u8; 3]; 2],
    styled: Option<(bool, Maker)>,
    keyboard: bool,
    previous: bool,
    path: PathBuf,
    watch: Watch,
    loaded: bool,
    snapshot: Snapshot,
    filter: Filter,
    oldest_first: bool,
    rows: Vec<Line>,
    count: usize,
    selected: Option<usize>,
    context: Option<usize>,
    scroll: bool,
    feedback: Option<(String, bool)>,
}

impl Inspector {
    fn new(hwnd: Option<HWND>, app: Maker, accents: [[u8; 3]; 2], ctx: &egui::Context) -> Self {
        let path = path(app, false);
        let watch = log::watch(path.clone(), ctx.clone());
        Self {
            hwnd, started_by: app, app, accents, styled: None, keyboard: false, previous: false, path,
            watch, loaded: false, snapshot: Snapshot::default(), filter: Filter::default(),
            oldest_first: false, rows: Vec::new(), count: 0, selected: None, context: None,
            scroll: true, feedback: None,
        }
    }

    fn change_source(&mut self, app: Maker, previous: bool, ctx: &egui::Context) {
        self.app = app;
        self.previous = previous;
        self.path = path(app, previous);
        self.watch = log::watch(self.path.clone(), ctx.clone());
        self.loaded = false;
        self.snapshot = Snapshot::default();
        self.selected = None;
        self.context = None;
        self.scroll = true;
        self.feedback = None;
        self.rebuild();
    }

    fn poll(&mut self) {
        if let Some(next) = self.watch.updates.try_iter().last() {
            let match_previous = |index: usize| {
                let old = self.snapshot.entries.get(index)?;
                if next.entries.get(index).is_some_and(|row| row.raw == old.raw) {
                    Some(index)
                } else {
                    next.entries.iter().rposition(|row| row.raw == old.raw)
                }
            };
            self.selected = self.selected.and_then(match_previous);
            self.context = self.context.and_then(match_previous);
            self.snapshot = next;
            self.loaded = true;
            self.rebuild();
        }
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        self.count = 0;
        let query = self.filter.query.to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        let mut day = None;
        let entries = &self.snapshot.entries;
        let mut add = |index: usize| {
            let entry = &entries[index];
            if !self.context.map_or_else(|| self.filter.matches(entry, &words), |center| index.abs_diff(center) <= 4) {
                return;
            }
            if day != Some(entry.date.as_str()) {
                day = Some(entry.date.as_str());
                self.rows.push(Line::Day(index));
            }
            self.rows.push(Line::Event(index));
            self.count += 1;
        };
        if self.oldest_first {
            (0..entries.len()).for_each(&mut add);
        } else {
            (0..entries.len()).rev().for_each(&mut add);
        }
        if !self.rows.iter().any(|row| matches!(*row, Line::Event(i) if Some(i) == self.selected)) {
            self.selected = self.rows.iter().find_map(|row| match *row {
                Line::Event(index) => Some(index),
                Line::Day(_) => None,
            });
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, look: &Look) {
        let apps = Maker::ALL.map(|app| (app.name(), ""));
        let ctx = ui.ctx().clone();
        let sources = |this: &mut Self, ui: &mut Ui| {
            if let Some(i) = widgets::dropdown_detail(ui, look, this.app.name(), &apps, 106.0, 106.0).picked
                && Maker::ALL[i] != this.app
            {
                this.change_source(Maker::ALL[i], this.previous, &ctx);
            }
            let run = RUNS[usize::from(this.previous)].0;
            if let Some(i) = widgets::dropdown_detail(ui, look, run, RUNS, 132.0, 132.0).picked
                && (i != 0) != this.previous
            {
                this.change_source(this.app, i != 0, &ctx);
            }
            let unit = if this.count == 1 { "event" } else { "events" };
            ui.label(RichText::new(format!("{} {unit}", this.count)).color(look.p.t3));
            if this.snapshot.shortened {
                ui.label(RichText::new("recent entries only").color(look.p.t3));
            }
        };
        let controls = |this: &mut Self, ui: &mut Ui| {
            let width = (ui.available_width() - 324.0).clamp(160.0, 420.0);
            let (rect, _) = ui.allocate_exact_size(vec2(width, 32.0), Sense::hover());
            let mut search = ui.new_child(UiBuilder::new().max_rect(rect));
            let response = widgets::text_field(&mut search, look, &mut this.filter.query, "search log", false);
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
                response.request_focus();
            }
            if response.changed() {
                this.refilter();
            }
            this.filter_menu(ui, look);
            if let Some(i) = widgets::dropdown_detail(ui, look, SORT[usize::from(this.oldest_first)].0, SORT, 120.0, 120.0).picked {
                this.oldest_first = i != 0;
                this.scroll = true;
                this.rebuild();
            }
            if button(ui, look, "open raw log", Kind::Text, 32.0, this.snapshot.file_exists).clicked() {
                let result = Command::new("notepad.exe").arg(&this.path).spawn();
                this.feedback = result.err().map(|error| (format!("couldn’t open the raw log: {error}"), true));
            }
        };
        if ui.available_width() >= 814.0 && !self.snapshot.shortened {
            ui.horizontal(|ui| {
                sources(self, ui);
                ui.add_space(10.0);
                controls(self, ui);
            });
        } else {
            ui.horizontal_wrapped(|ui| sources(self, ui));
            ui.horizontal_wrapped(|ui| controls(self, ui));
        }
        if self.context.is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("surrounding events").color(look.p.t2));
                if button(ui, look, "back to results", Kind::Text, 26.0, true).clicked() {
                    self.refilter();
                }
            });
        }
    }

    fn refilter(&mut self) {
        self.context = None;
        self.scroll = true;
        self.rebuild();
    }

    fn open_afterglow_settings(&mut self) {
        if self.started_by == Maker::Afterglow {
            let result = std::env::current_exe().and_then(|exe| Command::new(exe).arg("--settings").spawn());
            self.feedback = result.err().map(|error| (format!("couldn’t open settings: {error}"), true));
        } else {
            use glint_shell::afterglow::{MAIN_CLASS, WM_OPEN_SETTINGS};
            let sent = glint_shell::window::post(MAIN_CLASS, WM_OPEN_SETTINGS, 0);
            self.feedback = (!sent).then(|| ("open Afterglow from the tray, then go to settings → video".into(), true));
        }
    }

    fn filter_menu(&mut self, ui: &mut Ui, look: &Look) {
        let count = usize::from(self.filter.level.is_some()) + usize::from(self.filter.category.is_some());
        let text = if count == 0 { "filter".to_owned() } else { format!("filter · {count}") };
        let resp = button(ui, look, &text, Kind::Secondary, 32.0, true);
        let frame = egui::Frame::NONE.fill(look.p.surf).stroke(Stroke::new(1.0, look.p.line2)).corner_radius(6).inner_margin(10);
        egui::Popup::from_toggle_button_response(&resp).frame(frame).gap(4.0)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(302.0);
                ui.label(RichText::new("level").color(look.p.t3));
                ui.horizontal_wrapped(|ui| {
                    for (value, label) in LEVELS {
                        if filter_pill(ui, look, label, self.filter.level == value) {
                            self.filter.level = value;
                            self.refilter();
                        }
                    }
                });
                ui.add_space(4.0);
                ui.label(RichText::new("type").color(look.p.t3));
                ui.horizontal_wrapped(|ui| {
                    for category in std::iter::once(None).chain(Category::ALL.map(Some)) {
                        let label = category.map_or("all types", Category::label);
                        if filter_pill(ui, look, label, self.filter.category == category) {
                            self.filter.category = category;
                            self.refilter();
                        }
                    }
                });
                if count > 0 && button(ui, look, "clear filters", Kind::Text, 26.0, true).clicked() {
                    self.filter.level = None;
                    self.filter.category = None;
                    self.refilter();
                }
            });
    }

    fn list(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let mut list = ui.new_child(UiBuilder::new().max_rect(rect));
        list.set_clip_rect(rect.intersect(ui.clip_rect()));
        if self.rows.is_empty() {
            let text = if !self.loaded {
                "reading the log…"
            } else if self.snapshot.error.is_some() {
                "couldn’t read the log"
            } else if self.snapshot.entries.is_empty() {
                if self.previous { "no previous run log yet" } else { "no events in this run yet" }
            } else {
                "no events match your filters"
            };
            list.painter().text(rect.center(), Align2::CENTER_CENTER, text, sans(14.0), look.p.t3);
            return;
        }
        let mut area = egui::ScrollArea::vertical().id_salt("diagnostics events").auto_shrink([false; 2]);
        if self.scroll {
            let row = self.rows.iter().position(|row| matches!(*row, Line::Event(i) if Some(i) == self.selected));
            let offset = row.unwrap_or_default().saturating_sub(1) as f32 * ROW;
            area = area.vertical_scroll_offset(offset);
            self.scroll = false;
        }
        list.spacing_mut().item_spacing.y = 0.0;
        area.show_rows(&mut list, ROW, self.rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &self.rows[range] {
                match *row {
                    Line::Day(i) => {
                        let date = &self.snapshot.entries[i].date;
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
                        ui.painter().text(rect.left_center() + vec2(8.0, 1.0), Align2::LEFT_CENTER,
                            if date.is_empty() { "undated" } else { date }, mono(11.0), look.p.t3);
                    }
                    Line::Event(i) => {
                        let entry = &self.snapshot.entries[i];
                        if event_row(ui, look, entry, self.selected == Some(i)).clicked() {
                            self.selected = Some(i);
                            self.feedback = None;
                        }
                    }
                }
            }
        });
    }

    fn details(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let mut panel = ui.new_child(UiBuilder::new().max_rect(rect));
        panel.set_clip_rect(rect.intersect(ui.clip_rect()));
        panel.spacing_mut().item_spacing.y = 7.0;
        egui::ScrollArea::vertical().id_salt("diagnostics details").auto_shrink([false; 2])
            .show(&mut panel, |ui| {
                let Some(index) = self.selected else {
                    if let Some(error) = &self.snapshot.error {
                        ui.label(RichText::new(error).color(look.p.warn));
                    } else {
                        ui.label(RichText::new("select an event to see its details").color(look.p.t3));
                    }
                    return;
                };
                let entry = &self.snapshot.entries[index];
                let detail = Detail::of(entry);
                ui.label(RichText::new(entry.category.label()).color(look.p.t3));
                ui.label(RichText::new(detail.title).font(semibold(17.0)).color(look.p.t1));
                if let Some(text) = detail.body {
                    ui.label(RichText::new(text).color(look.p.t2));
                }
                if let Some(text) = detail.advice {
                    ui.add_space(3.0);
                    ui.label(RichText::new("what to check").font(semibold(13.0)).color(look.p.t1));
                    ui.label(RichText::new(text).color(look.p.t2));
                }
                if let Some(path) = detail.file {
                    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
                    ui.add(egui::Label::new(RichText::new(name).font(mono(12.0)).color(look.p.t3)).wrap())
                        .on_hover_text(path.to_string_lossy());
                }
                ui.add_space(4.0);
                let mut context = false;
                let mut settings = false;
                ui.horizontal_wrapped(|ui| {
                    if detail.settings && button(ui, look, "open settings", Kind::Secondary, 28.0, true).clicked() {
                        settings = true;
                    }
                    if let Some(file) = detail.file
                        && button(ui, look, "show in folder", Kind::Secondary, 28.0, true).clicked()
                    {
                        if file.is_file() {
                            explorer::reveal(file);
                        } else {
                            self.feedback = Some(("the clip has been moved or deleted".into(), true));
                        }
                    }
                    if let Some(folder) = detail.folder
                        && button(ui, look, "open output folder", Kind::Secondary, 28.0, true).clicked()
                    {
                        self.feedback = if folder.is_dir() {
                            Command::new("explorer.exe").arg(folder).spawn().err()
                                .map(|error| (format!("couldn’t open the output folder: {error}"), true))
                        } else {
                            Some(("the output folder isn’t available".into(), true))
                        };
                    }
                    if button(ui, look, "copy details", Kind::Text, 28.0, true).clicked() {
                        self.feedback = Some(match clipboard::copy_text(self.hwnd, &entry.raw) {
                            Ok(()) => ("copied event details".into(), false),
                            Err(error) => (format!("couldn’t copy details: {error}"), true),
                        });
                    }
                });
                if entry.level == Level::Error && self.context.is_none() {
                    context = widgets::link(ui, look, "show surrounding events").clicked();
                }
                if let Some((text, error)) = &self.feedback {
                    ui.label(RichText::new(text).color(if *error { look.p.warn } else { look.p.t3 }));
                }
                ui.add_space(6.0);
                ui.collapsing("raw event", |ui| {
                    ui.add(egui::Label::new(RichText::new(&entry.raw).font(mono(12.0)).color(look.p.t2)).selectable(true).wrap());
                });
                if settings {
                    self.open_afterglow_settings();
                }
                if context {
                    self.context = Some(index);
                    self.scroll = true;
                    self.rebuild();
                }
            });
    }
}

impl eframe::App for Inspector {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        window::closing();
    }

    fn ui(&mut self, ui: &mut Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        window::shots(&ctx);
        self.poll();
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let p = widgets::palette(light, self.accents[self.app as usize]);
        if self.styled != Some((light, self.app)) {
            self.styled = Some((light, self.app));
            theme::visuals(&ctx, &p, light);
        }
        ctx.input(|input| {
            if input.pointer.any_pressed() {
                self.keyboard = false;
            } else if input.key_pressed(egui::Key::Tab) {
                self.keyboard = true;
            }
        });
        let look = Look { p, keyboard: self.keyboard };
        let radius = window::radius(&ctx);
        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.side);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        let focused = ctx.input(|input| input.viewport().focused).unwrap_or(true);
        let active = widgets::anim(ui, Id::unique("diagnostics-active"), focused, 0.15);
        match widgets::title_bar(ui, &look, bar, self.started_by.name(), "diagnostics", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            TitleAction::None => {}
        }
        let tools = Rect::from_min_max(pos2(full.left() + 14.0, bar.bottom() + 7.0), pos2(full.right() - 14.0, full.bottom()));
        let mut toolbar = ui.new_child(UiBuilder::new().max_rect(tools));
        self.toolbar(&mut toolbar, &look);
        let top = toolbar.min_rect().bottom() + 10.0;
        let body = Rect::from_min_max(pos2(full.left(), top), full.max);
        ui.painter().rect_filled(body, egui::CornerRadius { nw: 0, ne: 0, sw: radius, se: radius }, p.surf);
        ui.painter().hline(body.x_range(), top, Stroke::new(1.0, p.line));
        let content = body.shrink(12.0);
        if full.width() >= SPLIT_WIDTH {
            let split = body.left() + (body.width() * 0.56).round();
            ui.painter().vline(split, body.y_range(), Stroke::new(1.0, p.line));
            self.list(ui, &look, Rect::from_min_max(content.min, pos2(split - 8.0, content.bottom())));
            self.details(ui, &look, Rect::from_min_max(pos2(split + 16.0, content.top() + 2.0), content.max));
        } else {
            let split = content.top() + (content.height() * 0.47).round();
            ui.painter().hline(body.x_range(), split, Stroke::new(1.0, p.line));
            self.list(ui, &look, Rect::from_min_max(content.min, pos2(content.right(), split - 6.0)));
            self.details(ui, &look, Rect::from_min_max(pos2(content.left() + 6.0, split + 12.0), content.max));
        }
        window::border(ui, full, p.line2);
    }
}

fn filter_pill(ui: &mut Ui, look: &Look, label: &str, selected: bool) -> bool {
    let response = button(ui, look, label, Kind::Secondary, 26.0, true);
    if selected {
        ui.painter().rect_stroke(response.rect, 5, Stroke::new(1.0, look.p.acc), StrokeKind::Inside);
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, label));

    response.clicked()
}

fn event_row(ui: &mut Ui, look: &Look, entry: &Entry, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = look.p;
    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    let border = if selected { p.acc } else { Color32::TRANSPARENT };
    let fill = if selected { p.sel } else if response.hovered() { p.hov } else { Color32::TRANSPARENT };
    painter.rect(rect.shrink2(vec2(1.0, 2.0)), 5, fill, Stroke::new(1.0, border), StrokeKind::Inside);
    let y = rect.center().y;
    let x = rect.left() + 8.0;
    let light = ui.ctx().system_theme() == Some(egui::Theme::Light);
    let status = match entry.level {
        Level::Info => p.t3,
        Level::Success => Color32::from_rgb(if light { 32 } else { 132 }, if light { 117 } else { 196 }, if light { 73 } else { 149 }),
        Level::Warning => p.warn,
        Level::Error => if light { Color32::from_rgb(0xB4, 0x25, 0x32) } else { Color32::from_rgb(0xFF, 0x87, 0x8B) },
    };
    painter.text(pos2(x, y), Align2::LEFT_CENTER, &entry.time, mono(12.0), p.t3);
    painter.text(pos2(x + 76.0, y), Align2::LEFT_CENTER, entry.status(), sans(12.0), status);
    let at = x + 173.0;
    let mut job = egui::text::LayoutJob::simple_singleline(entry.summary.clone(), sans(13.0), p.t1);
    job.wrap = egui::text::TextWrapping::truncate_at_width((rect.right() - at - 10.0).max(1.0));
    let galley = painter.layout_job(job);
    painter.galley(pos2(at, y - galley.size().y / 2.0), galley, p.t1);
    widgets::focus_ring(ui, look, &response, rect, 5.0);
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &entry.summary));

    response
}

struct Detail<'a> {
    title: &'a str,
    body: Option<String>,
    advice: Option<&'static str>,
    file: Option<&'a Path>,
    folder: Option<&'a Path>,
    settings: bool,
}

impl<'a> Detail<'a> {
    fn of(entry: &'a Entry) -> Self {
        let mut detail = Self { title: &entry.summary, body: None, advice: None, file: None, folder: None, settings: false };
        match &entry.event {
            Some(Event::ReplaySaved { duration_ms, elapsed_ms, path, .. }) => {
                detail.title = "Replay saved";
                detail.body = Some(format!("{} saved in {:.1} s", human_duration(f64::from(*duration_ms) / 1000.0), *elapsed_ms as f64 / 1000.0));
                detail.file = Some(path);
            }
            Some(Event::ReplayShortened { duration_ms, requested_s, path, .. }) => {
                detail.title = "Replay saved, but shorter";
                detail.body = Some(format!("Saved {} of the requested {}. The replay buffer hit its memory cap.",
                    human_duration(f64::from(*duration_ms) / 1000.0), human_duration(f64::from(*requested_s))));
                detail.advice = Some("In settings → video, raise the memory cap or lower video quality to keep more time.");
                detail.file = Some(path);
                detail.settings = true;
            }
            Some(Event::ReplayTimedOut { folder, .. }) => {
                detail.title = "Replay save timed out";
                detail.body = Some("No finished clip was reported before the timeout. You can save again.".into());
                detail.advice = Some("If there’s an .mp4.part in the output folder, keep it. It may contain recoverable footage.");
                detail.folder = Some(folder);
            }
            Some(Event::ReplayFailed { folder, .. }) => {
                detail.title = "Replay couldn’t be saved";
                detail.body = Some("Afterglow reported a write error. Open the raw event below for the exact error.".into());
                detail.advice = Some("Check that the output drive is connected, has free space, and the folder is writable.");
                detail.folder = Some(folder);
            }
            None => {}
        }

        detail
    }
}
