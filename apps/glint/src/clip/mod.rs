use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, Event, Id, Key, Modifiers, Rect, TextureFilter, TextureHandle, TextureOptions, Ui, pos2, vec2,
};
use glint_core::clip::clicks::Clicks;
use glint_core::clip::keys::Keys;
use glint_core::clip::crop::{Aspect, Crop, Grip};
use glint_core::clip::edit::{Edit, Undo};
use glint_core::clip::export::{Format, Job, Source, Video, VideoJob};
use glint_core::clip::glide::Glide;
use glint_core::clip::index::Index;
use glint_core::clip::info::Info;
use glint_core::clip::math::frame_len;
use glint_core::clip::viewer::Viewer;
use glint_core::clip::{export, index, math};
use glint_core::config::Config;
use glint_core::history::{self, Kind as HistoryKind};
use glint_core::icon::icon_rgba;
use glint_core::log::log;
use glint_core::upload::{Destination, Payload};
use glint_media::clip::audio::{Audio, Clock};
use glint_media::clip::media::Com;
use glint_media::clip::trim::Progress;
use glint_media::clip::{media, trim};
use glint_ui::widgets::{self, BAR_H, Look, TitleAction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC;

mod actions;
mod clicklane;
mod keylane;
mod overlay;
mod panel;
mod player;
mod preview;
mod scan;
mod timeline;
mod wave;

use actions::Saved;
use player::{Cmd, Player, Shown};
use scan::{Found, Scan};
use clicklane::ClickDrag;
use keylane::KeyDrag;
use timeline::{Drag, View};
use wave::Waves;

pub const CLIP_ARG: &str = "--clip";
const TITLE: &str = "glint clip";
const WINDOW: [f32; 2] = [1100.0, 740.0];
const MIN_WINDOW: [f32; 2] = [860.0, 560.0];
const SIDE_W: f32 = 280.0;
const PAD: f32 = 16.0;
const GAP: f32 = 12.0;
const FILE_H: f32 = 20.0;
const HINT_H: f32 = 20.0;
const SAVED_FLASH: f64 = 1.4;

pub(crate) fn spawn(path: &Path) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).arg(CLIP_ARG).arg(path).spawn().map(|_| ())
}

