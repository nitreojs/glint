use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, Id, Key, Layout, Modifiers, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, UiBuilder, pos2, vec2,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;

use crate::app::tray::icon_rgba;
use crate::config::Config;
use crate::history::{self, Kind as HistoryKind};
use crate::platform::format::human_size;
use crate::platform::log::log;
use crate::record::Codec;
use crate::settings::widgets::{
    self, BAR_H, FOOTER_H, Kind, Look, TitleAction, hint, medium, mono, mono_medium, sans, semibold,
};
use crate::upload::{Destination, Payload};

mod actions;
mod audio;
mod edit;
mod gif;
mod math;
mod media;
mod pcm;
mod player;
mod scan;
mod timeline;
mod trim;

use actions::Saved;
use audio::{Audio, Clock};
use edit::{Edit, Undo};
use gif::GifOptions;
use math::{GIF_FPS, GIF_WARN_BYTES, GIF_WIDTHS, fmt_time, frame_len};
use media::{Com, Info};
use player::{Cmd, Player, Shown};
use scan::{Found, Scan};
use timeline::{Drag, View};
use trim::{AudioExport, Plan, Progress, Quality};

pub const CLIP_ARG: &str = "--clip";
const TITLE: &str = "glint clip";
const WINDOW: [f32; 2] = [1040.0, 760.0];
const MIN_WINDOW: [f32; 2] = [760.0, 520.0];
const PAD: f32 = 24.0;
const HEADER_H: f32 = 44.0;
const ROW_H: f32 = 32.0;
const GAP: f32 = 10.0;
const STATUS_H: f32 = 22.0;

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
    let accent = cfg.accent_rgb();
    let saved = Saved::load();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(WINDOW)
            .with_min_inner_size(MIN_WINDOW)
            .with_resizable(true)
            .with_decorations(false)
            .with_icon(egui::IconData { rgba: icon_rgba(64, accent), width: 64, height: 64 }),
        centered: saved.rect.is_none(),
        ..Default::default()
    };
    let _ = eframe::run_native(
        TITLE,
        options,
        Box::new(move |cc| {
            crate::settings::fonts(&cc.egui_ctx);
            let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(HWND(h.hwnd.get() as _)),
                _ => None,
            };
            let rounded = hwnd.is_some_and(|h| crate::settings::frameless(h, true));
            if let (Some(h), Some(r)) = (hwnd, saved.rect) {
                actions::place(h, r);
            }
            Ok(Box::new(ClipApp::new(path, info, &cfg, saved, hwnd, rounded, accent)))
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
enum JobKind {
    Trim,
    Gif,
    Copy,
}

#[derive(Clone, Copy, PartialEq)]
enum Then {
    Nothing,
    Upload,
}

#[derive(Clone, Copy, PartialEq)]
enum Selected {
    Segment(usize),
    Marker(usize),
}

struct Job {
    kind: JobKind,
    out: PathBuf,
    dest: Option<PathBuf>,
    then: Then,
    permille: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<bool, String>>>,
}

struct Upload {
    path: PathBuf,
    size: u64,
    host: String,
    copy_link: bool,
    thread: Option<JoinHandle<Result<String, String>>>,
}

enum Status {
    Saved(PathBuf),
    Cancelled,
    Failed(String),
    Note(&'static str),
    Copied,
    Uploaded { url: String, copied: bool },
}

struct ClipApp {
    path: PathBuf,
    name: String,
    info: Result<Info, String>,
    quality: Quality,
    hwnd: Option<HWND>,
    player: Option<Player>,
    player_size: (u32, u32),
    player_error: Option<String>,
    audio: Option<Audio>,
    scan: Option<Scan>,
    cuts: Vec<i64>,
    thumbs: Vec<(i64, TextureHandle)>,
    tex: Option<TextureHandle>,
    epoch: u64,
    queue: VecDeque<Shown>,
    shown: Option<(i64, i64)>,
    pos: i64,
    play: Play,
    edit: Edit,
    undo: Undo<Edit>,
    before_drag: Option<Edit>,
    selected: Option<Selected>,
    view: View,
    drag: Option<Drag>,
    muted: Vec<bool>,
    solo: Vec<bool>,
    saved: Saved,
    job: Option<Job>,
    upload: Option<Upload>,
    status: Option<Status>,
    gif_dialog: bool,
    confirm_delete: bool,
    gif: GifOptions,
    keyboard: bool,
    rounded: bool,
    light: Option<bool>,
    accent: [u8; 3],
    host: String,
}

fn fit(src: (u32, u32), area: egui::Vec2) -> egui::Vec2 {
    let (w, h) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let s = (area.x / w).min(area.y / h);
    vec2(w * s, h * s)
}

fn decode_size(info: &Info, px: egui::Vec2) -> (u32, u32) {
    let scaled = fit(info.size, px);
    if scaled.x >= info.size.0 as f32 {
        info.size
    } else {
        ((scaled.x.round() as u32).max(2) & !1, (scaled.y.round() as u32).max(2) & !1)
    }
}

impl ClipApp {
    fn new(
        path: PathBuf,
        info: Result<Info, String>,
        cfg: &Config,
        saved: Saved,
        hwnd: Option<HWND>,
        rounded: bool,
        accent: [u8; 3],
    ) -> Self {
        let duration = info.as_ref().map_or(0, |i| i.duration);
        let tracks = info.as_ref().map_or(0, |i| i.tracks.len());
        let gif = GifOptions { fps: 15, width: info.as_ref().ok().and_then(|i| (i.size.0 > 960).then_some(960)) };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        ClipApp {
            path,
            name,
            info,
            quality: Quality { qp: cfg.quality, effort: cfg.effort },
            hwnd,
            player: None,
            player_size: (0, 0),
            player_error: None,
            audio: None,
            scan: None,
            cuts: Vec::new(),
            thumbs: Vec::new(),
            tex: None,
            epoch: 0,
            queue: VecDeque::new(),
            shown: None,
            pos: 0,
            play: Play::Paused,
            edit: Edit::new(duration),
            undo: Undo::new(),
            before_drag: None,
            selected: None,
            view: View::full(duration),
            drag: None,
            muted: vec![false; tracks],
            solo: vec![false; tracks],
            saved,
            job: None,
            upload: None,
            status: None,
            gif_dialog: false,
            confirm_delete: false,
            gif,
            keyboard: false,
            rounded,
            light: None,
            accent,
            host: cfg.upload_destination.trim().to_string(),
        }
    }

    fn info(&self) -> Option<&Info> {
        self.info.as_ref().ok()
    }

    fn duration(&self) -> i64 {
        self.info().map_or(0, |i| i.duration)
    }

    fn fps(&self) -> f64 {
        self.info().map_or(30.0, |i| i.fps)
    }

    fn frame_len(&self) -> i64 {
        frame_len(self.fps())
    }

    fn owner(&self) -> HWND {
        self.hwnd.unwrap_or_default()
    }

    fn audible(&self, k: usize) -> bool {
        if self.solo.iter().any(|&s| s) { self.solo[k] } else { !self.muted[k] }
    }

    fn sync_gains(&self) {
        let Some(a) = &self.audio else { return };
        a.gains.set_volume(self.saved.volume);
        for k in 0..self.muted.len() {
            a.gains.set_track(k, if self.audible(k) { 1.0 } else { 0.0 });
        }
    }

    fn audio_changed(&self) -> bool {
        let any_muted = (0..self.muted.len()).any(|k| !self.audible(k));
        match self.saved.audio {
            AudioExport::All => false,
            AudioExport::Unmuted => any_muted,
            AudioExport::Mix => self.muted.len() > 1 || any_muted,
        }
    }

    fn edited(&self) -> bool {
        self.edit.is_edited() || self.audio_changed()
    }

    fn plan(&self) -> Plan {
        Plan {
            kept: self.edit.kept(),
            unmuted: (0..self.muted.len()).map(|k| self.audible(k)).collect(),
            audio: self.saved.audio,
        }
    }

    fn busy(&self) -> bool {
        self.job.is_some() || self.upload.is_some()
    }

    fn seek(&mut self, t: i64, play: bool) {
        self.epoch += 1;
        self.queue.clear();
        self.play = if play { Play::Starting } else { Play::Paused };
        if let Some(p) = &self.player {
            p.send(Cmd::Seek { t, epoch: self.epoch });
            p.send(Cmd::Play(play));
        }
        if let Some(a) = &self.audio {
            if play {
                a.play(t, self.epoch);
            } else {
                a.stop();
            }
        }
    }

    fn jump(&mut self, t: i64) {
        self.pos = t;
        let playing = self.play != Play::Paused;
        self.seek(t, playing);
        self.view.reveal(t, self.duration());
    }

    fn pause(&mut self) {
        if self.play != Play::Paused {
            self.play = Play::Paused;
            self.epoch += 1;
            self.queue.clear();
            if let Some(p) = &self.player {
                p.send(Cmd::Play(false));
            }
            if let Some(a) = &self.audio {
                a.stop();
            }
        }
    }

    fn toggle_play(&mut self) {
        if self.play != Play::Paused {
            self.pause();
            return;
        }
        let near_end = self.edit.kept().last().is_some_and(|r| self.pos >= r.end - self.frame_len());
        let start = match self.edit.next_kept(self.pos) {
            Some(t) if !near_end => t,
            _ => self.edit.kept().first().map_or(0, |r| r.start),
        };
        self.pos = start;
        self.seek(start, true);
    }

    fn step(&mut self, forward: bool) {
        let Some((ts, dur)) = self.shown else { return };
        let t = if forward { ts + if dur > 0 { dur } else { self.frame_len() } } else { ts - 1 };
        if t < 0 || t >= self.duration() {
            return;
        }
        self.pos = t;
        self.seek(t, false);
        self.view.reveal(t, self.duration());
    }

    fn change(&mut self, f: impl FnOnce(&mut Edit) -> bool) -> bool {
        let before = self.edit.clone();
        let changed = f(&mut self.edit);
        self.undo.record(before, &self.edit);
        changed
    }

    fn start_media(&mut self, ctx: &egui::Context, preview: egui::Vec2) {
        let Some(info) = self.info().cloned() else { return };
        let want = decode_size(&info, preview * ctx.pixels_per_point());
        let resized = (want.0 as f32 - self.player_size.0 as f32).abs() > self.player_size.0 as f32 * 0.1;
        let settled = !ctx.input(|i| i.pointer.any_down());
        if self.player.is_none() || (resized && settled) {
            self.player = None;
            self.player_size = want;
            self.player = Some(Player::start(self.path.clone(), want, ctx.clone()));
            let playing = self.play != Play::Paused;
            self.seek(self.pos, playing);
        }
        if self.audio.is_none() && !info.tracks.is_empty() {
            let streams = info.tracks.iter().map(|t| t.stream).collect();
            self.audio = Some(Audio::start(self.path.clone(), streams, self.saved.volume));
            self.sync_gains();
        }
        if self.scan.is_none() {
            self.cuts.clear();
            self.thumbs.clear();
            self.scan = Some(Scan::start(self.path.clone(), info.size, info.duration, ctx.clone()));
        }
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.scan else { return };
        while let Some(found) = scan.poll() {
            match found {
                Found::Cut(t) => self.cuts.push(t),
                Found::Thumb(t, image) => {
                    let tex = ctx.load_texture(format!("clip-thumb-{t}"), image, TextureOptions::LINEAR);
                    self.thumbs.push((t, tex));
                }
            }
        }
    }

    fn clock(&self, at: Instant, from: i64) -> i64 {
        let wall = || from + (at.elapsed().as_secs_f64() * math::SECOND as f64) as i64;
        match self.audio.as_ref().map(|a| a.clock(self.epoch)) {
            Some(Clock::At(t)) => t.max(from),
            Some(Clock::Waiting) => from,
            Some(Clock::Wall) | None => wall(),
        }
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
        let now = match self.play {
            Play::Running { at, from } => self.clock(at, from),
            Play::Starting => i64::MIN,
            Play::Paused => i64::MAX,
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
            match self.edit.next_kept(now) {
                None => {
                    self.pos = now.min(self.edit.range.end);
                    self.pause();
                }
                Some(next) if next > now + self.frame_len() / 2 => {
                    self.pos = next;
                    self.seek(next, true);
                    ctx.request_repaint();
                }
                Some(_) => {
                    self.pos = now;
                    self.view.follow(now, self.duration());
                    ctx.request_repaint();
                }
            }
        }
    }

    fn export(&mut self, kind: JobKind, dest: Option<PathBuf>, then: Then) {
        let Some(info) = self.info().cloned() else { return };
        let out = match (&dest, kind) {
            (Some(d), _) => {
                let ext = d.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
                math::unique_sibling(d, ".part", &ext)
            }
            (None, JobKind::Gif) => math::unique_sibling(&self.path, "", "gif"),
            (None, _) => math::unique_sibling(&self.path, "_trim", "mp4"),
        };
        let permille = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (src, dst, plan, quality, gif) = (self.path.clone(), out.clone(), self.plan(), self.quality, self.gif);
        let (pm, c) = (permille.clone(), cancel.clone());
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            let p = Progress { permille: &pm, cancel: &c };
            match kind {
                JobKind::Trim => trim::trim(&src, &info, &plan, &dst, quality, &p),
                JobKind::Gif => gif::export(&src, &info, &plan.kept, gif, &dst, &p),
                JobKind::Copy => std::fs::copy(&src, &dst).map(|_| true).map_err(|e| e.to_string()),
            }
        });
        self.status = None;
        self.job = Some(Job { kind, out, dest, then, permille, cancel, thread: Some(thread) });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(job) = &mut self.job else { return };
        let Some(thread) = job.thread.take_if(|t| t.is_finished()) else {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        };
        let res = thread.join().unwrap_or_else(|_| Err("the export crashed".into()));
        let Some(job) = self.job.take() else { return };
        let res = match (res, &job.dest) {
            (Ok(true), Some(dest)) => match std::fs::rename(&job.out, dest) {
                Ok(()) => Ok(dest.clone()),
                Err(e) => {
                    let _ = std::fs::remove_file(&job.out);
                    Err(format!("can't write {}: {e}", dest.display()))
                }
            },
            (Ok(true), None) => Ok(job.out.clone()),
            (Ok(false), _) => {
                let _ = std::fs::remove_file(&job.out);
                self.status = Some(Status::Cancelled);
                return;
            }
            (Err(e), _) => {
                let _ = std::fs::remove_file(&job.out);
                if job.cancel.load(Ordering::Relaxed) {
                    self.status = Some(Status::Cancelled);
                    return;
                }
                Err(e)
            }
        };
        match res {
            Ok(path) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                history::add(HistoryKind::Recording, Some(&path), size, None);
                if job.then == Then::Upload {
                    self.start_upload(path);
                } else {
                    self.status = Some(Status::Saved(path));
                }
            }
            Err(e) => {
                log(&format!("clip: exporting {} failed: {e}", self.path.display()));
                self.status = Some(Status::Failed(format!("couldn't save: {e}")));
            }
        }
    }

    fn start_upload(&mut self, path: PathBuf) {
        let cfg = Config::load().unwrap_or_default();
        let started = Destination::from_config(&cfg).and_then(|dest| {
            let payload = Payload::file(&path)?;
            let (host, size) = (dest.name.clone(), payload.size);
            let thread = std::thread::spawn(move || dest.upload(payload).map(|u| u.url));
            Ok(Upload { path, size, host, copy_link: cfg.upload_copy_link, thread: Some(thread) })
        });
        match started {
            Ok(u) => {
                self.status = None;
                self.upload = Some(u);
            }
            Err(e) => self.status = Some(Status::Failed(format!("couldn't upload: {e}"))),
        }
    }

    fn poll_upload(&mut self, ctx: &egui::Context) {
        let Some(up) = &mut self.upload else { return };
        let Some(thread) = up.thread.take_if(|t| t.is_finished()) else {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        };
        let res = thread.join().unwrap_or_else(|_| Err("the upload crashed".into()));
        let Some(up) = self.upload.take() else { return };
        self.status = Some(match res {
            Ok(url) => {
                history::add(HistoryKind::Recording, Some(&up.path), up.size, Some(&url));
                let copied = up.copy_link && crate::shot::copy_text(self.hwnd, &url).is_ok();
                Status::Uploaded { url, copied }
            }
            Err(e) => {
                log(&format!("clip: upload to {} failed: {e}", up.host));
                Status::Failed(format!("couldn't upload to {}: {e}", up.host))
            }
        });
    }

    fn save_as(&mut self) {
        let edited = self.edited();
        let suggested = if edited { math::unique_sibling(&self.path, "_trim", "mp4") } else { self.path.clone() };
        let Some(dest) = actions::save_dialog(self.owner(), &suggested, "mp4") else { return };
        if dest == self.path {
            return;
        }
        let kind = if edited { JobKind::Trim } else { JobKind::Copy };
        self.export(kind, Some(dest), Then::Nothing);
    }

    fn upload_clip(&mut self) {
        if self.edited() {
            self.export(JobKind::Trim, None, Then::Upload);
        } else {
            self.start_upload(self.path.clone());
        }
    }

    fn copy_file(&mut self) {
        self.status = Some(match crate::shot::copy_files(self.hwnd, std::slice::from_ref(&self.path)) {
            Ok(()) => Status::Copied,
            Err(e) => Status::Failed(format!("couldn't copy the file: {e}")),
        });
    }

    fn delete(&mut self, ctx: &egui::Context) {
        self.pause();
        self.player = None;
        self.audio = None;
        self.scan = None;
        match actions::recycle(self.owner(), &self.path) {
            Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => {
                log(&format!("clip: deleting {} failed: {e}", self.path.display()));
                self.status = Some(Status::Failed(format!("couldn't delete: {e}")));
            }
        }
    }

    fn keys(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let (space, left, right, marker, set_in, set_out, prev, next, split, delete, redo, undo) =
            ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Space),
                    i.consume_key(Modifiers::NONE, Key::ArrowLeft),
                    i.consume_key(Modifiers::NONE, Key::ArrowRight),
                    i.consume_key(Modifiers::NONE, Key::M),
                    i.consume_key(Modifiers::NONE, Key::I),
                    i.consume_key(Modifiers::NONE, Key::O),
                    i.consume_key(Modifiers::NONE, Key::OpenBracket),
                    i.consume_key(Modifiers::NONE, Key::CloseBracket),
                    i.consume_key(Modifiers::NONE, Key::S),
                    i.consume_key(Modifiers::NONE, Key::Delete) | i.consume_key(Modifiers::NONE, Key::Backspace),
                    i.consume_key(cmd | Modifiers::SHIFT, Key::Z) | i.consume_key(cmd, Key::Y),
                    i.consume_key(cmd, Key::Z),
                )
            });
        let (frame, duration, pos) = (self.frame_len(), self.duration(), math::snap(self.pos, self.fps()));
        if space {
            self.toggle_play();
        }
        if left || right {
            self.step(right);
        }
        if marker {
            self.change(|e| e.add_marker(pos, frame / 2));
        }
        if set_in {
            self.change(|e| {
                e.range.set_start(pos, frame);
                true
            });
        }
        if set_out {
            self.change(|e| {
                e.range.set_end(pos + frame, duration, frame);
                true
            });
        }
        let target = match (prev, next) {
            (true, _) => self.edit.prev_marker(self.pos, frame / 2),
            (_, true) => self.edit.next_marker(self.pos, frame / 2),
            _ => None,
        };
        if let Some(t) = target {
            self.jump(t);
        }
        if split && self.change(|e| e.split(pos, frame)) {
            self.selected = None;
        }
        if delete {
            let selected = self.selected;
            match selected {
                Some(Selected::Marker(i)) if i < self.edit.markers.len() => {
                    self.change(|e| {
                        e.markers.remove(i);
                        true
                    });
                    self.selected = None;
                }
                Some(Selected::Segment(i)) if !self.change(|e| e.toggle_removed(i)) => {
                    self.status = Some(Status::Note("at least one piece has to stay"));
                }
                _ => {}
            }
        }
        if redo && self.undo.redo(&mut self.edit) || undo && self.undo.undo(&mut self.edit) {
            self.selected = None;
        }
    }
}

