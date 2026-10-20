use std::collections::VecDeque;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, UiBuilder, pos2, vec2,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;

use crate::app::tray::icon_pixels;
use crate::config::Config;
use crate::history::{self, Kind as HistoryKind};
use crate::platform::format::human_size;
use crate::platform::log::log;
use crate::record::Codec;
use crate::settings::widgets::{
    self, BAR_H, FOOTER_H, Kind, Look, TitleAction, hint, medium, mono, mono_medium, sans, semibold,
};

mod gif;
mod math;
mod media;
mod player;
mod trim;

use gif::GifOptions;
use math::{GIF_FPS, GIF_WARN_BYTES, GIF_WIDTHS, Range, fmt_time, frame_len, snap};
use media::{Com, Info};
use player::{Cmd, Player, Shown};
use trim::{Progress, Quality};

pub const CLIP_ARG: &str = "--clip";
const TITLE: &str = "glint clip";
const WINDOW: [f32; 2] = [960.0, 660.0];
const PAD: f32 = 24.0;
const PREVIEW_TOP: f32 = BAR_H + 44.0;
const PREVIEW_H: f32 = 420.0;
const TRACK_H: f32 = 36.0;

pub(crate) fn spawn(path: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).arg(CLIP_ARG).arg(path).spawn().map(|_| ())
}

pub fn run(path: PathBuf) {
    let probe = path.clone();
    let info = std::thread::spawn(move || {
        let _com = Com::init();
        media::probe(&probe)
    })
    .join()
    .unwrap_or_else(|_| Err("the video couldn't be read".into()));
    if let Err(e) = &info {
        log(&format!("clip: {}: {e}", path.display()));
    }
    let cfg = Config::load().unwrap_or_default();
    let size = 64;
    let rgba = icon_pixels(size, true)
        .into_iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8])
        .collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(WINDOW)
            .with_resizable(false)
            .with_decorations(false)
            .with_icon(egui::IconData { rgba, width: size as u32, height: size as u32 }),
        centered: true,
        ..Default::default()
    };
    let range = Range::full(info.as_ref().map_or(0, |i| i.duration));
    let gif = GifOptions { fps: 15, width: info.as_ref().ok().and_then(|i| (i.size.0 > 960).then_some(960)) };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let _ = eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            crate::settings::fonts(&cc.egui_ctx);
            let rounded = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => crate::settings::round(HWND(h.hwnd.get() as _)),
                _ => false,
            };
            Ok(Box::new(ClipApp {
                path,
                name,
                info,
                quality: Quality { qp: cfg.quality, effort: cfg.effort },
                player: None,
                player_error: None,
                tex: None,
                epoch: 0,
                queue: VecDeque::new(),
                shown: None,
                pos: 0,
                play: Play::Paused,
                range,
                drag: None,
                job: None,
                status: None,
                gif_dialog: false,
                gif,
                keyboard: false,
                rounded,
                light: None,
            }))
        }),
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Play {
    Paused,
    Starting,
    Running { at: Instant, from: i64 },
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Start,
    End,
    Head,
}

#[derive(Clone, Copy, PartialEq)]
enum JobKind {
    Trim,
    Gif,
}

struct Job {
    kind: JobKind,
    out: PathBuf,
    permille: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<bool, String>>>,
}

enum Status {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
}

struct ClipApp {
    path: PathBuf,
    name: String,
    info: Result<Info, String>,
    quality: Quality,
    player: Option<Player>,
    player_error: Option<String>,
    tex: Option<TextureHandle>,
    epoch: u64,
    queue: VecDeque<Shown>,
    shown: Option<(i64, i64)>,
    pos: i64,
    play: Play,
    range: Range,
    drag: Option<Drag>,
    job: Option<Job>,
    status: Option<Status>,
    gif_dialog: bool,
    gif: GifOptions,
    keyboard: bool,
    rounded: bool,
    light: Option<bool>,
}

fn fit(src: (u32, u32), area: egui::Vec2) -> egui::Vec2 {
    let (w, h) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let s = (area.x / w).min(area.y / h);
    vec2(w * s, h * s)
}

