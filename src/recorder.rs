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
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use windows::Win32::Foundation::{E_ACCESSDENIED, HANDLE, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use crate::cursor::CursorRenderer;
use crate::util::{Rect, log};

/// Shared textures between capture and encoder; capture drops frames only if all are busy.
const RING: usize = 6;
const INFINITE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

#[derive(Clone, Debug)]
pub struct Params {
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
pub struct Summary {
    pub path: PathBuf,
    /// Frames lost because the encoder could not keep up.
    pub dropped: u64,
    pub duration_s: f64,
    /// Capture ended on its own (e.g. resolution change); the file is still valid.
    pub stopped_early: Option<String>,
}

#[derive(Debug)]
pub enum Outcome {
    Saved(Summary),
    /// [`Recorder::abort`] was called; the file has been deleted.
    Discarded,
}

pub struct Recorder {
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
}

/// Moves COM objects created during setup onto the capture thread. D3D11/DXGI objects are
/// free-threaded; after the move only the capture thread touches them.
struct SendCapture(Capture);
unsafe impl Send for SendCapture {}

/// A recorder whose pipeline is initializing; see [`Starting::wait`].
pub struct Starting {
    ready: mpsc::Receiver<Result<(), String>>,
    go: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
}

impl Starting {
    /// Waits for initialization, then starts capturing. Call it once anything that must not
    /// appear in the video (the region picker) is gone.
    pub fn wait(self) -> Result<Recorder, String> {
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
    pub fn start(p: Params, on_done: impl FnOnce(Result<Outcome, String>) + Send + 'static) -> Result<Starting, String> {
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
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stops without saving: the file is not finalized and gets deleted.
    pub fn abort(&self) {
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

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
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

struct CaptureStats {
    dropped: u64,
    coalesced: u64,
}

struct Capture {
    region: Rect,
    draw_cursor: bool,
    output: IDXGIOutput1,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dupl: Option<IDXGIOutputDuplication>,
    desktop_size: (u32, u32),
    ring: Vec<(ID3D11Texture2D, IDXGIKeyedMutex)>,
    handles: Vec<HANDLE>,
}

impl Capture {
    unsafe fn new(p: &Params) -> Result<Self, String> {
        unsafe {
            let (adapter, output) = adapter_and_output(p)?;
            let (device, ctx) = create_device(&adapter, D3D11_CREATE_DEVICE_FLAG(0))?;
            let dupl = output.DuplicateOutput(&device).ctx("DuplicateOutput")?;
            let dd = dupl.GetDesc();
            if dd.Rotation != DXGI_MODE_ROTATION_IDENTITY && dd.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
                return Err("rotated (portrait) monitors are not supported yet".into());
            }
            let desktop_size = (dd.ModeDesc.Width, dd.ModeDesc.Height);
            let r = p.region;
            if r.x < 0 || r.y < 0 || r.w <= 0 || r.h <= 0 || r.w % 2 != 0 || r.h % 2 != 0 {
                return Err(format!("invalid capture region {r:?}"));
            }
            if r.right() as u32 > desktop_size.0 || r.bottom() as u32 > desktop_size.1 {
                return Err(format!("region {r:?} is outside the {}x{} desktop", desktop_size.0, desktop_size.1));
            }
            let mut ring = Vec::with_capacity(RING);
            let mut handles = Vec::with_capacity(RING);
            let desc = texture_desc(
                r,
                D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
                D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
            );
            for _ in 0..RING {
                let mut t = None;
                device.CreateTexture2D(&desc, None, Some(&mut t)).ctx("CreateTexture2D (shared)")?;
                let t = t.unwrap();
                let km: IDXGIKeyedMutex = t.cast().ctx("IDXGIKeyedMutex")?;
                handles.push(t.cast::<IDXGIResource>().ctx("IDXGIResource")?.GetSharedHandle().ctx("GetSharedHandle")?);
                ring.push((t, km));
            }
            Ok(Capture {
                region: r,
                draw_cursor: p.draw_cursor,
                output,
                device,
                ctx,
                dupl: Some(dupl),
                desktop_size,
                ring,
                handles,
            })
        }
    }

    unsafe fn run(&mut self, stop: &AtomicBool, tx: &Sender<Msg>, free_rx: &Receiver<usize>) -> Result<CaptureStats, String> {
        unsafe {
            let r = self.region;
            let mut free: Vec<usize> = (0..RING).rev().collect();
            let mut have_image = false;
            let mut ptr_visible = false;
            let mut ptr_pos = (0i32, 0i32);
            let mut shape_size: Option<(i32, i32)> = None;
            let mut shape_buf = Vec::<u8>::new();
            let mut meta = Vec::<u32>::new();
            let mut stats = CaptureStats { dropped: 0, coalesced: 0 };
            let hits = |size: Option<(i32, i32)>, vis: bool, (x, y): (i32, i32)| match size {
                Some((w, h)) if vis => Rect { x, y, w, h }.intersect(&r).is_some(),
                _ => false,
            };

            while !stop.load(Ordering::SeqCst) {
                while let Ok(i) = free_rx.try_recv() {
                    free.push(i);
                }
                let Some(dupl) = self.dupl.clone() else {
                    self.reacquire(stop)?;
                    continue;
                };
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut res = None;
                match dupl.AcquireNextFrame(50, &mut info, &mut res) {
                    Ok(()) => {}
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST || e.code() == E_ACCESSDENIED => {
                        log(&format!("duplication lost ({}), reacquiring", e.message()));
                        self.dupl = None;
                        continue;
                    }
                    Err(e) => return Err(format!("AcquireNextFrame: {} (0x{:08X})", e.message(), e.code().0 as u32)),
                }

                let mut slot = None;
                let mut changed = false;
                let mut stamp = 0i64;
                if info.LastPresentTime != 0
                    && let Some(res) = &res
                    && (!have_image || self.region_changed(&dupl, &info, &mut meta))
                {
                    if have_image && info.AccumulatedFrames > 1 {
                        stats.coalesced += info.AccumulatedFrames as u64 - 1;
                    }
                    if let Some(i) = free.pop() {
                        let desk: ID3D11Texture2D = res.cast().ctx("desktop texture")?;
                        let bx = D3D11_BOX {
                            left: r.x as u32,
                            top: r.y as u32,
                            front: 0,
                            right: r.right() as u32,
                            bottom: r.bottom() as u32,
                            back: 1,
                        };
                        let (tex, km) = &self.ring[i];
                        km.AcquireSync(0, INFINITE).ctx("AcquireSync")?;
                        self.ctx.CopySubresourceRegion(tex, 0, 0, 0, 0, &desk, 0, Some(&bx));
                        km.ReleaseSync(1).ctx("ReleaseSync")?;
                        slot = Some(i);
                        have_image = true;
                        changed = true;
                        stamp = info.LastPresentTime;
                    } else {
                        stats.dropped += 1;
                    }
                }
                if info.LastMouseUpdateTime != 0 {
                    let vis = info.PointerPosition.Visible.as_bool();
                    let pos = (info.PointerPosition.Position.x, info.PointerPosition.Position.y);
                    if self.draw_cursor
                        && (vis != ptr_visible || pos != ptr_pos)
                        && (hits(shape_size, ptr_visible, ptr_pos) || hits(shape_size, vis, pos))
                    {
                        changed = true;
                        stamp = stamp.max(info.LastMouseUpdateTime);
                    }
                    ptr_visible = vis;
                    if vis {
                        ptr_pos = pos;
                    }
                }
                if info.PointerShapeBufferSize > 0 && self.draw_cursor {
                    shape_buf.resize(info.PointerShapeBufferSize as usize, 0);
                    let mut need = 0;
                    let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                    if dupl
                        .GetFramePointerShape(shape_buf.len() as u32, shape_buf.as_mut_ptr() as _, &mut need, &mut si)
                        .is_ok()
                    {
                        let mono = si.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
                        shape_size = Some((si.Width as i32, if mono { si.Height as i32 / 2 } else { si.Height as i32 }));
                        let _ = tx.send(Msg::Shape(si, shape_buf[..need.min(shape_buf.len() as u32) as usize].to_vec()));
                        if hits(shape_size, ptr_visible, ptr_pos) {
                            changed = true;
                        }
                    }
                }
                drop(res);
                let _ = dupl.ReleaseFrame();

                if changed && have_image {
                    let cursor = (self.draw_cursor && ptr_visible).then_some(ptr_pos);
                    let qpc = if stamp != 0 { stamp } else { qpc() };
                    if tx.send(Msg::Frame { slot, qpc, cursor }).is_err() {
                        break; // encoder gave up
                    }
                }
            }
            Ok(stats)
        }
    }

    /// Whether this desktop update touched the capture region (per DDA move/dirty rects).
    unsafe fn region_changed(&self, dupl: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO, buf: &mut Vec<u32>) -> bool {
        let size = info.TotalMetadataBufferSize as usize;
        if size == 0 {
            return true;
        }
        buf.resize(size.div_ceil(4), 0);
        let r = self.region;
        let mut need = 0u32;
        unsafe {
            if dupl.GetFrameMoveRects(size as u32, buf.as_mut_ptr() as *mut DXGI_OUTDUPL_MOVE_RECT, &mut need).is_err() {
                return true;
            }
            let moves = std::slice::from_raw_parts(
                buf.as_ptr() as *const DXGI_OUTDUPL_MOVE_RECT,
                need as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>(),
            );
            if moves.iter().any(|m| Rect::from_win(&m.DestinationRect).intersect(&r).is_some()) {
                return true;
            }
            if dupl.GetFrameDirtyRects(size as u32, buf.as_mut_ptr() as *mut RECT, &mut need).is_err() {
                return true;
            }
            let dirty = std::slice::from_raw_parts(buf.as_ptr() as *const RECT, need as usize / size_of::<RECT>());
            dirty.iter().any(|d| Rect::from_win(d).intersect(&r).is_some())
        }
    }

    /// Re-creates the duplication after a mode change, UAC prompt or full-screen switch.
    unsafe fn reacquire(&mut self, stop: &AtomicBool) -> Result<(), String> {
        unsafe {
            while !stop.load(Ordering::SeqCst) {
                match self.output.DuplicateOutput(&self.device) {
                    Ok(d) => {
                        let dd = d.GetDesc();
                        if (dd.ModeDesc.Width, dd.ModeDesc.Height) != self.desktop_size {
                            return Err("display resolution changed".into());
                        }
                        self.dupl = Some(d);
                        return Ok(());
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            Ok(())
        }
    }
}

struct Encoder {
    p: Params,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    mt: ID3D11Multithread,
    ring: Vec<(ID3D11Texture2D, IDXGIKeyedMutex)>,
    clean: ID3D11Texture2D,
    cursor: Option<CursorRenderer>,
    writer: IMFSinkWriter,
    stream: u32,
    alloc: IMFVideoSampleAllocatorEx,
    _mgr: IMFDXGIDeviceManager,
    qpc_freq: i64,
}

impl Encoder {
    unsafe fn new(p: Params, cap: &Capture) -> Result<Self, String> {
        unsafe {
            let (adapter, _) = adapter_and_output(&p)?;
            let (device, ctx) = create_device(&adapter, D3D11_CREATE_DEVICE_VIDEO_SUPPORT)?;
            // Media Foundation uses this device from its own worker threads.
            let mt: ID3D11Multithread = ctx.cast().ctx("ID3D11Multithread")?;
            let _ = mt.SetMultithreadProtected(true);

            let mut ring = Vec::with_capacity(RING);
            for &h in &cap.handles {
                let mut t: Option<ID3D11Texture2D> = None;
                device.OpenSharedResource(h, &mut t).ctx("OpenSharedResource")?;
                let t = t.unwrap();
                let km: IDXGIKeyedMutex = t.cast().ctx("IDXGIKeyedMutex")?;
                ring.push((t, km));
            }
            let r = p.region;
            let mut clean = None;
            device
                .CreateTexture2D(&texture_desc(r, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_FLAG(0)), None, Some(&mut clean))
                .ctx("CreateTexture2D")?;

            let cursor = if p.draw_cursor {
                match CursorRenderer::new(&device) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        log(&format!("cursor rendering disabled: {e}"));
                        None
                    }
                }
            } else {
                None
            };

            let mut token = 0;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr).ctx("MFCreateDXGIDeviceManager")?;
            let mgr = mgr.unwrap();
            mgr.ResetDevice(&device, token).ctx("IMFDXGIDeviceManager::ResetDevice")?;

            let attrs = attributes(3)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).ctx("attr")?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &mgr).ctx("attr")?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).ctx("attr")?;
            if let Some(dir) = p.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let writer = MFCreateSinkWriterFromURL(&HSTRING::from(p.path.as_os_str()), None, &attrs)
                .ctx("MFCreateSinkWriterFromURL")?;

            let (w, h) = (r.w as u32, r.h as u32);
            let (subtype, profile) = match p.codec {
                Codec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
                Codec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
            };
            // Only a header hint in constant-QP mode (~0.1 bit per pixel).
            let nominal_bps = ((w as f64 * h as f64 * p.fps) / 10.0).clamp(2e6, 400e6) as u32;
            let fps_milli = (p.fps * 1000.0).round() as u32;
            let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
            out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
            out_t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_AVG_BITRATE, nominal_bps).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
            out_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
            out_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
            out_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).ctx("mt")?;
            let stream = writer.AddStream(&out_t).ctx("IMFSinkWriter::AddStream (codec not supported by this GPU?)")?;

            let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
            in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
            in_t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
            in_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
            in_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
            in_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).ctx("mt")?;

            let enc = attributes(4)?;
            enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_Quality.0 as u32)
                .ctx("enc")?;
            enc.SetUINT64(&CODECAPI_AVEncVideoEncodeQP, p.qp.min(51) as u64).ctx("enc")?;
            enc.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, p.effort.min(100)).ctx("enc")?;
            enc.SetUINT32(&CODECAPI_AVEncMPVGOPSize, (p.fps * 2.0).round() as u32).ctx("enc")?;
            writer.SetInputMediaType(stream, &in_t, &enc).ctx("IMFSinkWriter::SetInputMediaType")?;

            let name = describe_encoder(&writer, stream);
            writer.BeginWriting().ctx("IMFSinkWriter::BeginWriting")?;

            let mut raw = std::ptr::null_mut();
            MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw)
                .ctx("MFCreateVideoSampleAllocatorEx")?;
            let alloc = IMFVideoSampleAllocatorEx::from_raw(raw);
            alloc.SetDirectXManager(&mgr).ctx("SetDirectXManager")?;
            let aattrs = attributes(2)?;
            aattrs
                .SetUINT32(&MF_SA_D3D11_BINDFLAGS, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32)
                .ctx("attr")?;
            aattrs.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32).ctx("attr")?;
            alloc.InitializeSampleAllocatorEx(4, 16, &aattrs, &in_t).ctx("InitializeSampleAllocatorEx")?;

            let mut qpc_freq = 0;
            let _ = QueryPerformanceFrequency(&mut qpc_freq);
            log(&format!(
                "recording {w}x{h}@{:.3} {:?} qp={} effort={} -> {} [{name}]",
                p.fps,
                p.codec,
                p.qp,
                p.effort,
                p.path.display()
            ));
            Ok(Encoder {
                p,
                device,
                ctx,
                mt,
                ring,
                clean: clean.unwrap(),
                cursor,
                writer,
                stream,
                alloc,
                _mgr: mgr,
                qpc_freq,
            })
        }
    }

    fn to_hns(&self, qpc_delta: i64) -> i64 {
        (qpc_delta as i128 * 10_000_000 / self.qpc_freq as i128) as i64
    }

    /// Consumes capture messages until `End`, then finalizes the file (skipped when
    /// `abort` is set: the file is going to be deleted).
    /// Returns (frames written, frames dropped, duration in seconds).
    unsafe fn run(&mut self, rx: &Receiver<Msg>, free_tx: &Sender<usize>, abort: &AtomicBool) -> Result<(u64, u64, f64), String> {
        unsafe {
            // Frames sit on the stream's frame grid. Capture times jitter by a millisecond or two
            // around vsync (which would show up as judder), cursor-only updates arrive between
            // vsyncs, and a capped fps means several captures per slot: everything that lands in
            // the same slot is merged into one frame, the latest state wins.
            let grid = 1e7 / self.p.fps;
            let slot_time = |slot: i64| (slot as f64 * grid).round() as i64;
            let mut start_qpc: Option<i64> = None;
            // The newest frame and its slot; it is handed to the encoder once a later slot begins.
            let mut pending: Option<(IMFSample, i64)> = None;
            let mut have_image = false;
            let (mut frames, mut dropped) = (0u64, 0u64);
            let mut end_qpc = None;

            for msg in rx.iter() {
                match msg {
                    Msg::Shape(si, buf) => {
                        if let Some(c) = self.cursor.as_mut()
                            && let Err(e) = c.set_shape(&self.device, &si, &buf)
                        {
                            log(&format!("pointer shape update failed: {e}"));
                        }
                    }
                    Msg::End { qpc } => {
                        end_qpc = Some(qpc);
                        break;
                    }
                    Msg::Frame { slot: ring_slot, qpc, cursor } => {
                        if let Some(i) = ring_slot {
                            let (tex, km) = &self.ring[i];
                            self.mt.Enter();
                            let r = km.AcquireSync(1, INFINITE);
                            if r.is_ok() {
                                self.ctx.CopyResource(&self.clean, tex);
                                let _ = km.ReleaseSync(0);
                            }
                            self.mt.Leave();
                            r.ctx("AcquireSync")?;
                            let _ = free_tx.send(i);
                            have_image = true;
                        }
                        if !have_image {
                            continue;
                        }
                        let start = *start_qpc.get_or_insert(qpc);
                        let slot = ((self.to_hns(qpc - start) as f64 / grid).round() as i64).max(0);
                        match &pending {
                            Some((s, ps)) if slot <= *ps => self.compose(s, cursor)?,
                            _ => {
                                let sample = match self.alloc.AllocateSample() {
                                    Ok(s) => s,
                                    Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => {
                                        dropped += 1;
                                        continue;
                                    }
                                    Err(e) => return Err(format!("AllocateSample: {}", e.message())),
                                };
                                self.compose(&sample, cursor)?;
                                sample.SetSampleTime(slot_time(slot)).ctx("SetSampleTime")?;
                                if let Some((prev, ps)) = pending.take() {
                                    prev.SetSampleDuration(slot_time(slot) - slot_time(ps)).ctx("SetSampleDuration")?;
                                    self.writer.WriteSample(self.stream, &prev).ctx("WriteSample")?;
                                    frames += 1;
                                }
                                pending = Some((sample, slot));
                            }
                        }
                    }
                }
            }

            if abort.load(Ordering::SeqCst) {
                return Ok((frames, dropped, 0.0));
            }
            let mut duration_s = 0.0;
            if let (Some((prev, ps)), Some(start)) = (pending.take(), start_qpc) {
                let end = end_qpc.unwrap_or_else(qpc);
                let end_slot = ((self.to_hns(end - start) as f64 / grid).round() as i64).max(ps + 1);
                prev.SetSampleDuration(slot_time(end_slot) - slot_time(ps)).ctx("SetSampleDuration")?;
                self.writer.WriteSample(self.stream, &prev).ctx("WriteSample")?;
                frames += 1;
                duration_s = slot_time(end_slot) as f64 / 1e7;
            }
            if frames == 0 {
                return Err("no frames were captured".into());
            }
            self.writer.Finalize().ctx("IMFSinkWriter::Finalize")?;
            Ok((frames, dropped, duration_s))
        }
    }

    /// Copies the clean region image into a pooled sample texture and draws the cursor on it.
    unsafe fn compose(&self, sample: &IMFSample, cursor: Option<(i32, i32)>) -> Result<(), String> {
        unsafe {
            let buf = sample.GetBufferByIndex(0).ctx("GetBufferByIndex")?;
            let dx: IMFDXGIBuffer = buf.cast().ctx("IMFDXGIBuffer")?;
            let mut raw = std::ptr::null_mut();
            dx.GetResource(&ID3D11Texture2D::IID, &mut raw).ctx("IMFDXGIBuffer::GetResource")?;
            let tex = ID3D11Texture2D::from_raw(raw);
            let sub = dx.GetSubresourceIndex().ctx("GetSubresourceIndex")?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>()
                && let Ok(len) = b2.GetContiguousLength()
            {
                let _ = buf.SetCurrentLength(len);
            }

            let r = self.p.region;
            let rtv = match (&self.cursor, cursor) {
                (Some(c), Some(_)) if c.size().is_some() => {
                    let mut desc = D3D11_RENDER_TARGET_VIEW_DESC {
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                        ..Default::default()
                    };
                    desc.Anonymous.Texture2DArray =
                        D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: sub, ArraySize: 1 };
                    let mut rtv = None;
                    self.device.CreateRenderTargetView(&tex, Some(&desc), Some(&mut rtv)).ctx("CreateRenderTargetView")?;
                    rtv
                }
                _ => None,
            };

            self.mt.Enter();
            self.ctx.CopySubresourceRegion(&tex, sub, 0, 0, 0, &self.clean, 0, None);
            if let (Some(rtv), Some(c), Some((x, y))) = (&rtv, &self.cursor, cursor) {
                c.draw(&self.ctx, rtv, r.w, r.h, x - r.x, y - r.y);
            }
            self.mt.Leave();
            Ok(())
        }
    }
}

