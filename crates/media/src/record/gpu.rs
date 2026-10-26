//! The encoder's side of the capturing GPU: a D3D11 device shared with Media Foundation, what
//! the adapter is called, its encoders and whether it can convert frames to NV12.

use glint_core::log::log;
use glint_core::record::Codec;
use glint_core::record::negotiate::{Force, Route, routes};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::IDXGIAdapter1;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, Interface, PWSTR};

use super::Ctx;
use super::convert::Converter;
use super::encoder::attributes;
use super::win::create_device;

#[derive(Clone, Debug)]
pub(super) struct Adapter {
    pub name: String,
    pub luid: [u8; 8],
    pub vendor: u32,
}

impl Adapter {
    pub(super) unsafe fn of(adapter: &IDXGIAdapter1) -> Result<Adapter, String> {
        let d = unsafe { adapter.GetDesc1() }.ctx("IDXGIAdapter1::GetDesc1")?;
        let len = d.Description.iter().position(|&c| c == 0).unwrap_or(d.Description.len());
        let name = String::from_utf16_lossy(&d.Description[..len]).trim().to_string();
        let mut luid = [0u8; 8];
        luid[..4].copy_from_slice(&d.AdapterLuid.LowPart.to_le_bytes());
        luid[4..].copy_from_slice(&d.AdapterLuid.HighPart.to_le_bytes());
        Ok(Adapter { name, luid, vendor: d.VendorId })
    }

    /// How hardware MFTs name their vendor: `VEN_10DE`.
    pub(super) fn vendor_id(&self) -> String {
        format!("VEN_{:04X}", self.vendor)
    }
}

pub(super) struct Gpu {
    pub device: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    pub mt: ID3D11Multithread,
    pub mgr: IMFDXGIDeviceManager,
    pub adapter: Adapter,
    /// BGRA → NV12 for encoders that want it, when the GPU's video processor can.
    pub converter: Option<Converter>,
}

impl Gpu {
    /// A device on `adapter` (the one that captures) shared with Media Foundation, for
    /// `size` frames.
    pub(super) unsafe fn open(adapter: &IDXGIAdapter1, size: (u32, u32)) -> Result<Gpu, String> {
        unsafe {
            let info = Adapter::of(adapter)?;
            let (device, ctx) = create_device(adapter, D3D11_CREATE_DEVICE_VIDEO_SUPPORT)?;
            // Media Foundation uses this device from its own worker threads.
            let mt: ID3D11Multithread = ctx.cast().ctx("ID3D11Multithread")?;
            let _ = mt.SetMultithreadProtected(true);
            let mut token = 0;
            let mut mgr = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr).ctx("MFCreateDXGIDeviceManager")?;
            let mgr = mgr.unwrap();
            mgr.ResetDevice(&device, token).ctx("IMFDXGIDeviceManager::ResetDevice")?;
            let converter = match Converter::new(&device, &ctx, size) {
                Ok(c) => Some(c),
                Err(e) => {
                    log(&format!("{}: no NV12 conversion on the GPU: {e}", info.name));
                    None
                }
            };
            Ok(Gpu { device, ctx, mt, mgr, adapter: info, converter })
        }
    }

    /// The routes to the encoder worth trying on this GPU.
    pub(super) fn routes(&self, force: Force) -> Vec<Route> {
        routes(force, self.converter.is_some())
    }
}

pub(super) fn codec_subtype(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
    }
}