pub fn run(path: PathBuf) {
    let probe = path.clone();
    let (info, hevc) = std::thread::spawn(move || {
        let _com = Com::init();
        (media::probe(&probe), media::has_encoder(MFVideoFormat_HEVC))
    })
    .join()
    .unwrap_or_else(|_| (Err("the video couldn't be read".into()), false));
    if let Err(e) = &info {
        log(&format!("clip: {}: {e}", path.display()));
    }
    let index = info.as_ref().ok().and_then(|_| {
        index::read(&path).map_err(|e| log(&format!("clip: no sample index for {}: {e}", path.display()))).ok()
    });
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
            glint_ui::theme::fonts(&cc.egui_ctx);
            let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
                Ok(RawWindowHandle::Win32(h)) => Some(HWND(h.hwnd.get() as _)),
                _ => None,
            };
            let rounded = hwnd.is_some_and(|h| glint_ui::window::frameless(h, true));
            if let (Some(h), Some(r)) = (hwnd, saved.rect) {
                glint_ui::window::place(h, r);
            }
            let mut app = ClipApp::new(path, info, index, &cfg, saved, accent);
            (app.hwnd, app.rounded, app.live, app.hevc) = (hwnd, rounded, true, hevc);
            Ok(Box::new(app))
        }),
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Play {
    Paused,
    Starting,
    Running { at: Instant, from: i64 },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Then {
    Nothing,
    Upload,
    Copy,
}

#[derive(Clone, Copy, PartialEq)]
enum Selected {
    Segment(usize),
    Marker(usize),
}

/// An export in flight.
struct Task {
    out: PathBuf,
    dest: Option<PathBuf>,
    then: Then,
    copy: bool,
    /// Compressing: what the exported stretch takes in the source, to compare the result with.
    before: Option<u64>,
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

#[derive(Clone, Debug, PartialEq)]
enum Note {
    /// The file, and when compressing how its size compares with the source's.
    Saved(PathBuf, Option<String>),
    Uploaded { url: String, copied: bool },
    Copied(Option<String>),
    Info(String),
    Failed(String),
}

struct Toast {
    note: Note,
    at: f64,
}

/// Space plays on release unless the hold was used to pan.
#[derive(Default)]
struct Space {
    held: bool,
    panned: bool,
}

struct Cropping {
    aspect: Aspect,
    grab: Option<(Grip, Crop, egui::Pos2)>,
}

struct ClipApp {
    path: PathBuf,
    name: String,
    info: Result<Info, String>,
    index: Option<Index>,
    file_bytes: u64,
    effort: u32,
    hwnd: Option<HWND>,
    live: bool,
    player: Option<Player>,
    player_size: (u32, u32),
    player_error: Option<String>,
    audio: Option<Audio>,
    scan: Option<Scan>,
    waves: Option<Waves>,
    peaks: Vec<Vec<u8>>,
    cuts: Vec<i64>,
    thumbs: Vec<(i64, TextureHandle)>,
    tex: Option<TextureHandle>,
    image: Option<Arc<ColorImage>>,
    nearest: bool,
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
    viewer: Viewer,
    panning: bool,
    space: Space,
    cropping: Option<Cropping>,
    saved: Saved,
    custom_mb: String,
    custom_mbps: String,
    /// Windows can encode hevc.
    hevc: bool,
    task: Option<Task>,
    upload: Option<Upload>,
    toast: Option<Toast>,
    flash: f64,
    menu: bool,
    save_rect: Rect,
    sheet: bool,
    confirm_delete: bool,
    gone: bool,
    lane_menu: Option<(usize, egui::Pos2, Rect)>,
    renaming: Option<(usize, String)>,
    lane_drag: Option<usize>,
    playhead: Glide,
    trim: [Glide; 2],
    click_sel: Vec<usize>,
    click_focus: bool,
    click_menu: Option<(egui::Pos2, Rect)>,
    click_drag: Option<ClickDrag>,
    key_sel: Vec<usize>,
    key_focus: bool,
    key_menu: Option<(egui::Pos2, Rect)>,
    key_drag: Option<KeyDrag>,
    frame_rect: Rect,
    keyboard: bool,
    rounded: bool,
    light: Option<bool>,
    accent: [u8; 3],
    host: String,
    now: f64,
}

fn fit(src: (u32, u32), area: egui::Vec2) -> egui::Vec2 {
    let (w, h) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let s = (area.x / w).min(area.y / h);
    vec2(w * s, h * s)
}

fn decode_size(info: &Info, px: egui::Vec2) -> (u32, u32) {
    let scaled = fit(info.size, px * 2.0);
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
        index: Option<Index>,
        cfg: &Config,
        saved: Saved,
        accent: [u8; 3],
    ) -> Self {
        let duration = info.as_ref().map_or(0, |i| i.duration);
        let tracks = info.as_ref().map_or(0, |i| i.tracks.len());
        let clicks = info.as_ref().ok().and_then(|i| i.clicks.as_ref()).map(Clicks::new);
        let keys = info.as_ref().ok().and_then(|i| i.keys.as_ref()).map(Keys::new);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let file_bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        let custom_mb = match saved.export.target_mb {
            Some(mb) if !export::TARGETS_MB.contains(&mb) => mb.to_string(),
            _ => String::new(),
        };
        let custom_mbps = saved.export.kbps.map_or(String::new(), |k| format!("{}", k as f64 / 1000.0));
        ClipApp {
            path,
            name,
            info,
            index,
            file_bytes,
            effort: cfg.effort,
            hwnd: None,
            live: false,
            player: None,
            player_size: (0, 0),
            player_error: None,
            audio: None,
            scan: None,
            waves: None,
            peaks: vec![Vec::new(); tracks],
            cuts: Vec::new(),
            thumbs: Vec::new(),
            tex: None,
            image: None,
            nearest: false,
            epoch: 0,
            queue: VecDeque::new(),
            shown: None,
            pos: 0,
            play: Play::Paused,
            edit: Edit { clicks, keys, ..Edit::new(duration, tracks) },
            undo: Undo::default(),
            before_drag: None,
            selected: None,
            view: View::full(duration),
            drag: None,
            viewer: Viewer::default(),
            panning: false,
            space: Space::default(),
            cropping: None,
            saved,
            custom_mb,
            custom_mbps,
            hevc: false,
            task: None,
            upload: None,
            toast: None,
            flash: f64::NEG_INFINITY,
            gone: false,
            lane_menu: None,
            renaming: None,
            lane_drag: None,
            playhead: Glide::new(0),
            trim: [Glide::new(0), Glide::new(duration)],
            click_sel: Vec::new(),
            click_focus: false,
            click_menu: None,
            click_drag: None,
            frame_rect: Rect::NOTHING,
            menu: false,
            key_sel: Vec::new(),
            key_focus: false,
            key_menu: None,
            key_drag: None,
            save_rect: Rect::NOTHING,
            sheet: false,
            confirm_delete: false,
            keyboard: false,
            rounded: false,
            light: None,
            accent,
            host: cfg.upload_destination.trim().to_string(),
            now: 0.0,
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

    fn source(&self) -> Option<Source<'_>> {
        Some(Source { info: self.info()?, index: self.index.as_ref(), bytes: self.file_bytes, accent: self.accent })
    }

    /// What saving would write right now.
    fn job(&self) -> Option<Job> {
        let src = self.source()?;
        Some(export::plan(&self.edit, &self.saved.export, &src))
    }

    fn sync_gains(&self) {
        let Some(a) = &self.audio else { return };
        a.gains.set_volume(self.saved.volume);
        for k in 0..self.info().map_or(0, |i| i.tracks.len()) {
            let heard = self.edit.lanes.iter().any(|l| l.source == Some(k) && !l.muted);
            a.gains.set_track(k, if heard { 1.0 } else { 0.0 });
        }
    }

    fn busy(&self) -> bool {
        self.task.is_some() || self.upload.is_some()
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

    fn set_muted(&mut self, lane: usize, muted: bool) {
        self.change(|e| e.set_muted(lane, muted));
        self.sync_gains();
    }

    fn toggle_crop(&mut self) {
        let Some(size) = self.info().map(|i| i.size) else { return };
        if self.cropping.take().is_some() {
            if self.edit.crop.is_some_and(|c| c.is_full(size)) {
                self.change(|e| {
                    e.crop = None;
                    true
                });
            }
            return;
        }
        if self.edit.crop.is_none() {
            self.change(|e| {
                e.crop = Some(Crop::full(size));
                true
            });
        }
        self.cropping = Some(Cropping { aspect: Aspect::Free, grab: None });
    }

    fn start_media(&mut self, ctx: &egui::Context, shown_px: egui::Vec2, settled: bool) {
        if self.gone {
            return;
        }
        let Some(info) = self.info().cloned() else { return };
        let want = decode_size(&info, shown_px);
        let resized = (want.0 as f32 - self.player_size.0 as f32).abs() > self.player_size.0 as f32 * 0.1;
        let settled = settled && !ctx.input(|i| i.pointer.any_down());
        if self.player.is_none() || (resized && settled) {
            self.player = None;
            self.player_size = want;
            self.player = Some(Player::start(self.path.clone(), want, ctx.clone()));
            let playing = self.play != Play::Paused;
            self.seek(self.pos, playing);
        }
        if self.audio.is_none() && !info.tracks.is_empty() {
            let streams: Vec<u32> = info.tracks.iter().map(|t| t.stream).collect();
            self.audio = Some(Audio::start(self.path.clone(), streams.clone(), self.saved.volume));
            self.sync_gains();
            self.waves = Some(Waves::start(self.path.clone(), streams, ctx.clone()));
        }
        if self.scan.is_none() {
            self.cuts.clear();
            self.thumbs.clear();
            self.scan = Some(Scan::start(self.path.clone(), info.size, info.duration, ctx.clone()));
        }
    }

    fn poll_scan(&mut self, ctx: &egui::Context) {
        if let Some(w) = &self.waves {
            w.poll(&mut self.peaks);
        }
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

    fn texture_options(&self) -> TextureOptions {
        if self.nearest { TextureOptions::NEAREST } else { TextureOptions { mipmap_mode: Some(TextureFilter::Linear), ..TextureOptions::LINEAR } }
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
        if let (Play::Running { .. }, Some(p)) = (self.play, &self.player) {
            p.set_clock(now);
        }
        let mut latest = None;
        while self.queue.front().is_some_and(|s| s.ts <= now) {
            latest = self.queue.pop_front();
        }
        if let Some(s) = latest {
            self.shown = Some((s.ts, s.dur));
            if self.play == Play::Paused && self.drag.is_none() {
                self.pos = s.ts;
            }
            let image = Arc::new(s.image);
            let options = self.texture_options();
            match &mut self.tex {
                Some(t) => t.set(image.clone(), options),
                None => self.tex = Some(ctx.load_texture("clip-frame", image.clone(), options)),
            }
            self.image = Some(image);
        }
        if let Play::Running { .. } = self.play {
            match self.edit.next_kept(now) {
                None => {
                    self.pos = now.min(self.edit.range.end);
                    self.pause();
                }
                Some(next) if next > now + self.frame_len() / 2 => {
                    self.pos = next;
                    self.playhead.set(next);
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

    fn set_nearest(&mut self, nearest: bool) {
        if self.nearest == nearest {
            return;
        }
        self.nearest = nearest;
        let options = self.texture_options();
        if let (Some(t), Some(image)) = (&mut self.tex, &self.image) {
            t.set(image.clone(), options);
        }
    }

    fn toast(&mut self, note: Note) {
        self.toast = Some(Toast { note, at: self.now });
    }

    fn default_out(&self, gif: bool) -> PathBuf {
        if gif { math::unique_sibling(&self.path, "", "gif") } else { math::unique_sibling(&self.path, "_trim", "mp4") }
    }

    /// Saves (or saves to `dest`), then does `then` with the file.
    fn export(&mut self, dest: Option<PathBuf>, then: Then) {
        if self.busy() {
            return;
        }
        let Some(job) = self.job() else { return };
        let Some(info) = self.info().cloned() else { return };
        let whole = matches!(&job, Job::Video(v) if v.whole(&info));
        if whole && dest.is_none() && then != Then::Nothing {
            self.finish(self.path.clone(), then, None);
            return;
        }
        let gif = matches!(job, Job::Gif(_));
        let compressing = matches!(&job, Job::Video(VideoJob { video: Video::Encode { qp: Some(_), .. }, .. }));
        let before = self.source().filter(|_| compressing).map(|s| s.bytes_for(job.len()));
        let out = match &dest {
            Some(d) => {
                let ext = d.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
                math::unique_sibling(d, ".part", &ext)
            }
            None => self.default_out(gif),
        };
        self.pause();
        let permille = Arc::new(AtomicU32::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let (src, dst, effort) = (self.path.clone(), out.clone(), self.effort);
        let (pm, c) = (permille.clone(), cancel.clone());
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            let p = Progress { permille: &pm, cancel: &c };
            if whole {
                std::fs::copy(&src, &dst).map(|_| true).map_err(|e| e.to_string())
            } else {
                trim::export(&src, &info, &job, &dst, effort, &p)
            }
        });
        self.toast = None;
        self.task = Some(Task { out, dest, then, copy: whole, before, permille, cancel, thread: Some(thread) });
    }

    fn poll_task(&mut self, ctx: &egui::Context) {
        let Some(task) = &mut self.task else { return };
        let Some(thread) = task.thread.take_if(|t| t.is_finished()) else {
            ctx.request_repaint_after(Duration::from_millis(33));
            return;
        };
        let res = thread.join().unwrap_or_else(|_| Err("the export crashed".into()));
        let Some(task) = self.task.take() else { return };
        let res = match (res, &task.dest) {
            (Ok(true), Some(dest)) => match std::fs::rename(&task.out, dest) {
                Ok(()) => Ok(dest.clone()),
                Err(e) => {
                    let _ = std::fs::remove_file(&task.out);
                    Err(format!("can't write {}: {e}", dest.display()))
                }
            },
            (Ok(true), None) => Ok(task.out.clone()),
            (Ok(false), _) => {
                let _ = std::fs::remove_file(&task.out);
                self.toast(Note::Info("cancelled, nothing was saved".into()));
                return;
            }
            (Err(e), _) => {
                let _ = std::fs::remove_file(&task.out);
                if task.cancel.load(Ordering::Relaxed) {
                    self.toast(Note::Info("cancelled, nothing was saved".into()));
                    return;
                }
                Err(e)
            }
        };
        match res {
            Ok(path) => {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                history::add(HistoryKind::Recording, Some(&path), size, None, None);
                if task.then == Then::Nothing {
                    self.flash = self.now;
                }
                let saving = task.before.and_then(|b| export::saving_label(b, size));
                self.finish(path, task.then, saving);
            }
            Err(e) => {
                log(&format!("clip: exporting {} failed: {e}", self.path.display()));
                self.toast(Note::Failed(format!("couldn't save: {e}")));
            }
        }
    }

    fn finish(&mut self, path: PathBuf, then: Then, saving: Option<String>) {
        match then {
            Then::Nothing => self.toast(Note::Saved(path, saving)),
            Then::Upload => self.start_upload(path),
            Then::Copy => match glint_shell::clipboard::copy_files(self.hwnd, std::slice::from_ref(&path)) {
                Ok(()) => self.toast(Note::Copied(saving)),
                Err(e) => self.toast(Note::Failed(format!("couldn't copy the file: {e}"))),
            },
        }
    }

    fn start_upload(&mut self, path: PathBuf) {
        let cfg = Config::load().unwrap_or_default();
        let started = Destination::from_config(&cfg).and_then(|dest| {
            let payload = Payload::file(&path)?;
            let (host, size) = (dest.name.clone(), payload.size);
            let thread = std::thread::spawn(move || crate::upload::upload(&dest, payload).map(|u| u.url));
            Ok(Upload { path, size, host, copy_link: cfg.upload_copy_link, thread: Some(thread) })
        });
        match started {
            Ok(u) => {
                self.toast = None;
                self.upload = Some(u);
            }
            Err(e) => self.toast(Note::Failed(format!("couldn't upload: {e}"))),
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
        let note = match res {
            Ok(url) => {
                history::add(HistoryKind::Recording, Some(&up.path), up.size, Some(&url), None);
                let copied = up.copy_link && glint_shell::clipboard::copy_text(self.hwnd, &url).is_ok();
                Note::Uploaded { url, copied }
            }
            Err(e) => {
                log(&format!("clip: upload to {} failed: {e}", up.host));
                Note::Failed(format!("couldn't upload to {}: {e}", up.host))
            }
        };
        self.toast(note);
    }

    fn save(&mut self) {
        self.export(None, Then::Nothing);
    }

    fn save_as(&mut self) {
        if self.busy() {
            return;
        }
        let gif = self.saved.export.format == Format::Gif;
        self.pause();
        let suggested = self.default_out(gif);
        let Some(dest) = actions::save_dialog(self.owner(), &suggested, if gif { "gif" } else { "mp4" }) else {
            return;
        };
        if dest != self.path {
            self.export(Some(dest), Then::Nothing);
        }
    }

    fn delete(&mut self, ctx: &egui::Context) {
        self.pause();
        self.player = None;
        self.audio = None;
        self.scan = None;
        self.waves = None;
        match glint_shell::explorer::recycle(self.owner(), &self.path) {
            Ok(()) => {
                self.gone = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => {
                log(&format!("clip: deleting {} failed: {e}", self.path.display()));
                self.toast(Note::Failed(format!("couldn't delete: {e}")));
            }
        }
    }

    /// Space plays on release, so a hold can pan instead.
    fn space_key(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            if let Event::Key { key: Key::Space, pressed, repeat, modifiers, .. } = e {
                if modifiers.ctrl || modifiers.alt {
                    continue;
                }
                if pressed && !repeat {
                    self.space = Space { held: true, panned: false };
                } else if !pressed && self.space.held {
                    if !self.space.panned {
                        self.toggle_play();
                    }
                    self.space = Space::default();
                }
            }
        }
        ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space));
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused() {
            return;
        }
        let navigating = self.keyboard && ctx.memory(|m| m.focused().is_some());
        if !navigating {
            self.space_key(ctx);
        }
        let cmd = Modifiers::COMMAND;
        let none = Modifiers::NONE;
        let (left, right, marker, set_in, set_out, prev, next, split, delete, redo, undo) = ctx.input_mut(|i| {
            let arrows = !navigating;
            (
                arrows && i.consume_key(none, Key::ArrowLeft),
                arrows && i.consume_key(none, Key::ArrowRight),
                i.consume_key(none, Key::M),
                i.consume_key(none, Key::I),
                i.consume_key(none, Key::O),
                i.consume_key(none, Key::OpenBracket),
                i.consume_key(none, Key::CloseBracket),
                i.consume_key(none, Key::S),
                i.consume_key(none, Key::Delete) | i.consume_key(none, Key::Backspace),
                i.consume_key(cmd | Modifiers::SHIFT, Key::Z) | i.consume_key(cmd, Key::Y),
                i.consume_key(cmd, Key::Z),
            )
        });
        let (crop, fit, actual, enter, save_as, upload, keys, copy) = ctx.input_mut(|i| {
            let question = i.events.iter().any(|e| matches!(e, Event::Text(t) if t == "?"));
            (
                i.consume_key(none, Key::C),
                i.consume_key(none, Key::Num0),
                i.consume_key(none, Key::Num1),
                !navigating && i.consume_key(none, Key::Enter),
                i.consume_key(cmd | Modifiers::SHIFT, Key::S),
                i.consume_key(cmd, Key::U),
                question || i.key_pressed(Key::Questionmark),
                i.events.iter().any(|e| matches!(e, Event::Copy)),
            )
        });
        if keys {
            self.sheet = !self.sheet;
            self.menu = false;
        }
        let (frame, duration, pos) = (self.frame_len(), self.duration(), math::snap(self.pos, self.fps()));
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
        if delete && !self.click_sel.is_empty() {
            let picked = std::mem::take(&mut self.click_sel);
            self.change(|e| e.clicks.as_mut().is_some_and(|c| c.delete(&picked)));
        } else if delete && !self.key_sel.is_empty() {
            let picked = std::mem::take(&mut self.key_sel);
            self.change(|e| e.keys.as_mut().is_some_and(|k| k.delete(&picked)));
        } else if delete {
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
                    self.toast(Note::Info("at least one piece has to stay".into()));
                }
                _ => {}
            }
        }
        if redo && self.undo.redo(&mut self.edit) || undo && self.undo.undo(&mut self.edit) {
            self.selected = None;
            self.lane_menu = None;
            self.click_menu = None;
            self.click_sel.clear();
            self.key_menu = None;
            self.key_sel.clear();
            self.sync_gains();
        }
        if crop {
            self.toggle_crop();
        }
        let now = self.now;
        let img = self.info().map_or(vec2(1.0, 1.0), |i| vec2(i.size.0 as f32, i.size.1 as f32));
        let ppp = ctx.pixels_per_point();
        let current = self.viewer.target();
        if fit {
            self.viewer.fit_view(now, current, img);
        }
        if actual {
            self.viewer.actual_size(now, current, ppp, img);
        }
        if enter {
            if self.cropping.is_some() {
                self.toggle_crop();
            } else if !self.menu && !self.sheet {
                self.save();
            }
        }
        if save_as {
            self.save_as();
        }
        if upload {
            self.export(None, Then::Upload);
        }
        if copy {
            self.export(None, Then::Copy);
        }
    }
}

impl Drop for ClipApp {
    fn drop(&mut self) {
        if let Some(task) = &mut self.task {
            task.cancel.store(true, Ordering::Relaxed);
            if let Some(t) = task.thread.take() {
                let _ = t.join();
            }
            let _ = std::fs::remove_file(&task.out);
        }
        if self.live {
            self.saved.store();
        }
    }
}

const LETTERBOX: Color32 = Color32::from_rgb(0x0B, 0x0B, 0x0B);

fn describe(info: &Info) -> String {
    let fps = if (info.fps - info.fps.round()).abs() < 0.01 { format!("{:.0}", info.fps) } else { format!("{:.2}", info.fps) };
    let sound = match info.tracks.len() {
        0 => "no sound".to_string(),
        1 => "1 sound track".to_string(),
        n => format!("{n} sound tracks"),
    };
    format!("{}×{} · {fps} fps · {} · {sound}", info.size.0, info.size.1, math::fmt_short(info.duration))
}

impl eframe::App for ClipApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.now = ctx.input(|i| i.time);
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let p = widgets::palette(light, self.accent);
        if self.light != Some(light) {
            self.light = Some(light);
            glint_ui::theme::visuals(&ctx, &p, light);
        }
        ctx.input(|i| {
            if i.pointer.any_pressed() {
                self.keyboard = false;
            } else if i.key_pressed(Key::Tab) {
                self.keyboard = true;
            }
        });
        if let Some(r) = self.hwnd.and_then(glint_ui::window::placement) {
            self.saved.rect = Some(r);
        }
        let look = Look { p, keyboard: self.keyboard };
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let active = widgets::anim(ui, Id::unique("clip-active"), focused, 0.15);
        let radius: u8 = if self.rounded { 8 } else { 0 };
        let esc = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));

        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.surf);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, "glint", "clip", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            TitleAction::None => {}
        }
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, egui::Stroke::new(1.0, p.line));

        let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
        let side = Rect::from_min_max(pos2(body.right() - SIDE_W, body.top()), body.max);
        let main = Rect::from_min_max(body.min + vec2(PAD, PAD), pos2(side.left() - PAD, body.bottom() - GAP));
        let lanes = if self.info.is_ok() { self.edit.lanes.len() } else { 0 };
        let room = (main.height() - FILE_H - HINT_H - 3.0 * GAP) * 0.45;
        let clicks = self.info.is_ok() && self.edit.clicks.is_some();
        let keys = self.info.is_ok() && self.edit.keys.is_some();
        let lane = timeline::lane_height(lanes, room, clicks, keys);
        let line_h = timeline::height(lanes, lane, clicks, keys);
        let hint = Rect::from_min_max(pos2(main.left(), main.bottom() - HINT_H), main.max);
        let line = Rect::from_min_max(pos2(main.left(), hint.top() - GAP - line_h), pos2(main.right(), hint.top() - GAP));
        let file = Rect::from_min_size(main.min, vec2(main.width(), FILE_H));
        let preview = Rect::from_min_max(
            pos2(main.left(), file.bottom() + GAP),
            pos2(main.right(), (line.top() - 4.0).max(file.bottom() + GAP + 80.0)),
        );

        let modal = self.confirm_delete;
        if self.info.is_ok() && !modal {
            self.keys(&ctx);
        }
        if self.live && self.info.is_ok() {
            let (shown, settled) = self.decode_target(&ctx, preview);
            self.start_media(&ctx, shown, settled);
        }
        self.poll_player(&ctx);
        self.poll_scan(&ctx);
        self.poll_task(&ctx);
        self.poll_upload(&ctx);

        self.file_row(ui, &look, file);
        self.preview(ui, &look, preview);
        self.timeline(ui, &look, line, lane);
        self.hint_row(ui, &look, hint);
        self.panel(ui, &look, side);
        self.menu_popup(&ctx, &look, self.save_rect);
        self.lane_menu_popup(&ctx, &look);
        self.click_menu_popup(&ctx, &look);
        self.key_menu_popup(&ctx, &look);
        self.keys_sheet(&ctx, &look, body);
        self.show_toast(&ctx, &look, body);
        self.delete_dialog(&ctx, &look);

        if esc && !modal {
            if self.renaming.is_some() {
                self.renaming = None;
            } else if self.lane_menu.is_some() {
                self.lane_menu = None;
            } else if self.click_menu.is_some() {
                self.click_menu = None;
            } else if self.key_menu.is_some() {
                self.key_menu = None;
            } else if self.sheet {
                self.sheet = false;
            } else if self.menu {
                self.menu = false;
            } else if self.cropping.is_some() {
                self.toggle_crop();
            } else if !self.busy() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

#[cfg(test)]
mod tests;
