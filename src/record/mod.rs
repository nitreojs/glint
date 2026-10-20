//! Screen recording pipeline.
//!
//! * Capture thread: Desktop Duplication on its own D3D11 device. Frames arrive at the
//!   monitor's refresh rate; the region is cropped on the GPU into a small ring of shared
//!   textures. Nothing else touches this device, so the encoder can never stall capture.
//! * Encoder thread: a second device shared with Media Foundation. It pulls frames from
//!   the ring, draws the cursor, snaps timestamps to the frame grid and feeds the hardware
//!   encoder MFT, which writes the MP4.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
};
use windows::core::Interface;

use crate::platform::log::log;
use crate::platform::rect::Rect;

mod capture;
mod cursor;
mod encoder;
pub(crate) mod indicator;

use capture::Capture;
use encoder::Encoder;

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
    pub adapter_idx: u32,
    pub output_idx: u32,
    /// Capture region in output-local pixels; width and height must be even.
    pub region: Rect,
    /// Stream frame rate: the monitor's exact refresh rate, or a lower cap. Captures that
    /// land in the same 1/fps slot are merged, so a cap simply keeps the latest image.
    pub fps: f64,
    pub codec: Codec,
    /// Constant quantizer: lower = better quality and bigger files.
    pub qp: u32,
    /// Encoder quality-vs-speed knob, 0 (fastest) ..= 100 (best).
    pub effort: u32,
    pub draw_cursor: bool,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub path: PathBuf,
    /// Frames lost because the encoder could not keep up.
    pub dropped: u64,
    pub duration_s: f64,
    /// Capture ended on its own (e.g. resolution change); the file is still valid.
    pub stopped_early: Option<String>,
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
}

/// Moves COM objects created during setup onto the capture thread. D3D11/DXGI objects are
/// free-threaded; after the move only the capture thread touches them.
struct SendCapture(Capture);
unsafe impl Send for SendCapture {}

/// A recorder whose pipeline is initializing; see [`Starting::wait`].
pub(crate) struct Starting {
    ready: mpsc::Receiver<Result<(), String>>,
    go: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
}

impl Starting {
    /// Waits for initialization, then starts capturing. Call it once anything that must not
    /// appear in the video (the region picker) is gone.
    pub(crate) fn wait(self) -> Result<Recorder, String> {
        match self.ready.recv() {
            Ok(Ok(())) => {
                let _ = self.go.send(());
                Ok(Recorder { stop: self.stop, abort: self.abort })
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
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let (go_tx, go_rx) = mpsc::sync_channel::<()>(1);
        let (st, ab) = (stop.clone(), abort.clone());
        thread::Builder::new()
            .name("glint-encode".into())
            .spawn(move || unsafe {
                set_priority(THREAD_PRIORITY_ABOVE_NORMAL);
                let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                let mf = MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
                let path = p.path.clone();
                let setup = Capture::new(&p).and_then(|cap| Encoder::new(p, &cap).map(|enc| (cap, enc)));
                match setup {
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        let _ = ready_tx.send(Err(e));
                    }
                    Ok((cap, enc)) => {
                        let _ = ready_tx.send(Ok(()));
                        let res = if go_rx.recv().is_ok() {
                            run_session(cap, enc, &st, &ab)
                        } else {
                            drop((cap, enc));
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
        Ok(Starting { ready: ready_rx, go: go_tx, stop, abort })
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
}

unsafe fn run_session(cap: Capture, mut enc: Encoder, stop: &Arc<AtomicBool>, abort: &AtomicBool) -> Result<Summary, String> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let (free_tx, free_rx) = mpsc::channel::<usize>();
    let cap = SendCapture(cap);
    let st = stop.clone();
    let capture = thread::Builder::new()
        .name("glint-capture".into())
        .spawn(move || {
            let mut cap = cap;
            unsafe {
                set_priority(THREAD_PRIORITY_HIGHEST);
                let r = cap.0.run(&st, &tx, &free_rx);
                let _ = tx.send(Msg::End { qpc: qpc() });
                r
            }
        })
        .map_err(|e| format!("failed to spawn capture thread: {e}"))?;
    let enc_res = unsafe { enc.run(&rx, &free_tx, abort) };
    if enc_res.is_err() {
        stop.store(true, Ordering::SeqCst);
    }
    drop(rx);
    let cap_res = capture.join().unwrap_or_else(|_| Err("capture thread panicked".into()));
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
    Ok(Summary { path: enc.p.path.clone(), dropped, duration_s, stopped_early })
}

fn set_priority(p: THREAD_PRIORITY) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), p);
    }
}

trait Ctx<T> {
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

unsafe fn adapter_and_output(p: &Params) -> Result<(IDXGIAdapter1, IDXGIOutput1), String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ctx("CreateDXGIFactory1")?;
        let adapter = factory.EnumAdapters1(p.adapter_idx).ctx("EnumAdapters1")?;
        let output = adapter.EnumOutputs(p.output_idx).ctx("EnumOutputs")?.cast().ctx("IDXGIOutput1")?;
        Ok((adapter, output))
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
    /// the cursor moved), `qpc` the capture time, `cursor` the pointer position if visible.
    Frame { slot: Option<usize>, qpc: i64, cursor: Option<(i32, i32)> },
    Shape(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Vec<u8>),
    End { qpc: i64 },
}