unsafe fn attributes(n: u32) -> Result<IMFAttributes, String> {
    let mut a = None;
    unsafe { MFCreateAttributes(&mut a, n).ctx("MFCreateAttributes")? };
    Ok(a.unwrap())
}

/// Friendly name of the encoder MFT the sink writer picked; logs its effective settings.
unsafe fn describe_encoder(writer: &IMFSinkWriter, stream: u32) -> String {
    unsafe {
        let Ok(ex) = writer.cast::<IMFSinkWriterEx>() else { return "unknown encoder".into() };
        let mut i = 0;
        loop {
            let mut cat = GUID::zeroed();
            let mut t = None;
            if ex.GetTransformForStream(stream, i, Some(&mut cat), &mut t).is_err() {
                return "unknown encoder".into();
            }
            i += 1;
            let Some(t) = t else { continue };
            if cat != MFT_CATEGORY_VIDEO_ENCODER {
                continue;
            }
            let mut name = String::from("video encoder");
            if let Ok(a) = t.GetAttributes() {
                let mut p = PWSTR::null();
                let mut len = 0;
                if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len).is_ok() {
                    name = p.to_string().unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as _));
                }
            }
            if let Ok(api) = t.cast::<ICodecAPI>() {
                let get = |g: &GUID| api.GetValue(g).ok().map(|v| variant_u64(&v));
                log(&format!(
                    "{name}: rc={:?} qp={:?} effort={:?} gop={:?}",
                    get(&CODECAPI_AVEncCommonRateControlMode),
                    get(&CODECAPI_AVEncVideoEncodeQP),
                    get(&CODECAPI_AVEncCommonQualityVsSpeed),
                    get(&CODECAPI_AVEncMPVGOPSize),
                ));
            }
            return name;
        }
    }
}

fn variant_u64(v: &VARIANT) -> u64 {
    use windows::Win32::System::Variant::{VT_BOOL, VT_I4, VT_UI4, VT_UI8};
    unsafe {
        let a = &v.Anonymous.Anonymous;
        match a.vt {
            VT_UI4 => a.Anonymous.ulVal as u64,
            VT_I4 => a.Anonymous.lVal as u64,
            VT_UI8 => a.Anonymous.ullVal,
            VT_BOOL => (a.Anonymous.boolVal.0 != 0) as u64,
            _ => u64::MAX,
        }
    }
}