/// The hardware `codec` encoders of `adapter`, best first: those `MFTEnum2` lists for its LUID,
/// or, when that finds none (drivers that don't tag their encoders with it), every hardware
/// encoder of the adapter's vendor.
pub(super) unsafe fn hardware_encoders(codec: Codec, adapter: &Adapter) -> Vec<IMFActivate> {
    let out = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: codec_subtype(codec) };
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    unsafe {
        let by_luid = || -> Result<Vec<IMFActivate>, String> {
            let attrs = attributes(1)?;
            attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &adapter.luid).ctx("MFT_ENUM_ADAPTER_LUID")?;
            let (mut list, mut n) = (std::ptr::null_mut(), 0u32);
            MFTEnum2(MFT_CATEGORY_VIDEO_ENCODER, flags, None, Some(&out), &attrs, &mut list, &mut n).ctx("MFTEnum2")?;
            Ok(take_list(list, n))
        };
        match by_luid() {
            Ok(found) if !found.is_empty() => return found,
            Ok(_) => {}
            Err(e) => log(&format!("{}: {e}", adapter.name)),
        }
        let (mut list, mut n) = (std::ptr::null_mut(), 0u32);
        if MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, None, Some(&out), &mut list, &mut n).is_err() {
            return Vec::new();
        }
        let vendor = adapter.vendor_id();
        let found: Vec<IMFActivate> = take_list(list, n)
            .into_iter()
            .filter(|a| string(a, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute).is_some_and(|v| v.eq_ignore_ascii_case(&vendor)))
            .collect();
        if !found.is_empty() {
            log(&format!("{}: its {} encoders found by vendor ({vendor}), not by adapter", adapter.name, codec.name()));
        }
        found
    }
}

/// Windows' software `codec` encoders, best first.
pub(super) unsafe fn software_encoders(codec: Codec) -> Vec<IMFActivate> {
    let out = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: codec_subtype(codec) };
    let flags = MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
    unsafe {
        let (mut list, mut n) = (std::ptr::null_mut(), 0u32);
        if MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, None, Some(&out), &mut list, &mut n).is_err() {
            return Vec::new();
        }
        take_list(list, n)
    }
}

unsafe fn take_list(list: *mut Option<IMFActivate>, n: u32) -> Vec<IMFActivate> {
    if list.is_null() {
        return Vec::new();
    }
    unsafe {
        let found = (0..n as usize).filter_map(|i| (*list.add(i)).take()).collect();
        CoTaskMemFree(Some(list as _));
        found
    }
}

pub(super) fn string(a: &IMFAttributes, key: &GUID) -> Option<String> {
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0;
        a.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

pub(super) fn friendly_name(a: &IMFAttributes) -> String {
    string(a, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_else(|| "video encoder".into())
}

/// A Media Foundation error in plain words, with the step and the code for the log.
pub(super) fn plain(e: &windows::core::Error, step: &str) -> String {
    let code = e.code();
    let words = match code {
        MF_E_INVALIDMEDIATYPE | MF_E_TRANSFORM_TYPE_NOT_SET | MF_E_INVALIDTYPE | MF_E_UNSUPPORTED_D3D_TYPE => {
            "doesn't take frames in this format or size".to_string()
        }
        MF_E_TOPO_CODEC_NOT_FOUND => "found no encoder for this codec".to_string(),
        MF_E_HW_MFT_FAILED_START_STREAMING => "couldn't start (the GPU may be out of encoder sessions)".to_string(),
        windows::Win32::Foundation::E_INVALIDARG | MF_E_OUT_OF_RANGE => "refused the quality settings".to_string(),
        _ => format!("failed: {}", e.message().trim().trim_end_matches('.')),
    };
    format!("{words} ({step}, 0x{:08X})", code.0 as u32)
}

/// The name of a video subtype, for messages.
pub(super) fn subtype_name(g: &GUID) -> String {
    let known = [
        (MFVideoFormat_NV12, "NV12"),
        (MFVideoFormat_ARGB32, "ARGB32"),
        (MFVideoFormat_RGB32, "RGB32"),
        (MFVideoFormat_YUY2, "YUY2"),
        (MFVideoFormat_I420, "I420"),
        (MFVideoFormat_IYUV, "IYUV"),
        (MFVideoFormat_YV12, "YV12"),
        (MFVideoFormat_P010, "P010"),
        (MFVideoFormat_AYUV, "AYUV"),
        (MFVideoFormat_A2R10G10B10, "A2R10G10B10"),
    ];
    match known.iter().find(|(k, _)| k == g) {
        Some((_, n)) => n.to_string(),
        // FourCC subtypes keep the code in the first field
        None => String::from_utf8_lossy(&g.data1.to_le_bytes()).into_owned(),
    }
}
