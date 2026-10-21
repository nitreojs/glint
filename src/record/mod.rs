//! Screen recording pipeline.
//!
//! * Capture thread: Desktop Duplication of every monitor the region overlaps, on its own
//!   D3D11 device. Frames arrive at the monitors' refresh rates; the region is cropped (and,
//!   across monitors, composed) on the GPU into a small ring of shared textures. Nothing else
//!   touches this device, so the encoder can never stall capture.
//! * Encoder thread: a second device shared with Media Foundation. It pulls frames from
//!   the ring, draws the cursor, snaps timestamps to the frame grid and feeds the hardware
//!   encoder MFT, which writes the MP4.
//! * Audio thread (when enabled): WASAPI loopback, per-app process loopback and microphones,
//!   placed on the capture clock and mixed into up to six 48 kHz stereo tracks that the
//!   encoder writes as AAC streams.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::thread;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
};

use crate::config::audio::AudioSource;
use crate::platform::log::log;
use crate::platform::rect::Rect;

mod apps;
mod audio;
mod capture;
mod cursor;
mod encoder;
pub(crate) mod indicator;
mod mix;
pub(crate) mod mp4;
mod rotate;
mod timeline;
pub(crate) mod tracks;

pub(crate) use apps::{App, running_apps};
pub(crate) use audio::{Microphone, microphones};
use capture::Capture;
use encoder::Encoder;
use timeline::Pauses;

/// Shared textures between capture and encoder; capture drops frames only if all are busy.
const RING: usize = 6;
const INFINITE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    H264,
    Hevc,
}

