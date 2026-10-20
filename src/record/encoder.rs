use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Performance::QueryPerformanceFrequency;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use super::capture::Capture;
use super::cursor::CursorRenderer;
use super::{Codec, Ctx, INFINITE, Msg, Params, RING, adapter_and_output, create_device, qpc, texture_desc};
use crate::platform::log::log;

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

pub(super) struct Encoder {
    pub(super) p: Params,
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
    pub(super) unsafe fn new(p: Params, cap: &Capture) -> Result<Self, String> {
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
    pub(super) unsafe fn run(&mut self, rx: &Receiver<Msg>, free_tx: &Sender<usize>, abort: &AtomicBool) -> Result<(u64, u64, f64), String> {
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
