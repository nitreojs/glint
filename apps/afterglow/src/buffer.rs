//! The always-on recording into the replay buffer, and saving clips from it.

use std::path::{Path, PathBuf};

use glint_core::geom::monitor::Output;
use glint_core::geom::rect::Rect;
use glint_core::record::pipeline::{Outcome, Recorder};
use glint_core::record::replay::{Clip, Limits, ReplaySink};
use glint_media::record::{Mp4Sink, Params, Source, start_packets};
use glint_shell::monitors;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::config::Config;
use crate::events::{self, Watcher};

const SECOND: i64 = 10_000_000;

/// The window to tell, sendable to the pipeline's threads.
#[derive(Clone, Copy)]
pub struct Notify {
    hwnd: isize,
    msg: u32,
}

impl Notify {
    pub fn new(hwnd: HWND, msg: u32) -> Notify {
        Notify { hwnd: hwnd.0 as isize, msg }
    }

    /// Posts `value` boxed in LPARAM; the window takes it back with [`take`].
    pub fn post<T>(self, value: T) {
        let raw = Box::into_raw(Box::new(value));
        let hwnd = HWND(self.hwnd as *mut _);
        if unsafe { PostMessageW(Some(hwnd), self.msg, WPARAM(0), LPARAM(raw as isize)) }.is_err() {
            drop(unsafe { Box::from_raw(raw) });
        }
    }
}

/// The value [`Notify::post`] boxed into `lp`.
///
/// # Safety
/// `lp` must come from a `post::<T>` of the same `T`, taken once.
pub unsafe fn take<T>(lp: LPARAM) -> T {
    *unsafe { Box::from_raw(lp.0 as *mut T) }
}

pub struct Buffer {
    recorder: Recorder,
    /// The monitor being kept (desktop coordinates).
    monitor: Rect,
    replay: ReplaySink,
    /// What a save covers (100 ns units).
    length: i64,
    /// Follows games for their events, with `game_events` on.
    events: Option<Watcher>,
}

fn limits(cfg: &Config) -> Limits {
    Limits { keep: cfg.length as i64 * SECOND, max_bytes: (cfg.max_memory_mb as usize) << 20 }
}

/// Monitor `n` (1 = the first Windows lists), or with 0 (or one that's gone) the primary one: the
/// one holding the desktop's origin.
pub fn monitor(n: u32) -> Result<Output, String> {
    let outputs = monitors::enumerate().map_err(|e| format!("can't list monitors: {}", e.message()))?;
    let primary = || outputs.iter().find(|o| o.rect.contains(0, 0)).or(outputs.first());
    let chosen = n.checked_sub(1).and_then(|i| outputs.get(i as usize)).or_else(primary);
    chosen.cloned().ok_or_else(|| "no monitors".into())
}

impl Buffer {
    /// Starts buffering the configured monitor; `ended` hears when the recording stops, as
    /// `(generation, Result<Outcome, String>)`.
    pub fn start(cfg: &Config, generation: u64, ended: Notify) -> Result<Buffer, String> {
        let monitor = monitor(cfg.monitor)?;
        let r = monitor.rect;
        let fps = match cfg.fps {
            0 => monitor.refresh_hz,
            cap => (cap as f64).min(monitor.refresh_hz),
        };
        let params = Params {
            sources: vec![Source { adapter_idx: monitor.adapter_idx, output_idx: monitor.output_idx, desktop: r }],
            region: Rect { w: r.w & !1, h: r.h & !1, ..r },
            fps,
            codec: cfg.codec.codec(),
            qp: cfg.quality,
            // measured at 3440x1440 on an rtx 5060 ti: hevc at 50 manages ~50 fps, at 20 ~350
            effort: 20,
            keyframe_secs: 1.0,
            bitrate: (cfg.bitrate > 0).then(|| cfg.bitrate * 1_000_000),
            draw_cursor: true,
            clicks: None,
            keys: None,
            audio: cfg.audio_sources.clone(),
            track_names: cfg.audio_track_names.clone(),
            // nothing is written there: packets go to the buffer
            path: std::env::temp_dir().join("afterglow-buffer.mp4"),
        };
        let length = cfg.length as i64 * SECOND;
        let replay = ReplaySink::new(limits(cfg));
        let sink = replay.clone();
        let starting = start_packets(params, move || Ok(sink), move |r: Result<Outcome, String>| ended.post((generation, r)))?;
        let recorder = starting.wait()?;
        let events = cfg.game_events.then(|| Watcher::start(cfg.length));
        Ok(Buffer { recorder, monitor: r, replay, length, events })
    }

    /// A new length or memory cap, for what's buffered already; game events on or off.
    pub fn set_limits(&mut self, cfg: &Config) {
        self.length = cfg.length as i64 * SECOND;
        self.replay.lock().set_limits(limits(cfg));
        if cfg.game_events != self.events.is_some() {
            self.events = cfg.game_events.then(|| Watcher::start(cfg.length));
        }
        events::keep(cfg.length);
    }

    pub fn monitor(&self) -> Rect {
        self.monitor
    }

    /// How much the buffer holds now (seconds).
    pub fn held(&self) -> f64 {
        self.replay.lock().held() as f64 / SECOND as f64
    }

    /// The last `length` of the buffer, or `None` while it's empty.
    pub fn clip(&self) -> Option<Clip> {
        self.replay.lock().last(self.length)
    }

    /// What the user should know about how the buffer is recorded (the software encoder).
    pub fn notices(&self) -> &[String] {
        self.recorder.notices()
    }

    pub fn stop(&self) {
        self.recorder.stop();
    }
}

/// Writes `clip` to `dir`/`name`.mp4 (or `name_2.mp4`… when taken) on its own thread, with the
/// game events it covers; `done` hears `id` with the path or the error.
pub fn save(id: u64, clip: Clip, dir: PathBuf, name: PathBuf, done: Notify) {
    std::thread::spawn(move || {
        let result = unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mf = MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
            let r = write(clip, &dir.join(name));
            if mf {
                let _ = MFShutdown();
            }
            if com {
                CoUninitialize();
            }
            r
        };
        done.post((id, result));
    });
}

fn write(clip: Clip, stem: &Path) -> Result<PathBuf, String> {
    if let Some(dir) = stem.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let named = |suffix: &str| {
        let mut p = stem.as_os_str().to_owned();
        p.push(suffix);
        p.push(".mp4");
        PathBuf::from(p)
    };
    let path = std::iter::once(named("")).chain((2..).map(|n| named(&format!("_{n}")))).find(|p| !p.exists());
    let path = path.expect("some number is free");
    // written under another name until it's whole, so a save that dies halfway never looks like a clip
    let mut part = path.clone().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);
    let mut sink = Mp4Sink::create(&part)?;
    let span = clip.wall.map(|wall| (wall, clip.duration));
    clip.write(&mut sink).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    if let Some((wall, duration)) = span {
        events::attach(&part, wall, duration);
    }
    std::fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}