impl ClipApp {
    fn info(&self) -> Option<&Info> {
        self.info.as_ref().ok()
    }

    fn frame_len(&self) -> i64 {
        frame_len(self.info().map_or(30.0, |i| i.fps))
    }

    fn seek(&mut self, t: i64, play: bool) {
        self.epoch += 1;
        self.queue.clear();
        self.play = if play { Play::Starting } else { Play::Paused };
        if let Some(p) = &self.player {
            p.send(Cmd::Seek { t, epoch: self.epoch });
            p.send(Cmd::Play(play));
        }
    }

    fn pause(&mut self) {
        if self.play != Play::Paused {
            self.play = Play::Paused;
            self.epoch += 1;
            self.queue.clear();
            if let Some(p) = &self.player {
                p.send(Cmd::Play(false));
            }
        }
    }

    fn toggle_play(&mut self) {
        if self.play != Play::Paused {
            self.pause();
            return;
        }
        if self.pos < self.range.start || self.pos >= self.range.end - self.frame_len() {
            self.pos = self.range.start;
        }
        self.seek(self.pos, true);
    }

    fn step(&mut self, forward: bool) {
        let Some((ts, dur)) = self.shown else { return };
        let duration = self.info().map_or(0, |i| i.duration);
        let t = if forward { ts + if dur > 0 { dur } else { self.frame_len() } } else { ts - 1 };
        if t < 0 || t >= duration {
            return;
        }
        self.pos = t;
        self.seek(t, false);
    }

    fn start_player(&mut self, ctx: &egui::Context, area: egui::Vec2) {
        let Some(info) = self.info() else { return };
        let px = area * ctx.pixels_per_point();
        let scaled = fit(info.size, px);
        let size = if scaled.x >= info.size.0 as f32 {
            info.size
        } else {
            ((scaled.x.round() as u32).max(2), (scaled.y.round() as u32).max(2))
        };
        self.player = Some(Player::start(self.path.clone(), size, ctx.clone()));
        self.seek(0, false);
    }

    fn poll_player(&mut self, ctx: &egui::Context) {
        let Some(p) = &self.player else { return };
        let mut fresh = Vec::new();
        while let Some(got) = p.poll() {
            match got {
                Ok(s) if s.epoch == self.epoch => fresh.push(s),
                Ok(_) => {}
                Err(e) => self.player_error = Some(e),
            }
        }
        for s in fresh {
            if self.play == Play::Starting {
                self.play = Play::Running { at: Instant::now(), from: s.ts.max(self.pos) };
            }
            self.queue.push_back(s);
        }
        let (now, ended) = match self.play {
            Play::Running { at, from } => {
                let t = from + (at.elapsed().as_secs_f64() * math::SECOND as f64) as i64;
                (t.min(self.range.end), t >= self.range.end)
            }
            Play::Starting => (i64::MIN, false),
            Play::Paused => (i64::MAX, false),
        };
        let mut latest = None;
        while self.queue.front().is_some_and(|s| s.ts <= now) {
            latest = self.queue.pop_front();
        }
        if let Some(s) = latest {
            self.shown = Some((s.ts, s.dur));
            if self.play == Play::Paused && self.drag.is_none() {
                self.pos = s.ts;
            }
            match &mut self.tex {
                Some(t) => t.set(s.image, TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("clip-frame", s.image, TextureOptions::LINEAR)),
            }
        }
        if let Play::Running { .. } = self.play {
            self.pos = now;
            if ended {
                self.pause();
            } else {
                ctx.request_repaint();
            }
        }
    }

