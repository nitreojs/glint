use glint_core::geom::monitor::Output;
use glint_core::geom::rect::Rect;
use glint_core::wide::{from_wstr, wide};
use glint_media::rect::WinRect;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig,
};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::core::PCWSTR;

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
                        rect: Rect::from_win(&d.DesktopCoordinates),
                        refresh_hz,
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
