use std::sync::LazyLock;

use glint_core::geom::rect::Rect;
use glint_core::record::clicks::Style;
use glint_core::record::pipeline::{Backend, ClickCapture, Parts, Role, open_audio};
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
use super::encoder::{self, Composer, SinkWriter};
use super::mouse::{self, Hook};
use super::{Ctx, Params, audio};

/// Shared textures between capture and encoder; capture drops frames only if all are busy.
pub(super) const RING: usize = 6;
pub(super) const INFINITE: u32 = u32::MAX;

pub(super) struct Windows(pub(super) Params);

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

impl Backend for Windows {
    type Capture = Capture;
    type Frames = Composer;
    type Output = SinkWriter;
    type Audio = audio::Source;
    type Mouse = Hook;
    type Guard = Thread;

    fn enter(role: Role) -> Thread {
        let (priority, com, mf) = match role {
            Role::Encode => (THREAD_PRIORITY_ABOVE_NORMAL, true, true),
            Role::Capture => (THREAD_PRIORITY_HIGHEST, false, false),
            Role::Audio => (THREAD_PRIORITY_HIGHEST, true, false),
            Role::Mouse => (THREAD_PRIORITY_HIGHEST, false, false),
        };
        set_priority(priority);
        unsafe {
            let com = com && CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let mf = mf && MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
            Thread { com, mf }
        }
    }

    fn now() -> i64 {
        hns(qpc())
    }

    fn open(self) -> Result<Parts<Self>, String> {
        let p = self.0;
        let opened = unsafe { open(&p) };
        if opened.is_err() {
            let _ = std::fs::remove_file(&p.path);
        }
        opened
    }
}

unsafe fn open(p: &Params) -> Result<Parts<Windows>, String> {
    unsafe {
        let capture = Capture::new(p)?;
        let audio = open_audio::<audio::Source>(&p.audio, &p.track_names);
        let names = audio.tracks.iter().map(|t| t.name.clone()).collect();
        let (frames, output) = encoder::open(p, &capture, names)?;
        let clicks = p.clicks.map(|c| {
            let scale = mouse::scale(p.region);
            ClickCapture { mouse: Hook { region: p.region }, style: Style::new(c.accent, scale), scale, burn_in: c.burn_in }
        });
        Ok(Parts { capture, frames, output, audio, fps: p.fps, path: p.path.clone(), clicks })
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