    fn export(&mut self, kind: JobKind) {
        let Some(info) = self.info().cloned() else { return };
        let out = match kind {
            JobKind::Trim => math::unique_sibling(&self.path, "_trim", "mp4"),
            JobKind::Gif => math::unique_sibling(&self.path, "", "gif"),
        };
        let permille = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (src, dst, range, quality, gif) = (self.path.clone(), out.clone(), self.range, self.quality, self.gif);
        let (pm, c) = (permille.clone(), cancel.clone());
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            let p = Progress { permille: &pm, cancel: &c };
            match kind {
                JobKind::Trim => trim::trim(&src, &info, range, &dst, quality, &p),
                JobKind::Gif => gif::export(&src, &info, range, gif, &dst, &p),
            }
        });
        self.status = None;
        self.job = Some(Job { kind, out, permille, cancel, thread: Some(thread) });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.job else { return };
        let Some(thread) = job.thread.take_if(|t| t.is_finished()) else {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        };
        let res = thread.join().unwrap_or_else(|_| Err("the export crashed".into()));
        let Some(job) = self.job.take() else { return };
        self.status = Some(match res {
            Ok(true) => {
                let size = std::fs::metadata(&job.out).map(|m| m.len()).unwrap_or(0);
                history::add(HistoryKind::Recording, Some(&job.out), size, None);
                Status::Saved(job.out)
            }
            Err(e) if !job.cancel.load(Ordering::Relaxed) => {
                log(&format!("clip: exporting {} failed: {e}", self.path.display()));
                Status::Failed(e)
            }
            _ => Status::Cancelled,
        });
    }
}

impl Drop for ClipApp {
    fn drop(&mut self) {
        if let Some(job) = &mut self.job {
            job.cancel.store(true, Ordering::Relaxed);
            if let Some(t) = job.thread.take() {
                let _ = t.join();
            }
        }
    }
}

const LETTERBOX: Color32 = Color32::from_rgb(0x10, 0x10, 0x0F);

