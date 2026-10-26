use std::marker::PhantomData;
use std::sync::LazyLock;

use glint_core::geom::rect::Rect;
use glint_core::record::clicks::Style;
use glint_core::record::keys::Style as KeyStyle;
use glint_core::record::pipeline::{Backend, ClickCapture, Encoded, KeyCapture, PacketSink, Parts, Role, open_audio};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_HIGHEST,
};

use super::capture::Capture;
use glint_core::record::negotiate::{Force, Route, describe};

use super::encoder::{self, Composer, SinkWriter, log_start, video_config};
use super::gpu::Gpu;
use super::packets::{AacEncoder, MfVideoEncoder};
use super::keys::Hook as KeyHook;
use super::mouse::{self, Hook};
use super::{Ctx, Params, audio};

/// Shared textures between capture and encoder; capture drops frames only if all are busy.
pub(super) const RING: usize = 6;
pub(super) const INFINITE: u32 = u32::MAX;

/// Records with the sink writer encoding and muxing in one.
pub(super) struct Windows(pub(super) Params);

/// Records through the packet path: the encoders' packets go to the sink `make` opens (on the
/// encoder thread, where COM and Media Foundation are up).
pub(super) struct Packets<F, S> {
    pub(super) params: Params,
    pub(super) make: F,
    pub(super) sink: PhantomData<fn() -> S>,
}

pub(super) struct Thread {
    com: bool,
    mf: bool,
}

impl Drop for Thread {
    fn drop(&mut self) {
        unsafe {
            if self.mf {
                let _ = MFShutdown();
            }
            if self.com {
                CoUninitialize();
            }
        }
    }
}

fn enter(role: Role) -> Thread {
    let (priority, com, mf) = match role {
        Role::Encode => (THREAD_PRIORITY_ABOVE_NORMAL, true, true),
        Role::Capture => (THREAD_PRIORITY_HIGHEST, false, false),
        Role::Audio => (THREAD_PRIORITY_HIGHEST, true, false),
        Role::Mouse => (THREAD_PRIORITY_HIGHEST, false, false),
        Role::Keys => (THREAD_PRIORITY_HIGHEST, true, false),
    };
    set_priority(priority);
    unsafe {
        let com = com && CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let mf = mf && MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
        Thread { com, mf }
    }
}

impl Backend for Windows {
    type Capture = Capture;
    type Frames = Composer;
    type Output = SinkWriter;
    type Audio = audio::Source;
    type Mouse = Hook;
    type Keys = KeyHook;
    type Guard = Thread;

    fn enter(role: Role) -> Thread {
        enter(role)
    }

    fn now() -> i64 {
        hns(qpc())
    }

    fn open(self) -> Result<Parts<Self>, String> {
        unsafe { open(self.0, |p, gpu, force, names| encoder::sink_writer(p, gpu, force, names)) }
    }
}

impl<F, S> Backend for Packets<F, S>
where
    F: FnOnce() -> Result<S, String> + Send + 'static,
    S: PacketSink + 'static,
{
    type Capture = Capture;
    type Frames = Composer;
    type Output = Encoded<MfVideoEncoder, AacEncoder, S>;
    type Audio = audio::Source;
    type Mouse = Hook;
    type Keys = KeyHook;
    type Guard = Thread;

    fn enter(role: Role) -> Thread {
        enter(role)
    }

    fn now() -> i64 {
        hns(qpc())
    }

    fn open(self) -> Result<Parts<Self>, String> {
        let make = self.make;
        unsafe {
            open(self.params, move |p, gpu, force, names| {
                let tracks = names.len();
                let audio = names.into_iter().map(|n| Ok((n, AacEncoder::new()?))).collect::<Result<Vec<_>, String>>()?;
                let output = Encoded::new(MfVideoEncoder::new(gpu, force), &video_config(p), audio, make()?)?;
                let video = output.video_encoder();
                let route = video.route().ok_or("the video encoder isn't configured")?;
                log_start(p, &format!("packet path: {}", describe(video.name(), &gpu.adapter.name, route)), tracks);
                let notice = video.notice();
                Ok((output, route, notice))
            })
        }
    }
}

/// Opens capture, audio, the output `output` makes on the encoder's side of the GPU (with the
/// routes the environment leaves and the audio tracks' names; it says which route its encoder
/// took) and the compositor feeding it what that encoder takes. On failure the partial file is
/// deleted.
unsafe fn open<B>(
    p: Params,
    output: impl FnOnce(&Params, &Gpu, Force, Vec<String>) -> Result<(B::Output, Route, Option<&'static str>), String>,
) -> Result<Parts<B>, String>
where
    B: Backend<Capture = Capture, Frames = Composer, Audio = audio::Source, Mouse = Hook, Keys = KeyHook>,
{
    let opened = unsafe { open_parts::<B>(&p, output) };
    if opened.is_err() {
        let _ = std::fs::remove_file(&p.path);
    }
    opened
}

unsafe fn open_parts<B>(
    p: &Params,
    output: impl FnOnce(&Params, &Gpu, Force, Vec<String>) -> Result<(B::Output, Route, Option<&'static str>), String>,
) -> Result<Parts<B>, String>
where
    B: Backend<Capture = Capture, Frames = Composer, Audio = audio::Source, Mouse = Hook, Keys = KeyHook>,
{
    unsafe {
        let capture = Capture::new(p)?;
        let audio = open_audio::<audio::Source>(&p.audio, &p.track_names);
        let names = audio.tracks.iter().map(|t| t.name.clone()).collect();
        let gpu = Gpu::open(&capture.adapter, (p.region.w as u32, p.region.h as u32))?;
        let (output, route, notice) = output(p, &gpu, Force::from_env(), names)?;
        let frames = Composer::new(p, &capture.handles, gpu, route)?;
        let notices = notice.map(String::from).into_iter().collect();
        let clicks = p.clicks.map(|c| {
            let scale = mouse::scale(p.region);
            ClickCapture { mouse: Hook { region: p.region }, style: Style::new(c.accent, scale), scale, burn_in: c.burn_in }
        });
        let keys = p.keys.map(|o| {
            let scale = mouse::scale(p.region);
            KeyCapture { keys: KeyHook::new(o.level), style: KeyStyle::new(o.accent, scale).with(o.pos), scale, burn_in: o.burn_in }
        });
        Ok(Parts { capture, frames, output, audio, fps: p.fps, path: p.path.clone(), clicks, keys, notices })
    }
}

fn set_priority(p: THREAD_PRIORITY) {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), p);
    }
}

pub(super) fn qpc() -> i64 {
    let mut v = 0;
    unsafe {
        let _ = QueryPerformanceCounter(&mut v);
    }
    v
}

pub(super) fn hns(qpc: i64) -> i64 {
    static FREQ: LazyLock<i64> = LazyLock::new(|| {
        let mut f = 0;
        unsafe {
            let _ = QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    });
    (qpc as i128 * 10_000_000 / *FREQ as i128) as i64
}

pub(super) unsafe fn create_device(
    adapter: &IDXGIAdapter1,
    flags: D3D11_CREATE_DEVICE_FLAG,
) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
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

pub(super) unsafe fn adapter(idx: u32) -> Result<IDXGIAdapter1, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ctx("CreateDXGIFactory1")?;
        factory.EnumAdapters1(idx).ctx("EnumAdapters1")
    }
}

pub(super) fn texture_desc(r: Rect, bind: D3D11_BIND_FLAG, misc: D3D11_RESOURCE_MISC_FLAG) -> D3D11_TEXTURE2D_DESC {
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
