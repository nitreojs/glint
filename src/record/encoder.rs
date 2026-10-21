use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface, PWSTR};

use super::capture::Capture;
use super::cursor::CursorRenderer;
use super::mix::{RATE, frames_to_hns};
use super::mp4::name_tracks;
use super::timeline::{AudioPlacer, Pauses};
use super::{Codec, Ctx, INFINITE, Msg, Params, RING, create_device, hns, lock, qpc, texture_desc};
use crate::platform::log::log;

/// 192 kbps, one of the four rates the media foundation aac encoder accepts.
const AAC_BYTES_PER_SECOND: u32 = 24_000;

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
    audio: Vec<u32>,
    track_names: Vec<String>,
    alloc: IMFVideoSampleAllocatorEx,
    _mgr: IMFDXGIDeviceManager,
}

struct Video {
    grid: f64,
    start: Option<i64>,
    /// The newest frame and its slot; it is handed to the encoder once a later slot begins.
    pending: Option<(IMFSample, i64)>,
    have_image: bool,
    frames: u64,
    dropped: u64,
    cursor: Option<(i32, i32)>,
    stale: bool,
}

impl Video {
    fn slot_time(&self, slot: i64) -> i64 {
        (slot as f64 * self.grid).round() as i64
    }

    fn slot(&self, t: i64) -> i64 {
        self.start.map_or(0, |s| (((t - s) as f64 / self.grid).round() as i64).max(0))
    }
}