fn show_in_folder(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

fn describe(info: &Info) -> String {
    let codec = match info.codec {
        Some(Codec::H264) => "h.264",
        Some(Codec::Hevc) => "hevc",
        None => "video",
    };
    let fps = if (info.fps - info.fps.round()).abs() < 0.01 {
        format!("{:.0}", info.fps)
    } else {
        format!("{:.2}", info.fps)
    };
    let audio = if info.audio { "audio" } else { "no audio" };
    format!("{}×{} · {fps} fps · {codec} · {audio} · {}", info.size.0, info.size.1, fmt_time(info.duration))
}

impl eframe::App for ClipApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let p = if light { widgets::LIGHT } else { widgets::DARK };
        if self.light != Some(light) {
            self.light = Some(light);
            crate::settings::visuals(&ctx, &p, light);
        }
        ctx.input(|i| {
            if i.pointer.any_pressed() {
                self.keyboard = false;
            } else if i.key_pressed(Key::Tab) {
                self.keyboard = true;
            }
        });
        let look = Look { p, keyboard: self.keyboard };
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let active = widgets::anim(ui, Id::unique("clip-active"), focused, 0.15);
        let radius: u8 = if self.rounded { 8 } else { 0 };
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let had_dialog = self.gif_dialog;

        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.surf);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, "clip", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            TitleAction::None => {}
        }

        let preview = Rect::from_min_size(
            pos2(full.left() + PAD, full.top() + PREVIEW_TOP),
            vec2(full.width() - 2.0 * PAD, PREVIEW_H),
        );
        if self.player.is_none() && self.info.is_ok() {
            self.start_player(&ctx, preview.size());
        }
        self.poll_player(&ctx);
        self.poll_job(&ctx);
        if !self.gif_dialog && self.info.is_ok() {
            let (space, left, right) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Space),
                    i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                    i.consume_key(Modifiers::NONE, Key::ArrowRight),
                )
            });
            if space {
                self.toggle_play();
            }
            if left || right {
                self.step(right);
            }
        }

        let header_y = bar.bottom() + 22.0;
        let painter = ui.painter();
        let name = painter.text(pos2(preview.left(), header_y), Align2::LEFT_CENTER, &self.name, semibold(15.0), p.t1);
        if let Some(info) = self.info() {
            let text = describe(info);
            let galley = painter.layout_no_wrap(text, mono(11.0), p.t3);
            let x = (preview.right() - galley.size().x).max(name.right() + 16.0);
            painter.galley(pos2(x, header_y - galley.size().y / 2.0), galley, p.t3);
        }

        self.preview(ui, &look, preview);
        let transport = Rect::from_min_size(pos2(preview.left(), preview.bottom() + 12.0), vec2(preview.width(), 32.0));
        self.transport(ui, &look, transport);
        let track =
            Rect::from_min_size(pos2(preview.left(), transport.bottom() + 12.0), vec2(preview.width(), TRACK_H));
        self.timeline(ui, &look, track);
        let footer = Rect::from_min_max(pos2(full.left(), full.bottom() - FOOTER_H), full.right_bottom());
        self.footer(ui, &look, footer);
        self.dialog(&ctx, &look);

        if esc && !had_dialog && self.job.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl ClipApp {
    fn preview(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let resp = ui.interact(rect, Id::unique("clip-preview"), Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, 6, LETTERBOX);
        let problem = self.info.as_ref().err().or(self.player_error.as_ref());
        if let Some(e) = problem {
            let text = format!("can't play {}: {e}", self.name);
            let galley = painter.layout(text, sans(13.0), look.p.warn, rect.width() - 80.0);
            painter.galley(rect.center() - galley.size() / 2.0, galley, look.p.warn);
            return;
        }
        if let Some(tex) = &self.tex {
            let [w, h] = tex.size();
            let r = Rect::from_center_size(rect.center(), fit((w as u32, h as u32), rect.size()));
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            painter.image(tex.id(), r, uv, Color32::WHITE);
        }
        if resp.clicked() {
            self.toggle_play();
        }
    }

    fn transport(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let enabled = self.info.is_ok() && self.player_error.is_none();
        let mut ui = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
        if widgets::button(&mut ui, look, "←", Kind::Secondary, 30.0, enabled)
            .on_hover_text("previous frame (←)")
            .clicked()
        {
            self.step(false);
        }
        let label = if self.play == Play::Paused { "play" } else { "pause" };
        if widgets::button(&mut ui, look, label, Kind::Secondary, 30.0, enabled).on_hover_text("space").clicked() {
            self.toggle_play();
        }
        if widgets::button(&mut ui, look, "→", Kind::Secondary, 30.0, enabled).on_hover_text("next frame (→)").clicked()
        {
            self.step(true);
        }
        ui.add_space(10.0);
        let duration = self.info().map_or(0, |i| i.duration);
        ui.label(RichText::new(fmt_time(self.pos)).font(mono_medium(12.0)).color(p.t1));
        ui.label(RichText::new(format!("/ {}", fmt_time(duration))).font(mono(12.0)).color(p.t3));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = self.range;
            ui.label(
                RichText::new(format!("{} – {}", fmt_time(r.start), fmt_time(r.end))).font(mono(12.0)).color(p.t2),
            );
            ui.label(RichText::new(format!("selection {}", fmt_time(r.len()))).font(sans(12.0)).color(p.t3));
        });
    }

    fn timeline(&mut self, ui: &mut Ui, look: &Look, track: Rect) {
        let p = look.p;
        let Some(info) = self.info().cloned() else {
            ui.painter().rect(track, 6, p.field, Stroke::new(1.0, p.line2), StrokeKind::Inside);
            return;
        };
        let dur = info.duration.max(1);
        let frame = frame_len(info.fps);
        let inner = track.shrink2(vec2(6.0, 0.0));
        let x_of = |t: i64| inner.left() + inner.width() * (t.clamp(0, dur) as f32 / dur as f32);
        let t_of = |x: f32| (((x - inner.left()) / inner.width()).clamp(0.0, 1.0) as f64 * dur as f64) as i64;
        let handle = |t: i64| Rect::from_center_size(pos2(x_of(t), track.center().y), vec2(12.0, TRACK_H + 4.0));

        let bar = ui.interact(track, Id::unique("clip-track"), Sense::click_and_drag());
        let (hs, he) = (handle(self.range.start), handle(self.range.end));
        let start = ui.interact(hs, Id::unique("clip-in"), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal);
        let end = ui.interact(he, Id::unique("clip-out"), Sense::drag()).on_hover_cursor(CursorIcon::ResizeHorizontal);
        if start.drag_started() {
            self.drag = Some(Drag::Start);
        } else if end.drag_started() {
            self.drag = Some(Drag::End);
        } else if bar.drag_started() || bar.clicked() {
            self.drag = Some(Drag::Head);
        }
        let pointer = ui.input(|i| i.pointer.interact_pos());
        if let (Some(drag), Some(at)) = (self.drag, pointer) {
            let t = t_of(at.x);
            match drag {
                Drag::Start => {
                    let before = self.range.start;
                    self.range.set_start(snap(t, info.fps), frame);
                    if self.range.start != before || self.pos != self.range.start {
                        self.pos = self.range.start;
                        self.seek(self.range.start, false);
                    }
                }
                Drag::End => {
                    let before = self.range.end;
                    self.range.set_end(snap(t, info.fps), info.duration, frame);
                    if self.range.end != before || self.pos != self.range.end - 1 {
                        self.pos = self.range.end - 1;
                        self.seek(self.pos, false);
                    }
                }
                Drag::Head => {
                    let t = t.min(info.duration - 1);
                    if t != self.pos {
                        self.pos = t;
                        self.seek(t, false);
                    }
                }
            }
        }
        if self.drag.is_some() && !ui.input(|i| i.pointer.any_down()) {
            self.drag = None;
        }

        let painter = ui.painter();
        painter.rect(track, 6, p.field, Stroke::new(1.0, p.line2), StrokeKind::Inside);
        let (x0, x1) = (x_of(self.range.start), x_of(self.range.end));
        let shade = Color32::from_black_alpha(if self.light == Some(true) { 28 } else { 70 });
        let left = Rect::from_min_max(track.min, pos2(x0, track.bottom()));
        let right = Rect::from_min_max(pos2(x1, track.top()), track.max);
        painter.rect_filled(left, CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, shade);
        painter.rect_filled(right, CornerRadius { nw: 0, sw: 0, ne: 6, se: 6 }, shade);
        let sel = Rect::from_min_max(pos2(x0, track.top()), pos2(x1, track.bottom()));
        painter.rect_filled(sel, 0, p.acc.gamma_multiply(0.14));
        painter.hline(x0..=x1, track.top() + 1.0, Stroke::new(2.0, p.acc));
        painter.hline(x0..=x1, track.bottom() - 1.0, Stroke::new(2.0, p.acc));
        for (r, resp) in [(hs, &start), (he, &end)] {
            let h = widgets::anim(ui, resp.id.with("hov"), resp.hovered() || resp.dragged(), 0.08);
            let knob = r.shrink2(vec2(2.0 - h, 0.0));
            painter.rect_filled(knob, 3, if h > 0.0 { p.acch } else { p.acc });
            let c = knob.center();
            painter.vline(c.x, c.y - 6.0..=c.y + 6.0, Stroke::new(1.5, Color32::WHITE.gamma_multiply(0.85)));
        }
        let x = x_of(self.pos);
        painter.vline(x, track.top() - 4.0..=track.bottom() + 4.0, Stroke::new(2.0, p.t1));
        painter.circle_filled(pos2(x, track.top() - 4.0), 4.0, p.t1);
    }

    fn footer(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, p.line));
        let inner =
            Rect::from_min_max(pos2(rect.left() + PAD, rect.top() + 1.0), pos2(rect.right() - PAD, rect.bottom()));
        let mut ui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let ready = self.info.is_ok() && self.job.is_none();
        let trimmed = self.info().is_some_and(|i| !self.range.is_full(i.duration));
        let mut open_gif = false;
        let mut save = false;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            save = widgets::button(ui, look, "save trimmed", Kind::Primary, 30.0, ready && trimmed).clicked();
            open_gif = widgets::button(ui, look, "export gif…", Kind::Secondary, 30.0, ready).clicked();
            ui.add_space(8.0);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.status_line(ui, look));
        });
        if save {
            self.export(JobKind::Trim);
        }
        if open_gif {
            self.pause();
            self.gif_dialog = true;
        }
    }

    fn status_line(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        if let Some(job) = &self.job {
            let what = match job.kind {
                JobKind::Trim => "saving the trimmed clip",
                JobKind::Gif => "exporting the gif",
            };
            ui.label(RichText::new(what).font(sans(13.0)).color(p.t2));
            let permille = job.permille.load(Ordering::Relaxed);
            let (bar, _) = ui.allocate_exact_size(vec2(160.0, 4.0), Sense::hover());
            ui.painter().rect_filled(bar, 2, p.field);
            let done = Rect::from_min_size(bar.min, vec2(bar.width() * permille as f32 / 1000.0, bar.height()));
            ui.painter().rect_filled(done, 2, p.acc);
            ui.label(RichText::new(format!("{}%", permille / 10)).font(mono(12.0)).color(p.t3));
            if widgets::button(ui, look, "cancel", Kind::Text, 30.0, !job.cancel.load(Ordering::Relaxed)).clicked() {
                job.cancel.store(true, Ordering::Relaxed);
            }
            return;
        }
        match &self.status {
            Some(Status::Saved(path)) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let size = std::fs::metadata(path).map(|m| human_size(m.len())).unwrap_or_default();
                ui.add(
                    egui::Label::new(RichText::new(format!("saved {name} · {size}")).font(sans(13.0)).color(p.t2))
                        .truncate(),
                );
                if widgets::link(ui, look, "show in folder").clicked() {
                    show_in_folder(path);
                }
            }
            Some(Status::Cancelled) => {
                ui.label(RichText::new("cancelled, nothing was saved").font(sans(13.0)).color(p.t3));
            }
            Some(Status::Failed(e)) => {
                ui.add(
                    egui::Label::new(RichText::new(format!("couldn't export: {e}")).font(sans(13.0)).color(p.warn))
                        .truncate(),
                );
            }
            None => {
                ui.label(
                    RichText::new("space plays · ← → step frames · drag the red handles to trim")
                        .font(sans(12.0))
                        .color(p.t3),
                );
            }
        }
    }

    fn dialog(&mut self, ctx: &egui::Context, look: &Look) {
        if !self.gif_dialog {
            return;
        }
        let Some(info) = self.info().cloned() else { return };
        let p = look.p;
        let frame = egui::Frame::new()
            .fill(p.surf)
            .stroke(Stroke::new(1.0, p.line2))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(20))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(80) });
        let modal =
            egui::Modal::new(Id::unique("gif-dialog")).frame(frame).backdrop_color(Color32::from_black_alpha(90));
        let resp = modal.show(ctx, |ui| {
            ui.set_width(380.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(RichText::new("export gif").font(semibold(15.0)).color(p.t1));
            ui.add_space(4.0);
            ui.label(RichText::new("frame rate").font(medium(13.0)).color(p.t1));
            let fps_labels: Vec<String> = GIF_FPS.iter().map(|f| f.to_string()).collect();
            let fps: Vec<(u32, &str)> = GIF_FPS.iter().copied().zip(fps_labels.iter().map(String::as_str)).collect();
            widgets::segmented(ui, look, &mut self.gif.fps, &fps, None);
            ui.add_space(4.0);
            ui.label(RichText::new("width").font(medium(13.0)).color(p.t1));
            let widths: Vec<u32> = GIF_WIDTHS.iter().copied().filter(|&w| w < info.size.0).collect();
            let width_labels: Vec<String> = widths.iter().map(|w| w.to_string()).collect();
            let mut options = vec![(None, "original")];
            options.extend(widths.iter().map(|&w| Some(w)).zip(width_labels.iter().map(String::as_str)));
            widgets::segmented(ui, look, &mut self.gif.width, &options, None);
            let size = math::gif_size(info.size, self.gif.width);
            let frames = math::gif_frames(self.range.len(), self.gif.fps);
            let estimate = math::gif_estimate(size, frames);
            let text = format!("{}×{} · {frames} frames · about {}", size.0, size.1, human_size(estimate));
            hint(ui, text, p.t3);
            if estimate > GIF_WARN_BYTES {
                hint(ui, "that's a huge gif: lower the frame rate or width, or trim the clip shorter", p.warn);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::button(ui, look, "export", Kind::Primary, 30.0, true).clicked() {
                        self.gif_dialog = false;
                        self.export(JobKind::Gif);
                    }
                    if widgets::button(ui, look, "cancel", Kind::Secondary, 30.0, true).clicked() {
                        self.gif_dialog = false;
                    }
                });
            });
        });
        if resp.should_close() {
            self.gif_dialog = false;
        }
    }
}
