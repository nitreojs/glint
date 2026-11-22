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
use glint_core::clip::edit::{Edit, Speed, TimeMap, TimedRange, Undo};
use glint_core::clip::export::{Format, Job, Reencode, Source, Video, VideoJob};
use glint_core::clip::glide::Glide;
use glint_core::clip::index::Index;
use glint_core::clip::info::Info;
use glint_core::clip::math::frame_len;
use glint_core::clip::viewer::Viewer;
use glint_core::clip::{export, index, math};
use glint_core::config::{AfterTrim, Config};
use glint_core::gallery::Maker;
use glint_core::history::{self, Entry, Kind as HistoryKind};
use glint_core::icon::icon_rgba;
use glint_core::log::log;
use glint_core::paths::appdata;
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
mod closing;
mod keylane;
mod overlay;
mod panel;
mod player;
mod preview;
mod scan;
mod share;
mod start;
mod timeline;
mod wave;

use actions::Saved;
use player::{Cmd, NO_CLOCK, Player, Shown};
use scan::{Found, Scan};
use clicklane::ClickDrag;
use keylane::KeyDrag;
use timeline::{Drag, Nudge, View};
use wave::Waves;

pub const CLIP_ARG: &str = "--clip";
/// What the editor opens: dropped, picked, or given to `--clip`.
pub const VIDEO_EXTS: [&str; 4] = ["mp4", "mov", "m4v", "mkv"];
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
    glint_ui::gallery::open_editor(&exe, path, false)
}

/// A file probed for the editor.
struct Opened {
    path: PathBuf,
    info: Result<Info, String>,
    index: Option<Index>,
}

/// Reads what the editor needs to know of `path`; the calling thread needs COM.
fn probe(path: PathBuf) -> Opened {
    let info = media::probe(&path);
    if let Err(e) = &info {
        log(&format!("clip: {}: {e}", path.display()));
    }
    let index = info.as_ref().ok().and_then(|_| {
        index::read(&path).map_err(|e| log(&format!("clip: no sample index for {}: {e}", path.display()))).ok()
    });
    Opened { path, info, index }
}

fn is_video(path: &Path) -> bool {
    path.extension().is_some_and(|e| VIDEO_EXTS.iter().any(|v| e.eq_ignore_ascii_case(v)))
}

/// The window's title: the file's name when one is open.
fn title(name: &str) -> String {
    if name.is_empty() { TITLE.to_string() } else { format!("{name} · {TITLE}") }
}