impl Encoder {
    pub(super) unsafe fn new(p: Params, cap: &Capture, track_names: Vec<String>) -> Result<Self, String> {
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
            let audio = track_names.iter().map(|_| add_audio_stream(&writer)).collect::<Result<Vec<u32>, String>>()?;

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

            log(&format!(
                "recording {w}x{h}@{:.3} {:?} qp={} effort={}{} -> {} [{name}]",
                p.fps,
                p.codec,
                p.qp,
                p.effort,
                match audio.len() {
                    0 => String::new(),
                    n => format!(" + {n} aac track(s)"),
                },
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
                audio,
                track_names,
                alloc,
                _mgr: mgr,
            })
        }
    }

    /// Consumes capture and audio messages until both have ended, then finalizes the file
    /// (skipped when `abort` is set: the file is going to be deleted).
    /// Returns (frames written, frames dropped, duration in seconds).
    pub(super) unsafe fn run(
        &mut self,
        rx: &Receiver<Msg>,
        free_tx: &Sender<usize>,
        abort: &AtomicBool,
        pauses: &Mutex<Pauses>,
    ) -> Result<(u64, u64, f64), String> {
        unsafe {
            // Capture times jitter by a millisecond or two around vsync (which would show up as
            // judder), cursor-only updates arrive between vsyncs, and a capped fps means several
            // captures per slot: everything that lands in the same slot is merged into one
            // frame, the latest state wins.
            let mut v = Video {
                grid: 1e7 / self.p.fps,
                start: None,
                pending: None,
                have_image: false,
                frames: 0,
                dropped: 0,
                cursor: None,
                stale: false,
            };
            let mut placer = AudioPlacer::default();
            let mut early_audio: Vec<(i64, Vec<Vec<i16>>)> = Vec::new();
            let mut audio_open = !self.audio.is_empty();
            let mut end_qpc = None;

            loop {
                let msg = match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                match msg {
                    None => {}
                    Some(Msg::Shape(si, buf)) => {
                        if let Some(c) = self.cursor.as_mut()
                            && let Err(e) = c.set_shape(&self.device, &si, &buf)
                        {
                            log(&format!("pointer shape update failed: {e}"));
                        }
                    }
                    Some(Msg::End { qpc }) => {
                        end_qpc = Some(qpc);
                        if !audio_open {
                            break;
                        }
                    }
                    Some(Msg::AudioEnd) => {
                        audio_open = false;
                        if end_qpc.is_some() {
                            break;
                        }
                    }
                    Some(Msg::Audio { start, tracks }) => match v.start {
                        Some(origin) => {
                            let at = placer.next;
                            let out = placer.place(start, &tracks, &lock(pauses), origin);
                            self.write_audio(at, &out)?;
                        }
                        None => early_audio.push((start, tracks)),
                    },
                    Some(Msg::Frame { slot: ring_slot, qpc, cursor }) => {
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
                            v.have_image = true;
                        }
                        v.cursor = cursor;
                        if !v.have_image {
                            continue;
                        }
                        let t = lock(pauses).active(hns(qpc));
                        match t {
                            Some(t) => self.frame(&mut v, t)?,
                            None => v.stale = true,
                        }
                    }
                }
                if v.stale && end_qpc.is_none() {
                    let t = lock(pauses).active(hns(qpc()));
                    if let Some(t) = t {
                        v.stale = false;
                        self.frame(&mut v, t)?;
                    }
                }
                if let Some(origin) = v.start
                    && !early_audio.is_empty()
                {
                    for (start, tracks) in std::mem::take(&mut early_audio) {
                        let at = placer.next;
                        let out = placer.place(start, &tracks, &lock(pauses), origin);
                        self.write_audio(at, &out)?;
                    }
                }
            }

            if abort.load(Ordering::SeqCst) {
                return Ok((v.frames, v.dropped, 0.0));
            }
            let mut duration_s = 0.0;
            if let Some((prev, ps)) = v.pending.take() {
                let end = lock(pauses).active_clamped(hns(end_qpc.unwrap_or_else(qpc)));
                let end_slot = v.slot(end).max(ps + 1);
                prev.SetSampleDuration(v.slot_time(end_slot) - v.slot_time(ps)).ctx("SetSampleDuration")?;
                self.writer.WriteSample(self.stream, &prev).ctx("WriteSample")?;
                v.frames += 1;
                duration_s = v.slot_time(end_slot) as f64 / 1e7;
                let at = placer.next;
                let pad = vec![0; 2 * placer.pad(v.slot_time(end_slot))];
                self.write_audio(at, &vec![pad; self.audio.len()])?;
            }
            if v.frames == 0 {
                return Err("no frames were captured".into());
            }
            self.writer.Finalize().ctx("IMFSinkWriter::Finalize")?;
            if !self.track_names.is_empty()
                && let Err(e) = name_tracks(&self.p.path, &self.track_names)
            {
                log(&format!("couldn't name the audio tracks: {e}"));
            }
            Ok((v.frames, v.dropped, duration_s))
        }
    }

    unsafe fn frame(&self, v: &mut Video, t: i64) -> Result<(), String> {
        unsafe {
            v.start.get_or_insert(t);
            let slot = v.slot(t);
            match &v.pending {
                Some((s, ps)) if slot <= *ps => self.compose(s, v.cursor)?,
                _ => {
                    let sample = match self.alloc.AllocateSample() {
                        Ok(s) => s,
                        Err(e) if e.code() == MF_E_SAMPLEALLOCATOR_EMPTY => {
                            v.dropped += 1;
                            return Ok(());
                        }
                        Err(e) => return Err(format!("AllocateSample: {}", e.message())),
                    };
                    self.compose(&sample, v.cursor)?;
                    sample.SetSampleTime(v.slot_time(slot)).ctx("SetSampleTime")?;
                    if let Some((prev, ps)) = v.pending.take() {
                        prev.SetSampleDuration(v.slot_time(slot) - v.slot_time(ps)).ctx("SetSampleDuration")?;
                        self.writer.WriteSample(self.stream, &prev).ctx("WriteSample")?;
                        v.frames += 1;
                    }
                    v.pending = Some((sample, slot));
                }
            }
            Ok(())
        }
    }

    unsafe fn write_audio(&self, at: i64, tracks: &[Vec<i16>]) -> Result<(), String> {
        for (&stream, data) in self.audio.iter().zip(tracks) {
            unsafe { write_pcm(&self.writer, stream, at, data)? };
        }
        Ok(())
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
