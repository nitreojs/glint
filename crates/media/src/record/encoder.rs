use std::path::{Path, PathBuf};

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use glint_core::record::Codec;
use glint_core::record::mix::{RATE, frames_to_hns};
use glint_core::record::mp4::name_tracks;
use glint_core::record::clicks::Shape;
use glint_core::record::keys::KeyFrame;
use glint_core::record::pipeline::{Frames, Output, VideoConfig};
use glint_core::record::pointer::PointerShape;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use super::capture::Capture;
use super::cursor::CursorRenderer;
use crate::clicks;
use crate::keys;
use super::win::{INFINITE, RING, create_device, texture_desc};
use super::{Ctx, Params};

/// 192 kbps, one of the four rates the media foundation aac encoder accepts.
pub(super) const AAC_BYTES_PER_SECOND: u32 = 24_000;

pub(super) fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

pub(super) struct Composer {
    region: Rect,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    mt: ID3D11Multithread,
    ring: Vec<(ID3D11Texture2D, IDXGIKeyedMutex)>,
    clean: ID3D11Texture2D,
    cursor: Option<CursorRenderer>,
    clicks: Option<clicks::Renderer>,
    keys: Option<keys::Renderer>,
    alloc: IMFVideoSampleAllocatorEx,
    _mgr: IMFDXGIDeviceManager,
}

pub(super) struct SinkWriter {
    writer: IMFSinkWriter,
    stream: u32,
    audio: Vec<u32>,
    track_names: Vec<String>,
    path: PathBuf,
}

/// The frame compositor on a device shared with Media Foundation through the returned manager;
/// frames are ARGB32 samples of [`input_type`].
pub(super) unsafe fn composer(p: &Params, cap: &Capture) -> Result<(Composer, IMFDXGIDeviceManager), String> {
    unsafe {
        let (device, ctx) = create_device(&cap.adapter, D3D11_CREATE_DEVICE_VIDEO_SUPPORT)?;
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
        let clicks = match p.clicks {
            Some(c) if c.burn_in => match clicks::Renderer::new(&device) {
                Ok(r) => Some(r),
                Err(e) => {
                    log(&format!("click highlights won't be drawn: {e}"));
                    None
                }
            },
            _ => None,
        };
        let keys = match &p.keys {
            Some(k) if k.burn_in => match keys::Renderer::new(&device) {
                Ok(r) => Some(r),
                Err(e) => {
                    log(&format!("the keystroke overlay won't be drawn: {e}"));
                    None
                }
            },
            _ => None,
        };

        let mut token = 0;
        let mut mgr = None;
        MFCreateDXGIDeviceManager(&mut token, &mut mgr).ctx("MFCreateDXGIDeviceManager")?;
        let mgr = mgr.unwrap();
        mgr.ResetDevice(&device, token).ctx("IMFDXGIDeviceManager::ResetDevice")?;

        let mut raw = std::ptr::null_mut();
        MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw).ctx("MFCreateVideoSampleAllocatorEx")?;
        let alloc = IMFVideoSampleAllocatorEx::from_raw(raw);
        alloc.SetDirectXManager(&mgr).ctx("SetDirectXManager")?;
        let aattrs = attributes(2)?;
        aattrs
            .SetUINT32(&MF_SA_D3D11_BINDFLAGS, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32)
            .ctx("attr")?;
        aattrs.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32).ctx("attr")?;
        let in_t = input_type(r.w as u32, r.h as u32, p.fps)?;
        alloc.InitializeSampleAllocatorEx(4, 16, &aattrs, &in_t).ctx("InitializeSampleAllocatorEx")?;

        let composer =
            Composer { region: r, device, ctx, mt, ring, clean: clean.unwrap(), cursor, clicks, keys, alloc, _mgr: mgr.clone() };
        Ok((composer, mgr))
    }
}

/// What the encoder records, with `p`'s settings.
pub(super) fn video_config(p: &Params) -> VideoConfig {
    let r = p.region;
    VideoConfig { codec: p.codec, width: r.w as u32, height: r.h as u32, fps: p.fps, qp: p.qp, effort: p.effort }
}

pub(super) fn log_start(p: &Params, encoder: &str, tracks: usize) {
    let c = video_config(p);
    log(&format!(
        "recording {}x{}@{:.3} {:?} qp={} effort={}{} -> {} [{encoder}]",
        c.width,
        c.height,
        c.fps,
        c.codec,
        c.qp,
        c.effort,
        match tracks {
            0 => String::new(),
            n => format!(" + {n} aac track(s)"),
        },
        p.path.display()
    ));
}