impl Codec {
    /// Smallest frame (even sizes) the NVIDIA encoder accepts, measured on an RTX 5060 Ti:
    /// anything smaller fails in SetInputMediaType with MF_E_INVALIDMEDIATYPE.
    pub(crate) fn min_size(self) -> (i32, i32) {
        match self {
            Codec::H264 => (34, 34),
            Codec::Hevc => (130, 34),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Params {
    /// Every output the region overlaps, all on one adapter.
    pub sources: Vec<Source>,
    /// Capture region in desktop pixels; width and height must be even. Parts no source
    /// covers stay black.
    pub region: Rect,
    /// Stream frame rate: the fastest source's exact refresh rate, or a lower cap. Captures
    /// that land in the same 1/fps slot are merged, so a cap simply keeps the latest image.
    pub fps: f64,
    pub codec: Codec,
    /// Constant quantizer: lower = better quality and bigger files.
    pub qp: u32,
    /// Encoder quality-vs-speed knob, 0 (fastest) ..= 100 (best).
    pub effort: u32,
    pub draw_cursor: bool,
    pub audio: Vec<AudioSource>,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Source {
    pub adapter_idx: u32,
    pub output_idx: u32,
    /// The output's desktop rectangle when the region was picked.
    pub desktop: Rect,
}

#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub path: PathBuf,
    /// Frames lost because the encoder could not keep up.
    pub dropped: u64,
    pub duration_s: f64,
    /// Capture ended on its own (e.g. resolution change); the file is still valid.
    pub stopped_early: Option<String>,
    pub tracks: Vec<String>,
    pub audio_issues: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Saved(Summary),
    /// [`Recorder::abort`] was called; the file has been deleted.
    Discarded,
}

pub(crate) struct Recorder {
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    pauses: Arc<Mutex<Pauses>>,
}

/// Moves COM objects created during setup onto the capture and audio threads. D3D11/DXGI and
/// WASAPI objects are free-threaded; after the move only that thread touches them.
struct SendCapture(Capture);
unsafe impl Send for SendCapture {}
struct SendAudio(Vec<audio::Source>, Vec<Vec<usize>>);
unsafe impl Send for SendAudio {}

/// A recorder whose pipeline is initializing; see [`Starting::wait`].
pub(crate) struct Starting {
    ready: mpsc::Receiver<Result<(), String>>,
    go: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    pauses: Arc<Mutex<Pauses>>,
}

impl Starting {
    /// Waits for initialization, then starts capturing. Call it once anything that must not
    /// appear in the video (the region picker) is gone.
    pub(crate) fn wait(self) -> Result<Recorder, String> {
        match self.ready.recv() {
            Ok(Ok(())) => {
                let _ = self.go.send(());
                Ok(Recorder { stop: self.stop, abort: self.abort, pauses: self.pauses })
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err("encoder thread exited during startup".into()),
        }
    }
}

impl Recorder {
    /// Initializes the capture/encode pipeline in the background. `on_done` runs on the
    /// encoder thread once the recording ends (on failure the partial file is deleted).
    pub(crate) fn start(p: Params, on_done: impl FnOnce(Result<Outcome, String>) + Send + 'static) -> Result<Starting, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let abort = Arc::new(AtomicBool::new(false));
        let pauses = Arc::new(Mutex::new(Pauses::default()));
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let (go_tx, go_rx) = mpsc::sync_channel::<()>(1);
        let (st, ab, ps) = (stop.clone(), abort.clone(), pauses.clone());
        thread::Builder::new()
            .name("glint-encode".into())
            .spawn(move || unsafe {
                set_priority(THREAD_PRIORITY_ABOVE_NORMAL);
                let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                let mf = MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
                let path = p.path.clone();
                let setup = Capture::new(&p).and_then(|cap| {
                    let opened = audio::open_all(&p.audio);
                    let names: Vec<String> = opened.tracks.iter().map(|t| t.name.clone()).collect();
                    let routes = opened.tracks.into_iter().map(|t| t.sources).collect();
                    Encoder::new(p, &cap, names.clone())
                        .map(|enc| (cap, enc, SendAudio(opened.sources, routes), (names, opened.issues)))
                });
                match setup {
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        let _ = ready_tx.send(Err(e));
                    }
                    Ok((cap, enc, devices, audio)) => {
                        let _ = ready_tx.send(Ok(()));
                        let res = if go_rx.recv().is_ok() {
                            run_session(cap, enc, devices, audio, &st, &ab, &ps)
                        } else {
                            drop((cap, enc, devices));
                            Err("recording was cancelled before it started".into())
                        };
                        let discarded = ab.load(Ordering::SeqCst);
                        if res.is_err() || discarded {
                            let _ = std::fs::remove_file(&path);
                        }
                        on_done(if discarded { Ok(Outcome::Discarded) } else { res.map(Outcome::Saved) });
                    }
                }
                if mf {
                    let _ = MFShutdown();
                }
                if com {
                    CoUninitialize();
                }
            })
            .map_err(|e| format!("failed to spawn encoder thread: {e}"))?;
        Ok(Starting { ready: ready_rx, go: go_tx, stop, abort, pauses })
    }

    /// Requests a graceful stop; `on_done` fires once the MP4 is finalized.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stops without saving: the file is not finalized and gets deleted.
    pub(crate) fn abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }

    pub(crate) fn pause(&self) {
        lock(&self.pauses).pause(hns(qpc()));
    }

    pub(crate) fn resume(&self) {
        lock(&self.pauses).resume(hns(qpc()));
    }

    pub(crate) fn paused(&self) -> bool {
        lock(&self.pauses).paused()
    }
}

fn lock(m: &Mutex<Pauses>) -> MutexGuard<'_, Pauses> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

unsafe fn run_session(
    cap: Capture,
    mut enc: Encoder,
    audio: SendAudio,
    (tracks, mut audio_issues): (Vec<String>, Vec<String>),
    stop: &Arc<AtomicBool>,
    abort: &AtomicBool,
    pauses: &Mutex<Pauses>,
) -> Result<Summary, String> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (free_tx, free_rx) = mpsc::channel::<usize>();
    let mixer = if audio.0.is_empty() {
        None
    } else {
        let (st, tx) = (stop.clone(), tx.clone());
        let spawned = thread::Builder::new().name("glint-audio".into()).spawn(move || {
            let audio = audio;
            unsafe {
                set_priority(THREAD_PRIORITY_HIGHEST);
                let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                let issues = audio::run(audio.0, &audio.1, &st, &tx);
                if com {
                    CoUninitialize();
                }
                issues
            }
        });
        Some(spawned.map_err(|e| format!("failed to spawn audio thread: {e}"))?)
    };
    let cap = SendCapture(cap);
    let st = stop.clone();
    let capture = thread::Builder::new()
        .name("glint-capture".into())
        .spawn(move || {
            let mut cap = cap;
            unsafe {
                set_priority(THREAD_PRIORITY_HIGHEST);
                let r = cap.0.run(&st, &tx, &free_rx);
                st.store(true, Ordering::SeqCst);
                let _ = tx.send(Msg::End { qpc: qpc() });
                r
            }
        })
        .map_err(|e| {
            stop.store(true, Ordering::SeqCst);
            format!("failed to spawn capture thread: {e}")
        })?;
    let enc_res = unsafe { enc.run(&rx, &free_tx, abort, pauses) };
    if enc_res.is_err() {
        stop.store(true, Ordering::SeqCst);
    }
    drop(rx);
    let cap_res = capture.join().unwrap_or_else(|_| Err("capture thread panicked".into()));
    if let Some(m) = mixer {
        audio_issues.extend(m.join().unwrap_or_else(|_| vec!["audio thread panicked".into()]));
    }
    let (frames, enc_dropped, duration_s) = enc_res?;
    let (cap_dropped, coalesced, stopped_early) = match cap_res {
        Ok(s) => (s.dropped, s.coalesced, None),
        Err(e) => (0, 0, Some(e)),
    };
    let dropped = enc_dropped + cap_dropped;
    log(&format!(
        "finished: {frames} frames, {dropped} dropped, {coalesced} coalesced by DDA, {duration_s:.2}s{}",
        stopped_early.as_deref().map(|e| format!(", stopped early: {e}")).unwrap_or_default()
    ));
    Ok(Summary { path: enc.p.path.clone(), dropped, duration_s, stopped_early, tracks, audio_issues })
}

fn set_priority(p: THREAD_PRIORITY) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), p);
    }
}

