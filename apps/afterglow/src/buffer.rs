//! The always-on recording into the replay buffer, and saving clips from it.

use std::path::{Path, PathBuf};

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
    replay: ReplaySink,
    /// What a save covers (100 ns units).
    length: i64,
}

/// The primary monitor: the one holding the desktop's origin.
fn primary() -> Result<glint_core::geom::monitor::Output, String> {
    let outputs = monitors::enumerate().map_err(|e| format!("can't list monitors: {}", e.message()))?;
    outputs.iter().find(|o| o.rect.contains(0, 0)).or(outputs.first()).cloned().ok_or_else(|| "no monitors".into())
}

impl Buffer {
    /// Starts buffering the primary monitor; `ended` hears when the recording stops, as
    /// `(generation, Result<Outcome, String>)`.
    pub fn start(cfg: &Config, generation: u64, ended: Notify) -> Result<Buffer, String> {
        let monitor = primary()?;
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
            effort: 50,
            keyframe_secs: 1.0,
            draw_cursor: true,
            clicks: None,
            keys: None,
            audio: cfg.audio_sources.clone(),
            track_names: Vec::new(),
            // nothing is written there: packets go to the buffer
            path: std::env::temp_dir().join("afterglow-buffer.mp4"),
        };
        let length = cfg.length as i64 * SECOND;
        let replay = ReplaySink::new(Limits { keep: length, max_bytes: (cfg.max_memory_mb as usize) << 20 });
        let sink = replay.clone();
        let starting = start_packets(params, move || Ok(sink), move |r: Result<Outcome, String>| ended.post((generation, r)))?;
        let recorder = starting.wait()?;
        Ok(Buffer { recorder, replay, length })
    }

    /// How much the buffer holds now (seconds).
    pub fn held(&self) -> f64 {
        self.replay.lock().held() as f64 / SECOND as f64
    }

    /// The last `length` of the buffer, or `None` while it's empty.
    pub fn clip(&self) -> Option<Clip> {
        self.replay.lock().last(self.length)
    }

    pub fn stop(&self) {
        self.recorder.stop();
    }
}

/// Writes `clip` to a new file in `dir` on its own thread; `done` hears the path or the error.
pub fn save(clip: Clip, dir: PathBuf, done: Notify) {
    std::thread::spawn(move || {
        let result = unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mf = MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
            let r = write(clip, &dir);
            if mf {
                let _ = MFShutdown();
            }
            if com {
                CoUninitialize();
            }
            r
        };
        done.post(result);
    });
}

fn write(clip: Clip, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (date, time) = glint_shell::time::date_time();
    let mut path = dir.join(format!("afterglow_{date}_{time}.mp4"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("afterglow_{date}_{time}_{n}.mp4"));
    }
    let mut sink = Mp4Sink::create(&path)?;
    clip.write(&mut sink).inspect_err(|_| {
        let _ = std::fs::remove_file(&path);
    })?;
    Ok(path)
}