/// The sink writer encoding and muxing in one, on the device behind `mgr`.
pub(super) unsafe fn sink_writer(
    p: &Params,
    mgr: &IMFDXGIDeviceManager,
    track_names: Vec<String>,
) -> Result<SinkWriter, String> {
    let tracks = track_names.len();
    let (writer, name) = unsafe { SinkWriter::new(mgr, &video_config(p), track_names, &p.path)? };
    log_start(p, &name, tracks);
    Ok(writer)
}

/// Uncompressed frames as the compositor makes them: ARGB32 textures.
pub(super) unsafe fn input_type(w: u32, h: u32, fps: f64) -> Result<IMFMediaType, String> {
    let fps_milli = (fps * 1000.0).round() as u32;
    unsafe {
        let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        in_t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).ctx("mt")?;
        Ok(in_t)
    }
}

impl SinkWriter {
    unsafe fn new(
        mgr: &IMFDXGIDeviceManager,
        config: &VideoConfig,
        track_names: Vec<String>,
        path: &Path,
    ) -> Result<(SinkWriter, String), String> {
        unsafe {
            let attrs = attributes(3)?;
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).ctx("attr")?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, mgr).ctx("attr")?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).ctx("attr")?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let writer =
                MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs).ctx("MFCreateSinkWriterFromURL")?;

            let (w, h) = (config.width, config.height);
            let (subtype, profile) = match config.codec {
                Codec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
                Codec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
            };
            // Only a header hint in constant-QP mode (~0.1 bit per pixel).
            let nominal_bps = ((w as f64 * h as f64 * config.fps) / 10.0).clamp(2e6, 400e6) as u32;
            let fps_milli = (config.fps * 1000.0).round() as u32;
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

            let in_t = input_type(w, h, config.fps)?;
            let enc = attributes(4)?;
            enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_Quality.0 as u32)
                .ctx("enc")?;
            enc.SetUINT64(&CODECAPI_AVEncVideoEncodeQP, config.qp.min(51) as u64).ctx("enc")?;
            enc.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, config.effort.min(100)).ctx("enc")?;
            enc.SetUINT32(&CODECAPI_AVEncMPVGOPSize, (config.fps * 2.0).round() as u32).ctx("enc")?;
            writer.SetInputMediaType(stream, &in_t, &enc).ctx("IMFSinkWriter::SetInputMediaType")?;
            let audio = track_names.iter().map(|_| add_audio_stream(&writer)).collect::<Result<Vec<u32>, String>>()?;

            let name = describe_encoder(&writer, stream);
            writer.BeginWriting().ctx("IMFSinkWriter::BeginWriting")?;
            let path = path.to_path_buf();
            Ok((SinkWriter { writer, stream, audio, track_names, path }, name))
        }
    }
}

impl Output for SinkWriter {
    type Frame = IMFSample;

    fn video(&mut self, sample: IMFSample, time: i64, duration: i64) -> Result<(), String> {
        unsafe {
            sample.SetSampleTime(time).ctx("SetSampleTime")?;
            sample.SetSampleDuration(duration).ctx("SetSampleDuration")?;
            self.writer.WriteSample(self.stream, &sample).ctx("WriteSample")
        }
    }

    fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) -> Result<(), String> {
        unsafe { write_pcm(&self.writer, self.audio[track], at, pcm) }
    }

    fn finish(&mut self) -> Result<(), String> {
        unsafe { self.writer.Finalize().ctx("IMFSinkWriter::Finalize")? };
        if !self.track_names.is_empty()
            && let Err(e) = name_tracks(&self.path, &self.track_names)
        {
            log(&format!("couldn't name the audio tracks: {e}"));
        }
        Ok(())
    }
}

impl Frames for Composer {
    type Image = usize;
    type Frame = IMFSample;

    fn latch(&mut self, &slot: &usize) -> Result<(), String> {
        let (tex, km) = &self.ring[slot];
        unsafe {
            self.mt.Enter();
            let r = km.AcquireSync(1, INFINITE);
            if r.is_ok() {
                self.ctx.CopyResource(&self.clean, tex);
                let _ = km.ReleaseSync(0);
            }
            self.mt.Leave();
            r.ctx("AcquireSync")
        }
    }