impl Drop for ClipApp {
    fn drop(&mut self) {
        if let Some(job) = &mut self.job {
            job.cancel.store(true, Ordering::Relaxed);
            if let Some(t) = job.thread.take() {
                let _ = t.join();
            }
            let _ = std::fs::remove_file(&job.out);
        }
        self.saved.store();
    }
}

const LETTERBOX: Color32 = Color32::from_rgb(0x10, 0x10, 0x0F);

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
    let audio = match info.tracks.len() {
        0 => "no audio".to_string(),
        1 => "audio".to_string(),
        n => format!("{n} audio tracks"),
    };
    format!("{}×{} · {fps} fps · {codec} · {audio} · {}", info.size.0, info.size.1, fmt_time(info.duration))
}

impl eframe::App for ClipApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let p = widgets::palette(light, self.accent);
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
        if let Some(r) = self.hwnd.and_then(actions::window_rect) {
            self.saved.rect = Some(r);
        }
        let look = Look { p, keyboard: self.keyboard };
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let active = widgets::anim(ui, Id::unique("clip-active"), focused, 0.15);
        let radius: u8 = if self.rounded { 8 } else { 0 };
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let had_dialog = self.gif_dialog || self.confirm_delete;

        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.surf);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, "clip", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            TitleAction::None => {}
        }

        let (left, right) = (full.left() + PAD, full.right() - PAD);
        let footer = Rect::from_min_max(pos2(full.left(), full.bottom() - FOOTER_H), full.right_bottom());
        let status = Rect::from_min_max(pos2(left, footer.top() - 6.0 - STATUS_H), pos2(right, footer.top() - 6.0));
        let lanes = timeline::height();
        let line = Rect::from_min_max(pos2(left, status.top() - GAP - lanes), pos2(right, status.top() - GAP));
        let has_tracks = self.info().is_some_and(|i| !i.tracks.is_empty());
        let tracks_h = if has_tracks { ROW_H + GAP } else { 0.0 };
        let tracks_row = Rect::from_min_max(pos2(left, line.top() - GAP - tracks_h), pos2(right, line.top() - GAP));
        let transport = Rect::from_min_max(pos2(left, tracks_row.top() - ROW_H), pos2(right, tracks_row.top()));
        let preview_top = bar.bottom() + HEADER_H;
        let preview =
            Rect::from_min_max(pos2(left, preview_top), pos2(right, (transport.top() - 12.0).max(preview_top + 60.0)));

        if self.info.is_ok() {
            self.start_media(&ctx, preview.size());
        }
        self.poll_player(&ctx);
        self.poll_scan(&ctx);
        self.poll_job(&ctx);
        self.poll_upload(&ctx);
        if !self.gif_dialog && !self.confirm_delete && self.info.is_ok() {
            self.keys(&ctx);
        }

        let header_y = bar.bottom() + 22.0;
        let painter = ui.painter();
        let name = painter.text(pos2(left, header_y), Align2::LEFT_CENTER, &self.name, semibold(15.0), p.t1);
        if let Some(info) = self.info() {
            let galley = painter.layout_no_wrap(describe(info), mono(11.0), p.t3);
            let x = (right - galley.size().x).max(name.right() + 16.0);
            painter.galley(pos2(x, header_y - galley.size().y / 2.0), galley, p.t3);
        }

        self.preview(ui, &look, preview);
        self.transport(ui, &look, transport);
        if has_tracks {
            self.tracks_row(ui, &look, Rect::from_min_max(pos2(left, tracks_row.bottom() - ROW_H), tracks_row.max));
        }
        self.timeline(ui, &look, line);
        self.status_line(ui, &look, status);
        self.footer(ui, &look, footer);
        self.gif_dialog(&ctx, &look);
        self.delete_dialog(&ctx, &look);

        if esc && !had_dialog && !self.busy() {
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
        ui.label(RichText::new(fmt_time(self.pos)).font(mono_medium(12.0)).color(p.t1));
        ui.label(RichText::new(format!("/ {}", fmt_time(self.duration()))).font(mono(12.0)).color(p.t3));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = self.edit.range;
            ui.label(
                RichText::new(format!("{} – {}", fmt_time(r.start), fmt_time(r.end))).font(mono(12.0)).color(p.t2),
            );
            let kept = fmt_time(self.edit.kept_len());
            ui.label(RichText::new(format!("output {kept}")).font(sans(12.0)).color(p.t3));
            if self.audio.is_some() {
                ui.add_space(14.0);
                self.volume(ui, look);
            }
        });
    }

    fn volume(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        ui.label(RichText::new(format!("{:>3}%", (self.saved.volume * 100.0).round())).font(mono(12.0)).color(p.t3));
        let (rect, resp) = ui.allocate_exact_size(vec2(90.0, 20.0), Sense::click_and_drag());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("volume (wheel adjusts)");
        let mut v = self.saved.volume;
        if (resp.dragged() || resp.clicked())
            && let Some(at) = resp.interact_pointer_pos()
        {
            v = ((at.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        }
        if resp.hovered() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            v = (v + wheel / 600.0).clamp(0.0, 1.0);
        }
        if v != self.saved.volume {
            self.saved.volume = v;
            self.sync_gains();
        }
        let painter = ui.painter();
        let y = rect.center().y;
        let track = Rect::from_min_max(pos2(rect.left(), y - 2.0), pos2(rect.right(), y + 2.0));
        painter.rect_filled(track, 2, p.field);
        let x = rect.left() + rect.width() * v;
        painter.rect_filled(Rect::from_min_max(track.min, pos2(x, track.bottom())), 2, p.acc);
        painter.circle_filled(pos2(x, y), if resp.hovered() || resp.dragged() { 6.0 } else { 5.0 }, p.t1);
        ui.label(RichText::new("volume").font(sans(12.0)).color(p.t3));
    }

    fn tracks_row(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let names: Vec<String> = self.info().map(|i| i.tracks.iter().map(|t| t.name.clone()).collect()).unwrap_or_default();
        let many = names.len() > 1;
        let mut x = rect.left();
        let mut changed = false;
        for (k, name) in names.iter().enumerate() {
            let audible = self.audible(k);
            let color = if audible { p.t1 } else { p.t4 };
            let galley = ui.painter().layout_no_wrap(name.clone(), medium(12.0), color);
            let chip = Rect::from_min_size(pos2(x, rect.center().y - 13.0), vec2(galley.size().x + 32.0, 26.0));
            let tip = if self.muted[k] { "muted: click to unmute" } else { "click to mute" };
            let resp = ui
                .interact(chip, Id::unique(("clip-track-mute", k)), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(tip);
            if resp.clicked() {
                self.muted[k] = !self.muted[k];
                changed = true;
            }
            let h = widgets::anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
            let painter = ui.painter();
            let fill = if audible { p.ctl } else { p.field };
            painter.rect(chip, 5, fill.lerp_to_gamma(p.ctlh, h), Stroke::new(1.0, p.line2), egui::StrokeKind::Inside);
            let dot = pos2(chip.left() + 13.0, chip.center().y);
            painter.circle_filled(dot, 3.5, if audible { p.acc } else { p.t4 });
            let text = pos2(chip.left() + 22.0, chip.center().y - galley.size().y / 2.0);
            let width = galley.size().x;
            painter.galley(text, galley, color);
            if self.muted[k] {
                let y = chip.center().y;
                painter.hline(text.x..=text.x + width, y, Stroke::new(1.0, p.t4));
            }
            x = chip.right() + 4.0;
            if many {
                let solo = Rect::from_min_size(pos2(x, chip.top()), vec2(26.0, 26.0));
                let resp = ui
                    .interact(solo, Id::unique(("clip-track-solo", k)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("solo: hear only this track");
                if resp.clicked() {
                    self.solo[k] = !self.solo[k];
                    changed = true;
                }
                let h = widgets::anim(ui, resp.id.with("hov"), resp.hovered(), 0.08);
                let painter = ui.painter();
                let (fill, fg) = if self.solo[k] {
                    (p.acc, Color32::WHITE)
                } else {
                    (p.ctl.lerp_to_gamma(p.ctlh, h), p.t2)
                };
                painter.rect(solo, 5, fill, Stroke::new(1.0, p.line2), egui::StrokeKind::Inside);
                painter.text(solo.center(), Align2::CENTER_CENTER, "s", medium(12.0), fg);
                x = solo.right() + 4.0;
            }
            x += 8.0;
        }
        if changed {
            self.sync_gains();
        }
        let mut options = vec![(AudioExport::All, "all tracks"), (AudioExport::Unmuted, "unmuted")];
        if many {
            options.push((AudioExport::Mix, "mixed"));
        } else if self.saved.audio == AudioExport::Mix {
            self.saved.audio = AudioExport::Unmuted;
        }
        let width = 92.0 * options.len() as f32;
        let seg = Rect::from_min_max(pos2(rect.right() - width, rect.top()), rect.right_bottom());
        let label_x = seg.left() - 10.0;
        if label_x > x + 80.0 {
            ui.painter().text(pos2(label_x, rect.center().y), Align2::RIGHT_CENTER, "export audio", sans(12.0), p.t3);
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(seg).layout(Layout::left_to_right(Align::Center)));
        widgets::segmented(&mut child, look, &mut self.saved.audio, &options, Some(width)).on_hover_text(
            "all tracks keeps every audio track · unmuted drops muted tracks · mixed merges the audible tracks into one",
        );
    }

    fn footer(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, p.line));
        let inner =
            Rect::from_min_max(pos2(rect.left() + PAD, rect.top() + 1.0), pos2(rect.right() - PAD, rect.bottom()));
        let mut ui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        let ready = self.info.is_ok() && !self.busy();
        let edited = self.edited();
        let cut = self.edit.segments.iter().any(|s| s.removed) || self.audio_changed();
        let host = self.host.clone();
        let mut open = false;
        let mut copy = false;
        let mut save_as = false;
        let mut upload = false;
        let mut delete = false;
        let mut gif = false;
        let mut save = false;
        open |= widgets::button(&mut ui, look, "open folder", Kind::Text, 30.0, true).clicked();
        copy |= widgets::button(&mut ui, look, "copy file", Kind::Text, 30.0, true)
            .on_hover_text("copies the original file: paste it into explorer, telegram or discord")
            .clicked();
        let save_tip = if edited { "exports your edit to a file you choose" } else { "saves a copy of the file" };
        save_as |=
            widgets::button(&mut ui, look, "save as…", Kind::Text, 30.0, ready).on_hover_text(save_tip).clicked();
        let upload_tip =
            if edited { format!("exports your edit and uploads it to {host}") } else { format!("uploads to {host}") };
        upload |= widgets::button(&mut ui, look, "upload", Kind::Text, 30.0, ready).on_hover_text(upload_tip).clicked();
        delete |= widgets::button(&mut ui, look, "delete", Kind::Text, 30.0, !self.busy())
            .on_hover_text("moves the file to the recycle bin")
            .clicked();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
            let label = if cut { "export" } else { "save trimmed" };
            save = widgets::button(ui, look, label, Kind::Primary, 30.0, ready && edited).clicked();
            gif = widgets::button(ui, look, "export gif…", Kind::Secondary, 30.0, ready).clicked();
        });
        if open {
            actions::show_in_folder(&self.path);
        }
        if copy {
            self.copy_file();
        }
        if save_as {
            self.pause();
            self.save_as();
        }
        if upload {
            self.upload_clip();
        }
        if delete {
            self.pause();
            self.confirm_delete = true;
        }
        if save {
            self.export(JobKind::Trim, None, Then::Nothing);
        }
        if gif {
            self.pause();
            self.gif_dialog = true;
        }
    }

    fn status_line(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        let mut ui = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)));
        let ui = &mut ui;
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let text = |ui: &mut Ui, s: String, color: Color32| {
            ui.add(egui::Label::new(RichText::new(s).font(sans(13.0)).color(color)).truncate());
        };
        if let Some(up) = &self.upload {
            text(ui, format!("uploading to {}…", up.host), p.t2);
            return;
        }
        if let Some(job) = &self.job {
            let what = match (job.kind, job.then) {
                (_, Then::Upload) => "exporting for the upload",
                (JobKind::Trim, _) => "exporting",
                (JobKind::Gif, _) => "exporting the gif",
                (JobKind::Copy, _) => "saving a copy",
            };
            ui.label(RichText::new(what).font(sans(13.0)).color(p.t2));
            if job.kind == JobKind::Copy {
                return;
            }
            let permille = job.permille.load(Ordering::Relaxed);
            let (bar, _) = ui.allocate_exact_size(vec2(160.0, 4.0), Sense::hover());
            ui.painter().rect_filled(bar, 2, p.field);
            let done = Rect::from_min_size(bar.min, vec2(bar.width() * permille as f32 / 1000.0, bar.height()));
            ui.painter().rect_filled(done, 2, p.acc);
            ui.label(RichText::new(format!("{}%", permille / 10)).font(mono(12.0)).color(p.t3));
            if widgets::button(ui, look, "cancel", Kind::Text, 22.0, !job.cancel.load(Ordering::Relaxed)).clicked() {
                job.cancel.store(true, Ordering::Relaxed);
            }
            return;
        }
        match &self.status {
            Some(Status::Saved(path)) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let size = std::fs::metadata(path).map(|m| human_size(m.len())).unwrap_or_default();
                if widgets::link(ui, look, "show in folder").clicked() {
                    actions::show_in_folder(path);
                }
                text(ui, format!("saved {name} · {size}"), p.t2);
            }
            Some(Status::Uploaded { url, copied }) => {
                if widgets::link(ui, look, "open").clicked() {
                    actions::open_url(url);
                }
                let what = if *copied { "uploaded, link copied" } else { "uploaded" };
                text(ui, format!("{what}: {url}"), p.t2);
            }
            Some(Status::Copied) => text(ui, "copied, paste it anywhere".into(), p.t2),
            Some(Status::Cancelled) => text(ui, "cancelled, nothing was saved".into(), p.t3),
            Some(Status::Note(s)) => text(ui, (*s).into(), p.t3),
            Some(Status::Failed(e)) => text(ui, e.clone(), p.warn),
            None => {
                let help = "space play · ← → frames · i / o in and out · m marker · [ ] markers · s split · \
                            delete removes · ctrl+z / ctrl+y undo · ctrl+wheel zoom · hold ctrl to stop snapping";
                ui.add(egui::Label::new(RichText::new(help).font(sans(12.0)).color(p.t3)).truncate())
                    .on_hover_text(help);
            }
        }
    }

    fn modal_frame(look: &Look) -> egui::Frame {
        egui::Frame::new()
            .fill(look.p.surf)
            .stroke(Stroke::new(1.0, look.p.line2))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(20))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(80) })
    }

    fn delete_dialog(&mut self, ctx: &egui::Context, look: &Look) {
        if !self.confirm_delete {
            return;
        }
        let p = look.p;
        let modal = egui::Modal::new(Id::unique("delete-dialog"))
            .frame(Self::modal_frame(look))
            .backdrop_color(Color32::from_black_alpha(90));
        let mut delete = false;
        let resp = modal.show(ctx, |ui| {
            ui.set_width(360.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(RichText::new("delete this recording?").font(semibold(15.0)).color(p.t1));
            hint(ui, format!("{} goes to the recycle bin.", self.name), p.t3);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    delete = widgets::button(ui, look, "delete", Kind::Primary, 30.0, true).clicked();
                    if widgets::button(ui, look, "cancel", Kind::Secondary, 30.0, true).clicked() {
                        self.confirm_delete = false;
                    }
                });
            });
        });
        if resp.should_close() {
            self.confirm_delete = false;
        }
        if delete {
            self.confirm_delete = false;
            self.delete(ctx);
        }
    }

    fn gif_dialog(&mut self, ctx: &egui::Context, look: &Look) {
        if !self.gif_dialog {
            return;
        }
        let Some(info) = self.info().cloned() else { return };
        let p = look.p;
        let modal = egui::Modal::new(Id::unique("gif-dialog"))
            .frame(Self::modal_frame(look))
            .backdrop_color(Color32::from_black_alpha(90));
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
            let frames = math::gif_frames(self.edit.kept_len(), self.gif.fps);
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
                        self.export(JobKind::Gif, None, Then::Nothing);
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
