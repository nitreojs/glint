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

/// Maps a drag from `anchor` to `cursor` (desktop coordinates) onto one output: the output
/// under the anchor (or, failing that, the one overlapping the drag most), clipped to it.
/// Tiny selections grow to the encoder minimum *away from the anchor* and odd sizes lose
/// their last pixel on the cursor side, so the corner where the drag started never moves.
pub fn fit_drag(anchor: (i32, i32), cursor: (i32, i32), outputs: &[Output]) -> Option<(Rect, usize)> {
    let raw = Rect::from_points(anchor.0, anchor.1, cursor.0, cursor.1);
    let idx = output_at(outputs, anchor.0, anchor.1).or_else(|| {
        let overlap = |o: &Output| o.rect.intersect(&raw).map_or(0, |r| r.area());
        outputs.iter().enumerate().filter(|(_, o)| overlap(o) > 0).max_by_key(|(_, o)| overlap(o)).map(|(i, _)| i)
    })?;
    let mon = outputs[idx].rect;
    let axis = |a: i32, c: i32, lo: i32, hi: i32, min: i32| -> (i32, i32) {
        let (a, c) = (a.clamp(lo, hi), c.clamp(lo, hi));
        let len = ((c - a).abs().max(min.min(hi - lo))) & !1;
        if c >= a {
            let start = a.min(hi - len); // pushed back only when the monitor edge forces it
            (start, len)
        } else {
            let end = a.max(lo + len);
            (end - len, len)
        }
    };
    let (x, w) = axis(anchor.0, cursor.0, mon.x, mon.right(), MIN_W);
    let (y, h) = axis(anchor.1, cursor.1, mon.y, mon.bottom(), MIN_H);
    Some((Rect { x, y, w, h }, idx))
}

pub fn output_at(outputs: &[Output], x: i32, y: i32) -> Option<usize> {
    outputs.iter().position(|o| o.rect.contains(x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(x: i32, y: i32, w: i32, h: i32) -> Output {
        Output {
            adapter_idx: 0,
            output_idx: 0,
            device_name: String::new(),
            rect: Rect { x, y, w, h },
            refresh_hz: 144.0,
            rotated: false,
        }
    }

    #[test]
    fn tiny_drags_grow_away_from_the_anchor() {
        let outs = [output(0, 0, 3440, 1440)];
        // A few pixels above the start point: the bottom edge stays at the anchor.
        let (r, _) = fit_drag((500, 400), (530, 390), &outs).unwrap();
        assert_eq!((r.x, r.bottom(), r.w, r.h), (500, 400, MIN_W, MIN_H));
        // Moving further up (still under the minimum) must not move the box at all.
        let (r2, _) = fit_drag((500, 400), (530, 350), &outs).unwrap();
        assert_eq!(r2, r);
        // Down-right: the top-left corner stays at the anchor.
        let (r, _) = fit_drag((500, 400), (510, 410), &outs).unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (500, 400, MIN_W, MIN_H));
    }

    #[test]
    fn odd_sizes_trim_the_cursor_side() {
        let outs = [output(0, 0, 3440, 1440)];
        let (r, _) = fit_drag((1000, 800), (499, 299), &outs).unwrap(); // up-left, 501 x 501
        assert_eq!((r.right(), r.bottom(), r.w, r.h), (1000, 800, 500, 500));
    }

    #[test]
    fn growth_is_pushed_inside_the_monitor_and_stays_on_the_anchor_monitor() {
        let outs = [output(-1920, 540, 1920, 1080), output(0, 0, 3440, 1440)];
        let (r, i) = fit_drag((3430, 1435), (3436, 1438), &outs).unwrap();
        assert_eq!((i, r.right(), r.bottom(), r.w, r.h), (1, 3440, 1440, MIN_W, MIN_H));
        // A drag that starts on the right monitor and crosses into the left one is clipped to the right one.
        let (r, i) = fit_drag((300, 600), (-500, 900), &outs).unwrap();
        assert_eq!((i, r.x, r.right()), (1, 0, 300));
    }
}