    fn set_pointer(&mut self, shape: &PointerShape) {
        if let Some(c) = self.cursor.as_mut()
            && let Err(e) = c.set_shape(&self.device, shape)
        {
            log(&format!("pointer shape update failed: {e}"));
        }
    }

    fn alloc(&mut self) -> Result<Option<IMFSample>, String> {
        match unsafe { self.alloc.AllocateSample() } {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => Ok(None),
            Err(e) => Err(format!("AllocateSample: {}", e.message())),
        }
    }

    /// Copies the clean region image into a pooled sample texture and draws the clicks, the key
    /// pills and the cursor on it (in that order; the cursor stays on top).
    fn compose(
        &mut self,
        sample: &IMFSample,
        cursor: Option<(i32, i32)>,
        clicks: &[Shape],
        keys: Option<KeyFrame>,
    ) -> Result<(), String> {
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

            let r = self.region;
            let pills = match (&self.keys, &keys) {
                (Some(kr), Some(kf)) => keys::pills(kf.keys, kf.t, kf.style, (r.w as f32, r.h as f32), kr),
                _ => Vec::new(),
            };
            let pointer = matches!((&self.cursor, cursor), (Some(c), Some(_)) if c.size().is_some());
            let ripples = self.clicks.is_some() && !clicks.is_empty();
            let rtv = if pointer || ripples || !pills.is_empty() {
                let mut desc = D3D11_RENDER_TARGET_VIEW_DESC {
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                    ..Default::default()
                };
                desc.Anonymous.Texture2DArray = D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: sub, ArraySize: 1 };
                let mut rtv = None;
                self.device.CreateRenderTargetView(&tex, Some(&desc), Some(&mut rtv)).ctx("CreateRenderTargetView")?;
                rtv
            } else {
                None
            };

            self.mt.Enter();
            self.ctx.CopySubresourceRegion(&tex, sub, 0, 0, 0, &self.clean, 0, None);
            let mut drawn = match (&rtv, &self.clicks) {
                (Some(rtv), Some(c)) => c.draw(&self.ctx, rtv, (r.w as u32, r.h as u32), clicks),
                _ => Ok(()),
            };
            if drawn.is_ok()
                && !pills.is_empty()
                && let (Some(rtv), Some(kr)) = (&rtv, &self.keys)
            {
                drawn = kr.draw(&self.ctx, rtv, (r.w as u32, r.h as u32), &pills);
            }
            if let (Some(rtv), Some(c), Some((x, y))) = (&rtv, &self.cursor, cursor) {
                c.draw(&self.ctx, rtv, r.w, r.h, x - r.x, y - r.y);
            }
            self.mt.Leave();
            drawn
        }
    }
}

unsafe fn add_audio_stream(writer: &IMFSinkWriter) -> Result<u32, String> {
    unsafe {
        let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        out_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29).ctx("mt")?;
        let stream = writer.AddStream(&out_t).ctx("IMFSinkWriter::AddStream (audio)")?;

        let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        in_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE * 4).ctx("mt")?;
        writer.SetInputMediaType(stream, &in_t, None).ctx("IMFSinkWriter::SetInputMediaType (audio)")?;
        Ok(stream)
    }
}

unsafe fn write_pcm(writer: &IMFSinkWriter, stream: u32, at: i64, data: &[i16]) -> Result<(), String> {
    if data.is_empty() {
        return Ok(());
    }
    unsafe {
        let bytes = std::mem::size_of_val(data);
        let buf = MFCreateMemoryBuffer(bytes as u32).ctx("MFCreateMemoryBuffer")?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None).ctx("IMFMediaBuffer::Lock")?;
        std::ptr::copy_nonoverlapping(data.as_ptr() as *const u8, p, bytes);
        let _ = buf.Unlock();
        buf.SetCurrentLength(bytes as u32).ctx("SetCurrentLength")?;
        let sample = MFCreateSample().ctx("MFCreateSample")?;
        sample.AddBuffer(&buf).ctx("AddBuffer")?;
        let end = at + (data.len() / 2) as i64;
        sample.SetSampleTime(frames_to_hns(at)).ctx("SetSampleTime")?;
        sample.SetSampleDuration(frames_to_hns(end) - frames_to_hns(at)).ctx("SetSampleDuration")?;
        writer.WriteSample(stream, &sample).ctx("WriteSample (audio)")
    }
}

pub(super) unsafe fn attributes(n: u32) -> Result<IMFAttributes, String> {
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