/// Opens the editor on `path`, or on its start screen without one.
pub fn run(path: Option<PathBuf>) {
    let (file, hevc) = std::thread::spawn(move || {
        let _com = Com::init();
        (path.map(probe), media::has_encoder(MFVideoFormat_HEVC))
    })
    .join()
    .unwrap_or((None, false));
    let cfg = Config::load().unwrap_or_default();
    let accent = cfg.accent_rgb();
    let saved = Saved::load();
    let name = file.as_ref().and_then(|f| f.path.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let background = std::env::var_os(glint_ui::gallery::BACKGROUND_EDITOR).is_some_and(|v| v == "1");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title(&name))
            .with_inner_size(WINDOW)
            .with_min_inner_size(MIN_WINDOW)
            .with_resizable(true)
            .with_decorations(false)
            .with_transparent(glint_ui::window::transparent())
            .with_active(!background && !glint_ui::window::quiet())
            .with_drag_and_drop(true)
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
            if let Some(h) = hwnd {
                glint_ui::window::frameless(h, true);
            }
            if let (Some(h), Some(r)) = (hwnd, saved.rect) {
                glint_ui::window::place(h, r);
            }
            let mut app = ClipApp::new(file, cfg.effort, cfg.after_trim, cfg.upload_destination.trim().to_string(), saved, accent);
            (app.hwnd, app.live, app.hevc) = (hwnd, true, hevc);
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

struct Playback {
    pieces: Arc<Vec<TimedRange>>,
    audio_epoch: u64,
    index: usize,
    offset: i64,
    elapsed: i64,
}

impl Playback {
    fn from_source(mut pieces: Vec<TimedRange>, source: i64) -> Vec<TimedRange> {
        let first = pieces.partition_point(|p| p.range.end <= source);
        pieces.drain(..first);
        if let Some(piece) = pieces.first_mut() {
            piece.range.start = piece.range.start.max(source);
        }

        pieces
    }

    fn new(pieces: Vec<TimedRange>) -> Option<Self> {
        if pieces.is_empty() {
            return None;
        }

        Some(Self { pieces: Arc::new(pieces), audio_epoch: 0, index: 0, offset: 0, elapsed: 0 })
    }

    fn matches(&self, source: i64, pieces: &[TimedRange]) -> bool {
        let [first, rest @ ..] = pieces else { return false };
        let current = self.pieces[self.index];

        first.range.start == source
            && first.range.end == current.range.end
            && first.speed == current.speed
            && rest == &self.pieces[self.index + 1..]
    }

    fn advance(&mut self, elapsed: i64) -> (i64, bool, bool) {
        self.elapsed = self.elapsed.max(elapsed);
        let mut skipped = false;
        loop {
            let piece = self.pieces[self.index];
            let len = piece.len();
            let within = self.elapsed - self.offset;
            if within < len {
                let source = piece.range.start + piece.speed.source_delta(within);

                return (source.min(piece.range.end - 1), skipped, false);
            }

            let Some(next) = self.pieces.get(self.index + 1) else {
                return (piece.range.end, skipped, true);
            };
            skipped |= next.range.start != piece.range.end;
            self.offset += len;
            self.index += 1;
        }
    }
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
    /// The exact edit being saved, only when original cleanup is enabled.
    edit: Option<Edit>,
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

enum Note {
    Saved,
    Uploaded { url: String, copied: bool },
    Copied,
    Info(String),
    Failed(String),
    /// Another file opened over this one's unsaved edits, which "back" brings back.
    Left(String),
}

struct Toast {
    note: Note,
    at: f64,
    held: bool,
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
    after_trim: AfterTrim,
    trimmed: Vec<closing::Trimmed>,
    confirm_cleanup: Option<Vec<PathBuf>>,
    closing: bool,
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
    overview: Vec<(i64, TextureHandle)>,
    tex: Option<TextureHandle>,
    image: Option<Arc<ColorImage>>,
    nearest: bool,
    epoch: u64,
    queue: VecDeque<Shown>,
    shown: Option<(i64, i64)>,
    pos: i64,
    play: Play,
    playback: Option<Playback>,
    edit: Edit,
    undo: Undo<Edit>,
    before_drag: Option<Edit>,
    selected: Option<Selected>,
    view: View,
    reset_motion: Option<timeline::ResetMotion>,
    refit: Option<(View, View, f64)>,
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
    last_export: Option<share::SavedFile>,
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
    /// The lane whose volume slider is open, under the volume pill at that rect.
    volume_pop: Option<(usize, Rect)>,
    nudge: Option<Nudge>,
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
    light: Option<bool>,
    accent: [u8; 3],
    host: String,
    now: f64,
    /// No file open: the start screen.
    start: Option<start::Start>,
    /// A file being read to open next.
    opening: Option<start::Opening>,
    /// The file left for another with edits not saved, and those edits.
    previous: Option<(PathBuf, Edit)>,
    /// The edit as the file opened, to tell whether anything was changed.
    pristine: Edit,
}

fn fit(src: (u32, u32), area: egui::Vec2) -> egui::Vec2 {
    let (w, h) = (src.0.max(1) as f32, src.1.max(1) as f32);
    let s = (area.x / w).min(area.y / h);
    vec2(w * s, h * s)
}

fn decode_size(info: &Info, px: egui::Vec2, fitted: bool) -> (u32, u32) {
    let scaled = fit(info.size, if fitted { px } else { px * 2.0 });
    if scaled.x >= info.size.0 as f32 {
        info.size
    } else {
        ((scaled.x.round() as u32).max(2) & !1, (scaled.y.round() as u32).max(2) & !1)
    }
}

impl ClipApp {
    /// The editor on `file`, or on the start screen without one.
    fn new(file: Option<Opened>, effort: u32, after_trim: AfterTrim, host: String, saved: Saved, accent: [u8; 3]) -> Self {
        let start = file.is_none().then(start::Start::default);
        let Opened { path, info, index } = file.unwrap_or(Opened { path: PathBuf::new(), info: Err(String::new()), index: None });
        let duration = info.as_ref().map_or(0, |i| i.duration);
        let tracks = info.as_ref().map_or(0, |i| i.tracks.len());
        let clicks = info.as_ref().ok().and_then(|i| i.clicks.as_ref()).map(Clicks::new);
        let keys = info.as_ref().ok().and_then(|i| i.keys.as_ref()).map(Keys::new);
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let file_bytes = std::fs::metadata(&path).map_or(0, |m| m.len());
        let edit = Edit { clicks, keys, ..Edit::new(duration, tracks) };
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
            effort,
            after_trim,
            trimmed: Vec::new(),
            confirm_cleanup: None,
            closing: false,
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
            overview: Vec::new(),
            tex: None,
            image: None,
            nearest: false,
            epoch: 0,
            queue: VecDeque::new(),
            shown: None,
            pos: 0,
            play: Play::Paused,
            playback: None,
            pristine: edit.clone(),
            edit,
            undo: Undo::default(),
            before_drag: None,
            selected: None,
            view: View::full(duration),
            reset_motion: None,
            refit: None,
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
            last_export: None,
            toast: None,
            flash: f64::NEG_INFINITY,
            gone: false,
            lane_menu: None,
            renaming: None,
            lane_drag: None,
            volume_pop: None,
            nudge: None,
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
            light: None,
            accent,
            host,
            now: 0.0,
            start,
            opening: None,
            previous: None,
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

    /// What saving would write right now, and why its picture would be made anew (nothing: it's
    /// copied as recorded).
    fn plan(&self) -> Option<(Job, Vec<Reencode>)> {
        let src = self.source()?;
        Some(export::plan_why(&self.edit, &self.saved.export, &src))
    }

    /// What saving would write right now.
    fn job(&self) -> Option<Job> {
        self.plan().map(|(job, _)| job)
    }

    fn sync_gains(&self) {
        let Some(a) = &self.audio else { return };
        a.gains.set_volume(self.saved.volume);
        for k in 0..self.info().map_or(0, |i| i.tracks.len()) {
            let heard = self.edit.lanes.iter().filter(|l| l.source == Some(k) && !l.muted);
            a.gains.set_track(k, heard.map(|l| l.gain()).sum());
        }
    }

    fn busy(&self) -> bool {
        self.task.is_some() || self.upload.is_some()
    }

    fn seek(&mut self, t: i64, play: bool) {
        let playback = play.then(|| Playback::from_source(self.edit.timed(), t)).and_then(Playback::new);
        self.seek_with(t, playback);
    }

    fn seek_with(&mut self, t: i64, mut playback: Option<Playback>) {
        let t = playback.as_ref().map_or(t, |p| p.pieces[0].range.start);
        let playing = playback.is_some();
        let was_playing = self.play != Play::Paused;
        self.seek_video(t, playing, NO_CLOCK);
        if let Some(playback) = &mut playback {
            self.pos = t;
            playback.audio_epoch = self.epoch;
        }
        self.play = if playing { Play::Starting } else { Play::Paused };
        if let Some(a) = &self.audio {
            if let Some(playback) = &playback {
                a.prepare(playback.pieces.clone(), playback.audio_epoch);
            } else if was_playing {
                a.stop();
            }
        }
        self.playback = playback;
    }

    fn seek_video(&mut self, t: i64, play: bool, clock: i64) {
        self.epoch += 1;
        self.queue.clear();
        if let Some(p) = &self.player {
            p.send(Cmd::Seek { t, epoch: self.epoch, play, clock });
        }
    }

    fn refresh_playback(&mut self) {
        let Some(playback) = &self.playback else { return };
        let pieces = Playback::from_source(self.edit.timed(), self.pos);
        if !playback.matches(self.pos, &pieces) {
            self.seek_with(self.pos, Playback::new(pieces));
        }
    }

    fn jump(&mut self, t: i64) {
        self.pos = t;
        let playing = self.play != Play::Paused;
        self.seek(t, playing);
        self.reveal(self.pos);
    }

    fn pause(&mut self) {
        if self.play != Play::Paused {
            self.play = Play::Paused;
            self.playback = None;
            self.epoch += 1;
            self.queue.clear();
            if let Some(p) = &self.player {
                p.send(Cmd::Pause);
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
        let pieces = self.edit.timed();
        let near_end = pieces.last().is_some_and(|p| self.pos >= p.range.end - self.frame_len());
        let start = if near_end { pieces[0].range.start } else { self.pos };
        self.seek_with(start, Playback::new(Playback::from_source(pieces, start)));
    }

    fn step(&mut self, forward: bool) {
        let Some((ts, dur)) = self.shown else { return };
        let t = if forward { ts + if dur > 0 { dur } else { self.frame_len() } } else { ts - 1 };
        if t < 0 || t >= self.duration() {
            return;
        }
        self.pos = t;
        self.seek(t, false);
        self.reveal(t);
    }

    fn reveal(&mut self, source: i64) {
        let map = TimeMap::new(&self.edit);
        self.view.reveal(map.to_edit(source), map.duration());
    }

    fn change(&mut self, f: impl FnOnce(&mut Edit) -> bool) -> bool {
        self.settle_volume(true);
        let before = self.edit.clone();
        let changed = f(&mut self.edit);
        if self.playback.is_some() && (before.range != self.edit.range || before.segments != self.edit.segments) {
            self.refresh_playback();
        }
        self.undo.record(before, &self.edit);

        changed
    }

    pub(super) fn reset_piece_speeds(&mut self) -> bool {
        self.change(|e| {
            let mut changed = false;
            for piece in &mut e.segments {
                changed |= piece.speed != Speed::NORMAL;
                piece.speed = Speed::NORMAL;
            }

            changed
        })
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
        let Some(info) = self.info() else { return };
        let want = decode_size(info, shown_px, self.viewer.fitted);
        let (size, duration) = (info.size, info.duration);
        let resized = (want.0 as f32 - self.player_size.0 as f32).abs() > self.player_size.0 as f32 * 0.1;
        let settled = settled && !ctx.input(|i| i.pointer.any_down());
        let first = self.player.is_none();
        if self.audio.is_none() && !info.tracks.is_empty() {
            let streams = info.tracks.iter().map(|t| t.stream).collect();
            self.audio = Some(Audio::start(self.path.clone(), streams, self.saved.volume));
            self.sync_gains();
        }
        if first || (resized && settled) {
            self.player = None;
            self.player_size = want;
            self.player = Some(Player::start(self.path.clone(), want, ctx.clone()));
            match self.play {
                Play::Running { .. } => self.seek_video(self.pos, true, self.pos),
                Play::Starting if !first => self.seek_video(self.pos, true, NO_CLOCK),
                _ => self.seek(self.pos, self.play != Play::Paused),
            }
        }
        if self.tex.is_none() && self.player_error.is_none() {
            return;
        }

        if self.waves.is_none() && let Ok(info) = &self.info && !info.tracks.is_empty() {
            let streams = info.tracks.iter().map(|t| t.stream).collect();
            self.waves = Some(Waves::start(self.path.clone(), streams, duration, ctx.clone()));
        }
        if self.scan.is_none() {
            self.cuts.clear();
            self.thumbs.clear();
            self.overview.clear();
            let keys = self.index.as_ref().map_or(&[][..], |i| i.video.keys.as_slice());
            self.scan = Some(Scan::start(self.path.clone(), size, duration, keys, ctx.clone()));
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
                Found::Overview(t, image) => {
                    if let Err(at) = self.overview.binary_search_by_key(&t, |(ts, _)| *ts) {
                        let tex = ctx.load_texture(format!("clip-overview-{t}"), image, TextureOptions::LINEAR);
                        self.overview.insert(at, (t, tex));
                    }
                }
            }
        }
    }

    fn clock(&mut self, at: Instant, from: i64) -> i64 {
        let now = Instant::now();
        let Some(playback) = &self.playback else { return from };
        let audio = self.audio.as_ref().map_or(Clock::Wall, |a| a.clock(playback.audio_epoch));
        let elapsed = playback.elapsed;
        match audio {
            Clock::At(t) => {
                let from = t.max(elapsed);
                self.play = Play::Running { at: now, from };

                from
            }
            Clock::Waiting => {
                self.play = Play::Running { at: now, from: elapsed };

                elapsed
            }
            Clock::Wall => from + (now.duration_since(at).as_nanos() / 100) as i64,
        }
    }

    fn texture_options(&self) -> TextureOptions {
        if self.nearest { TextureOptions::NEAREST } else { TextureOptions { mipmap_mode: Some(TextureFilter::Linear), ..TextureOptions::LINEAR } }
    }

    fn poll_player(&mut self, ctx: &egui::Context) -> Option<i64> {
        let p = self.player.as_ref()?;
        while self.queue.len() < player::AHEAD
            && let Some(got) = p.poll()
        {
            match got {
                Ok(s) if s.epoch == self.epoch => self.queue.push_back(s),
                Ok(_) => {}
                Err(e) => self.player_error = Some(e),
            }
        }

        if self.play == Play::Starting
            && !self.queue.is_empty()
            && let Some(playback) = &self.playback
        {
            if let Some(a) = &self.audio {
                a.play(playback.audio_epoch);
            }
            self.play = Play::Running { at: Instant::now(), from: 0 };
        }
        let mut done = false;
        let (now, follow) = match self.play {
            Play::Running { at, from } => {
                let elapsed = self.clock(at, from);
                let playback = self.playback.as_mut()?;
                let (source, skipped, finished) = playback.advance(elapsed);
                done = finished;
                if skipped {
                    self.playhead.set(source);
                    if done {
                        self.queue.clear();
                    } else {
                        self.seek_video(source, true, source);
                    }
                } else if let Some(p) = &self.player {
                    p.set_clock(source);
                }
                self.pos = source;

                (if done { source - 1 } else { source }, Some(source))
            }
            Play::Starting => (i64::MIN, None),
            Play::Paused => (i64::MAX, None),
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
            if self.image.as_ref().is_none_or(|image| !Arc::ptr_eq(image, &s.image)) {
                let options = self.texture_options();
                match &mut self.tex {
                    Some(t) => t.set(s.image.clone(), options),
                    None => self.tex = Some(ctx.load_texture("clip-frame", s.image.clone(), options)),
                }
                self.image = Some(s.image);
            }
        }
        if done {
            self.pause();
        } else if matches!(self.play, Play::Running { .. }) {
            ctx.request_repaint();
        }

        follow
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
        self.toast = Some(Toast { note, at: self.now, held: false });
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
        if whole && dest.is_none() {
            match then {
                Then::Nothing => {}
                Then::Copy => return self.toast(share::copy_file(self.hwnd, &self.path)),
                Then::Upload => return self.start_upload(self.path.clone()),
            }
        }
        let gif = matches!(job, Job::Gif(_));
        let compressing = matches!(&job, Job::Video(VideoJob { video: Video::Encode { .. }, .. }));
        let before = self.source().filter(|_| compressing).map(|s| {
            let source_len = self.edit.kept().iter().map(|r| r.len()).sum();

            s.bytes_for(source_len)
        });
        let out = match &dest {
            Some(d) => {
                let ext = d.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
                math::unique_sibling(d, ".part", &ext)
            }
            None => self.default_out(gif),
        };
        self.forget_trim();
        let edit = (self.after_trim != AfterTrim::KeepBoth && !whole).then(|| self.edit.clone());
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
        self.task = Some(Task { out, dest, then, copy: whole, edit, before, permille, cancel, thread: Some(thread) });
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
                if !task.cancel.load(Ordering::Relaxed)
                    && let Some(edit) = task.edit
                {
                    self.remember_trim(&path, edit);
                }
                let size = std::fs::metadata(&path).ok().map(|m| m.len());
                // the export shows what the clip it's cut from shows, whichever app saved that
                let histories = Maker::ALL.map(|m| m.history(&appdata()));
                let from = history::find_in(&histories, &self.path);
                let from_trimmed = from.as_ref().and_then(|entry| entry.trimmed);
                let (app, exe) = from.map_or((None, None), |e| (e.app, e.exe));
                let mut entry = Entry::new(HistoryKind::Recording, Some(&path), size.unwrap_or_default())
                    .with_app(app.as_deref(), exe.as_deref());
                entry.trimmed = if task.copy { from_trimmed } else { Some(true) };
                history::add(entry.with_duration(glint_ui::gallery::length(&path)));
                self.finish_export(path, task.then, size, task.before);
            }
            Err(e) => {
                log(&format!("clip: exporting {} failed: {e}", self.path.display()));
                self.toast(Note::Failed(format!("couldn't save: {e}")));
            }
        }
    }

    fn finish_export(&mut self, path: PathBuf, then: Then, size: Option<u64>, before: Option<u64>) {
        let file = share::SavedFile::new(path, size, before);
        match then {
            Then::Nothing => {
                self.flash = self.now;
                self.toast(Note::Saved);
            }
            Then::Copy => self.toast(share::copy_file(self.hwnd, &file.path)),
            Then::Upload => self.start_upload(file.path.clone()),
        }
        self.last_export = Some(file);
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
                history::add(Entry::new(HistoryKind::Recording, Some(&up.path), up.size).with_url(&url));
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
        self.release_media();
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

    fn toggle_piece(&mut self, i: usize) {
        if !self.change(|e| e.toggle_removed(i)) {
            self.toast(Note::Info("at least one piece has to stay".into()));
        }
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
            self.toast(Note::Info(format!("split at {} · right-click a piece to remove it", math::fmt_time(pos))));
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
                Some(Selected::Segment(i)) => self.toggle_piece(i),
                _ => {}
            }
        }
        if undo || redo {
            self.settle_volume(true);
        }
        if redo && self.undo.redo(&mut self.edit) || undo && self.undo.undo(&mut self.edit) {
            self.selected = None;
            self.lane_menu = None;
            self.renaming = None;
            self.lane_drag = None;
            self.volume_pop = None;
            self.click_menu = None;
            self.click_sel.clear();
            self.key_menu = None;
            self.key_sel.clear();
            self.sync_gains();
            self.refresh_playback();
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
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        glint_ui::window::closing();
    }

    /// Past the window's painted corners is the desktop.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_close(&ctx);
        }
        glint_ui::window::shots(&ctx);
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
        let radius = glint_ui::window::radius(&ctx);
        let esc = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));

        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.surf);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, "glint", "clip", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => self.request_close(&ctx),
            TitleAction::None => {}
        }
        ui.painter().hline(bar.x_range(), bar.bottom() - 0.5, egui::Stroke::new(1.0, p.line));

        let body = Rect::from_min_max(pos2(full.left(), bar.bottom()), full.max);
        let held = self.files(&ctx);
        self.poll_opening(&ctx);
        if !self.confirm_delete && self.confirm_cleanup.is_none() && ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.browse();
        }
        if self.start.is_some() {
            self.start_screen(ui, &look, body, held.as_ref());
            glint_ui::window::border(ui, full, p.line2);
            self.show_toast(&ctx, &look, body);
            if esc {
                self.request_close(&ctx);
            }
            return;
        }
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

        let modal = self.confirm_delete || self.confirm_cleanup.is_some();
        if self.info.is_ok() && !modal {
            self.keys(&ctx);
        }
        if self.live && self.info.is_ok() && self.confirm_cleanup.is_none() && !self.closing {
            let (shown, settled) = self.decode_target(&ctx, preview);
            self.start_media(&ctx, shown, settled);
        }
        let follow = self.poll_player(&ctx);
        self.poll_scan(&ctx);
        self.poll_task(&ctx);
        self.poll_upload(&ctx);

        self.file_row(ui, &look, file);
        self.preview(ui, &look, preview);
        self.timeline(ui, &look, line, lane, follow);
        self.hint_row(ui, &look, hint);
        self.panel(ui, &look, side);
        glint_ui::window::border(ui, full, p.line2);
        self.menu_popup(&ctx, &look, self.save_rect);
        self.lane_menu_popup(&ctx, &look);
        self.volume_popup(&ctx, &look);
        self.click_menu_popup(&ctx, &look);
        self.key_menu_popup(&ctx, &look);
        self.keys_sheet(&ctx, &look, body);
        self.drop_overlay(&ctx, &look, body, held.as_ref());
        self.show_toast(&ctx, &look, body);
        self.delete_dialog(&ctx, &look);
        self.cleanup_dialog(&ctx, &look);

        if esc && !modal {
            if self.renaming.is_some() {
                self.renaming = None;
            } else if self.lane_menu.is_some() {
                self.lane_menu = None;
            } else if self.volume_pop.is_some() {
                self.volume_pop = None;
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
                self.request_close(&ctx);
            }
        }
    }
}

#[cfg(test)]
mod playback_tests;
#[cfg(test)]
mod preview_media_tests;

#[cfg(test)]
mod tests;