pub(crate) trait Ctx<T> {
    fn ctx(self, what: &str) -> Result<T, String>;
}
impl<T> Ctx<T> for windows::core::Result<T> {
    fn ctx(self, what: &str) -> Result<T, String> {
        self.map_err(|e| format!("{what}: {} (0x{:08X})", e.message(), e.code().0 as u32))
    }
}

fn qpc() -> i64 {
    let mut v = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v
}

fn hns(qpc: i64) -> i64 {
    static FREQ: LazyLock<i64> = LazyLock::new(|| {
        let mut f = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    });
    (qpc as i128 * 10_000_000 / *FREQ as i128) as i64
}

unsafe fn create_device(adapter: &IDXGIAdapter1, flags: D3D11_CREATE_DEVICE_FLAG) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let mut device = None;
    let mut ctx = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            flags | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .ctx("D3D11CreateDevice")?;
    }
    Ok((device.unwrap(), ctx.unwrap()))
}

unsafe fn adapter(idx: u32) -> Result<IDXGIAdapter1, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ctx("CreateDXGIFactory1")?;
        factory.EnumAdapters1(idx).ctx("EnumAdapters1")
    }
}

fn texture_desc(r: Rect, bind: D3D11_BIND_FLAG, misc: D3D11_RESOURCE_MISC_FLAG) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: r.w as u32,
        Height: r.h as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: misc.0 as u32,
    }
}

enum Msg {
    /// A new state of the region: `slot` is a ring texture with a fresh image (None when only
    /// the cursor moved), `qpc` the capture time, `cursor` the pointer's desktop position if visible.
    Frame { slot: Option<usize>, qpc: i64, cursor: Option<(i32, i32)> },
    Shape(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Vec<u8>),
    End { qpc: i64 },
    /// `start`: capture time of the first frame in 100 ns units.
    Audio { start: i64, tracks: Vec<Vec<i16>> },
    AudioEnd,
}
