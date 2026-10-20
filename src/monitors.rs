use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::core::PCWSTR;

use crate::util::{Rect, from_wstr, wide};

/// A desktop-attached DXGI output (monitor).
#[derive(Clone, Debug)]
pub struct Output {
    pub adapter_idx: u32,
    pub output_idx: u32,
    pub device_name: String,
    /// Desktop coordinates in physical pixels.
    pub rect: Rect,
    pub refresh_hz: f64,
    pub rotated: bool,
}

impl Output {
    /// Native frame rate rounded to an integer (143.98 Hz -> 144).
    pub fn native_fps(&self) -> u32 {
        (self.refresh_hz.round() as u32).max(1)
    }
}

pub fn enumerate() -> windows::core::Result<Vec<Output>> {
    let rates = refresh_rates();
    let mut out = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut ai = 0;
        while let Ok(adapter) = factory.EnumAdapters1(ai) {
            let mut oi = 0;
            while let Ok(output) = adapter.EnumOutputs(oi) {
                let d = output.GetDesc()?;
                if d.AttachedToDesktop.as_bool() {
                    let device_name = from_wstr(&d.DeviceName);
                    let refresh_hz = rates
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case(&device_name))
                        .map(|(_, hz)| *hz)
                        .unwrap_or_else(|| gdi_refresh(&device_name));
                    out.push(Output {
                        adapter_idx: ai,
                        output_idx: oi,
                        device_name,
                        rect: Rect::from_win(&d.DesktopCoordinates),
                        refresh_hz,
                        rotated: d.Rotation != DXGI_MODE_ROTATION_IDENTITY && d.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED,
                    });
                }
                oi += 1;
            }
            ai += 1;
        }
    }
    Ok(out)
}

/// Exact (rational) refresh rates of active display paths, keyed by GDI device name.
fn refresh_rates() -> Vec<(String, f64)> {
    let mut res = Vec::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return res;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None)
            != ERROR_SUCCESS
        {
            return res;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let rr = p.targetInfo.refreshRate;
            if rr.Denominator != 0 && rr.Numerator != 0 {
                res.push((from_wstr(&src.viewGdiDeviceName), rr.Numerator as f64 / rr.Denominator as f64));
            }
        }
    }
    res
}

fn gdi_refresh(device: &str) -> f64 {
    let name = wide(device);
    let mut dm = DEVMODEW { dmSize: size_of::<DEVMODEW>() as u16, ..Default::default() };
    unsafe {
        if EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() && dm.dmDisplayFrequency > 1
        {
            return dm.dmDisplayFrequency as f64;
        }
    }
    60.0
}

/// Minimum capture size; hardware encoders reject tiny frames.
pub const MIN_W: i32 = 160;
pub const MIN_H: i32 = 64;

/// Maps a desktop-space selection onto one output: picks the output with the largest
/// overlap, clips to it, grows tiny selections to the encoder minimum and rounds the size
/// down to even numbers (required by 4:2:0 video).
pub fn fit_selection(sel: Rect, outputs: &[Output]) -> Option<(Rect, usize)> {
    let (idx, clipped) = outputs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| o.rect.intersect(&sel).map(|r| (i, r)))
        .max_by_key(|(_, r)| r.area())?;
    let mon = outputs[idx].rect;
    let mut r = clipped;
    for (pos, len, min, lo, hi) in [
        (&mut r.x, &mut r.w, MIN_W, mon.x, mon.right()),
        (&mut r.y, &mut r.h, MIN_H, mon.y, mon.bottom()),
    ] {
        if *len < min {
            let c = *pos + *len / 2;
            *len = min.min(hi - lo);
            *pos = (c - *len / 2).clamp(lo, hi - *len);
        }
        *len &= !1;
    }
    Some((r, idx))
}

pub fn output_at(outputs: &[Output], x: i32, y: i32) -> Option<usize> {
    outputs.iter().position(|o| o.rect.contains(x, y))
}
