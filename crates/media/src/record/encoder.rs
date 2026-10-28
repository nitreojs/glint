use std::path::{Path, PathBuf};

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use glint_core::record::Codec;
use glint_core::record::clicks::Shape;
use glint_core::record::effort;
use glint_core::record::keys::KeyFrame;
use glint_core::record::mix::{RATE, frames_to_hns};
use glint_core::record::mp4::{name_tracks, tag_colours};
use glint_core::record::negotiate::{Engine, Force, Input, Matrix, Refusal, Route, describe, log_tried, matrix, negotiate_rate, notice};
use glint_core::record::pipeline::{Frames, Output, VideoConfig};
use glint_core::record::pointer::PointerShape;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface};

use super::convert::Converter;
use super::cursor::CursorRenderer;
use super::gpu::{Gpu, friendly_name, hardware_encoders, plain, software_encoders, string};
use crate::clicks;
use crate::keys;
use super::win::{INFINITE, texture_desc};
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
    /// With NV12 samples: how frames get converted.
    nv12: Option<Nv12>,
    _mgr: IMFDXGIDeviceManager,
}

struct Nv12 {
    converter: Converter,
    /// The image the overlays are drawn on before it's converted (there when any overlay is
    /// on; a frame without overlays is converted straight from the clean image).
    canvas: Option<(ID3D11Texture2D, ID3D11RenderTargetView)>,
}

pub(super) struct SinkWriter {
    writer: IMFSinkWriter,
    matrix: Matrix,
    stream: u32,
    audio: Vec<u32>,
    track_names: Vec<String>,
    path: PathBuf,
}

impl Composer {
    /// The frame compositor on `gpu`'s device, reading the capture ring behind `handles`;
    /// frames are samples of [`input_type`] for `route`, from a pool of up to 16.
    pub(super) unsafe fn new(p: &Params, handles: &[HANDLE], gpu: Gpu, route: Route) -> Result<Composer, String> {
        unsafe {
            let (input, matrix) = (route.input, matrix(route, p.region.h as u32));
            let Gpu { device, ctx, mt, mgr, converter, .. } = gpu;
            let mut ring = Vec::with_capacity(handles.len());
            for &h in handles {
                let mut t: Option<ID3D11Texture2D> = None;
                device.OpenSharedResource(h, &mut t).ctx("OpenSharedResource")?;
                let t = t.unwrap();
                let km: IDXGIKeyedMutex = t.cast().ctx("IDXGIKeyedMutex")?;
                ring.push((t, km));
            }
            let r = p.region;
            let mut clean = None;
            device
                .CreateTexture2D(&texture_desc(r, D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, D3D11_RESOURCE_MISC_FLAG(0)), None, Some(&mut clean))
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

            let nv12 = match input {
                Input::Argb => None,
                Input::Nv12 => {
                    let mut converter = converter.ok_or("the GPU can't convert frames to NV12")?;
                    converter.set_matrix(matrix);
                    let canvas = if cursor.is_some() || clicks.is_some() || keys.is_some() {
                        let bind = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
                        let mut t = None;
                        device
                            .CreateTexture2D(&texture_desc(r, bind, D3D11_RESOURCE_MISC_FLAG(0)), None, Some(&mut t))
                            .ctx("CreateTexture2D (canvas)")?;
                        let t = t.unwrap();
                        let mut rtv = None;
                        device.CreateRenderTargetView(&t, None, Some(&mut rtv)).ctx("CreateRenderTargetView (canvas)")?;
                        Some((t, rtv.unwrap()))
                    } else {
                        None
                    };
                    Some(Nv12 { converter, canvas })
                }
            };

            let mut raw = std::ptr::null_mut();
            MFCreateVideoSampleAllocatorEx(&IMFVideoSampleAllocatorEx::IID, &mut raw).ctx("MFCreateVideoSampleAllocatorEx")?;
            let alloc = IMFVideoSampleAllocatorEx::from_raw(raw);
            alloc.SetDirectXManager(&mgr).ctx("SetDirectXManager")?;
            let in_t = input_type(r.w as u32, r.h as u32, p.fps, input, matrix)?;
            // NV12 textures are written by the video processor and read by the encoder, which
            // some drivers want to know about
            let binds: &[D3D11_BIND_FLAG] = match input {
                Input::Argb => &[D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE],
                Input::Nv12 => &[D3D11_BIND_RENDER_TARGET | D3D11_BIND_VIDEO_ENCODER, D3D11_BIND_RENDER_TARGET],
            };
            let mut pooled = Err(String::new());
            for bind in binds {
                let aattrs = attributes(2)?;
                aattrs.SetUINT32(&MF_SA_D3D11_BINDFLAGS, bind.0 as u32).ctx("attr")?;
                aattrs.SetUINT32(&MF_SA_D3D11_USAGE, D3D11_USAGE_DEFAULT.0 as u32).ctx("attr")?;
                pooled = alloc.InitializeSampleAllocatorEx(4, 16, &aattrs, &in_t).ctx("InitializeSampleAllocatorEx");
                if pooled.is_ok() {
                    break;
                }
            }
            pooled?;

            Ok(Composer { region: r, device, ctx, mt, ring, clean: clean.unwrap(), cursor, clicks, keys, alloc, nv12, _mgr: mgr })
        }
    }
}

/// What the encoder records, with `p`'s settings: the effort lowered where the slower presets
/// can't keep up (see [`effort::fit`]).
pub(super) fn video_config(p: &Params) -> VideoConfig {
    let r = p.region;
    let (width, height) = (r.w as u32, r.h as u32);
    VideoConfig {
        codec: p.codec,
        width,
        height,
        fps: p.fps,
        qp: p.qp,
        effort: effort::fit(p.effort, p.codec, width, height, p.fps),
        keyframe_secs: p.keyframe_secs,
        bitrate: p.bitrate,
    }
}

pub(super) fn log_start(p: &Params, encoder: &str, tracks: usize) {
    let c = video_config(p);
    log(&format!(
        "recording {}x{}@{:.3} {:?} {} effort={}{}{} -> {} [{encoder}]",
        c.width,
        c.height,
        c.fps,
        c.codec,
        match c.bitrate {
            Some(b) => format!("{:.1} Mbit/s", b as f64 / 1e6),
            None => format!("qp={}", c.qp),
        },
        c.effort,
        match c.effort < p.effort {
            true => format!(" (not {}: its preset can't keep up at this size and rate)", p.effort),
            false => String::new(),
        },
        match tracks {
            0 => String::new(),
            n => format!(" + {n} aac track(s)"),
        },
        p.path.display()
    ));
}

/// The sink writer encoding and muxing in one, on `gpu`: the first route to the encoder that
/// works (see [`negotiate_rate`]), with what to tell the user about it and the rate it records
/// at (lower than asked when no encoder takes that); the error says what was tried.
pub(super) unsafe fn sink_writer(
    p: &Params,
    gpu: &Gpu,
    force: Force,
    track_names: Vec<String>,
) -> Result<(SinkWriter, Route, Option<&'static str>, f64), String> {
    let config = video_config(p);
    let hardware: Vec<String> = unsafe { hardware_encoders(config.codec, &gpu.adapter) }.iter().map(|a| friendly_name(a)).collect();
    log(&format!("{} encoders on {}: {}", config.codec.name(), gpu.adapter.name, match hardware.is_empty() {
        true => "none".to_string(),
        false => hardware.join(", "),
    }));
    let attempt = |fps, route| {
        let config = VideoConfig { fps, ..config };
        unsafe { SinkWriter::new(gpu, &config, &track_names, &p.path, route, hardware.first().map(String::as_str)) }
    };
    match negotiate_rate(p.fps, &gpu.routes(force), attempt) {
        Ok(((writer, name), route, fps, tried)) => {
            log_tried(&tried);
            log(&format!("video: {}", describe(&name, &gpu.adapter.name, route)));
            log_start(&Params { fps, ..p.clone() }, &name, track_names.len());
            Ok((writer, route, notice(route, &tried, (config.width, config.height)), fps))
        }
        Err(failed) => {
            log_tried(&failed.tried);
            Err(failed.explain(&config, &gpu.adapter.name))
        }
    }
}

/// Frames between keyframes.
pub(super) fn gop(config: &VideoConfig) -> u32 {
    (config.fps * config.keyframe_secs).round().max(1.0) as u32
}

/// Uncompressed frames as the compositor makes them: ARGB32 textures, or NV12 ones the GPU
/// converted them to (limited range, `matrix`, like the stream's tags).
pub(super) unsafe fn input_type(w: u32, h: u32, fps: f64, input: Input, matrix: Matrix) -> Result<IMFMediaType, String> {
    let fps_milli = (fps * 1000.0).round() as u32;
    let (subtype, stride) = match input {
        Input::Argb => (MFVideoFormat_ARGB32, w * 4),
        Input::Nv12 => (MFVideoFormat_NV12, w),
    };
    unsafe {
        let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        in_t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_DEFAULT_STRIDE, stride).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).ctx("mt")?;
        if input == Input::Nv12 {
            colour_tags(&in_t, matrix)?;
        }
        Ok(in_t)
    }
}

/// BT.709 primaries and transfer, `matrix`, limited range: what glint's videos are.
pub(super) unsafe fn colour_tags(t: &IMFMediaType, matrix: Matrix) -> Result<(), String> {
    let matrix = match matrix {
        Matrix::Bt709 => MFVideoTransferMatrix_BT709,
        Matrix::Bt601 => MFVideoTransferMatrix_BT601,
    };
    unsafe {
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32).ctx("mt")?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32).ctx("mt")?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, matrix.0 as u32).ctx("mt")?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).ctx("mt")
    }
}

fn fatal<T>(r: Result<T, String>) -> Result<T, Refusal> {
    r.map_err(Refusal::Fatal)
}

impl SinkWriter {
    /// A sink writer whose encoder takes `route`; returns it with the encoder's name.
    /// `hardware`: the adapter's best hardware encoder, the one the sink writer should pick.
    unsafe fn new(
        gpu: &Gpu,
        config: &VideoConfig,
        track_names: &[String],
        path: &Path,
        route: Route,
        hardware: Option<&str>,
    ) -> Result<(SinkWriter, String), Refusal> {
        let codec = config.codec.name();
        unsafe {
            let expected = match route.engine {
                Engine::Hardware => match hardware {
                    Some(name) => name.to_string(),
                    None => return Err(Refusal::Missing(format!("{} has no {codec} encoder", gpu.adapter.name))),
                },
                Engine::Software => match software_encoders(config.codec).first() {
                    Some(a) => friendly_name(a),
                    None => return Err(Refusal::Missing(format!("Windows has no software {codec} encoder"))),
                },
            };
            let attrs = fatal(attributes(3))?;
            if route.engine == Engine::Hardware {
                // with the device's manager the sink writer picks that adapter's encoder
                fatal(attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).ctx("attr"))?;
                fatal(attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &gpu.mgr).ctx("attr"))?;
            }
            fatal(attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).ctx("attr"))?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let writer = fatal(
                MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs).ctx("MFCreateSinkWriterFromURL"),
            )?;

            let (w, h) = (config.width, config.height);
            let matrix = matrix(route, h);
            let (subtype, profile) = match config.codec {
                Codec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
                Codec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
            };
            // Only a header hint in constant-QP mode (~0.1 bit per pixel).
            let nominal_bps = config.nominal_bps();
            let fps_milli = (config.fps * 1000.0).round() as u32;
            let out_t = fatal(MFCreateMediaType().ctx("MFCreateMediaType"))?;
            let set_out = || -> Result<(), String> {
                out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
                out_t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
                out_t.SetUINT32(&MF_MT_AVG_BITRATE, nominal_bps).ctx("mt")?;
                out_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
                out_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
                out_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
                out_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
                out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile).ctx("mt")?;
                colour_tags(&out_t, matrix)
            };
            fatal(set_out())?;
            let stream = writer
                .AddStream(&out_t)
                .map_err(|e| Refusal::Missing(format!("{codec} can't be written here: {}", plain(&e, "AddStream"))))?;

            let in_t = fatal(input_type(w, h, config.fps, route.input, matrix))?;
            let enc = fatal(encoder_settings(config))?;
            writer
                .SetInputMediaType(stream, &in_t, &enc)
                .map_err(|e| Refusal::Format(format!("{expected} {}", plain(&e, "SetInputMediaType"))))?;
            let audio = fatal(track_names.iter().map(|_| add_audio_stream(&writer)).collect::<Result<Vec<u32>, String>>())?;

            let found = describe_encoder(&writer, stream, &expected);
            let name = found.as_ref().and_then(|f| f.name.clone()).unwrap_or_else(|| expected.clone());
            if let Some(f) = &found {
                // with hardware transforms on, the sink writer quietly takes the software
                // encoder when the hardware one refuses; that's a route of its own
                if route.engine == Engine::Hardware && !f.hardware {
                    let input = route.input.name();
                    let why = format!("{expected} refused {input} frames of {w}×{h}; Windows fell back to its software encoder");
                    return Err(Refusal::Format(why));
                }
                if let Some(v) = &f.vendor
                    && route.engine == Engine::Hardware
                    && !v.eq_ignore_ascii_case(&gpu.adapter.vendor_id())
                {
                    log(&format!("{name} is another GPU's encoder ({v}, the capture runs on {})", gpu.adapter.vendor_id()));
                }
            }
            writer.BeginWriting().map_err(|e| Refusal::Format(format!("{name} {}", plain(&e, "BeginWriting"))))?;
            let path = path.to_path_buf();
            Ok((SinkWriter { writer, matrix, stream, audio, track_names: track_names.to_vec(), path }, name))
        }
    }
}

/// Rate control, quality, speed and keyframes for the encoder, as `config` says.
unsafe fn encoder_settings(config: &VideoConfig) -> Result<IMFAttributes, String> {
    unsafe {
        let enc = attributes(4)?;
        match (config.bitrate, config.peak_bps()) {
            (Some(mean), Some(peak)) => {
                let mode = eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32;
                enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, mode).ctx("enc")?;
                enc.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, mean).ctx("enc")?;
                enc.SetUINT32(&CODECAPI_AVEncCommonMaxBitRate, peak).ctx("enc")?;
            }
            _ => {
                let mode = eAVEncCommonRateControlMode_Quality.0 as u32;
                enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, mode).ctx("enc")?;
                enc.SetUINT64(&CODECAPI_AVEncVideoEncodeQP, config.qp.min(51) as u64).ctx("enc")?;
            }
        }
        enc.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, config.effort.min(100)).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncMPVGOPSize, gop(config)).ctx("enc")?;
        Ok(enc)
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
        if let Err(e) = tag_colours(&self.path, self.matrix) {
            log(&format!("couldn't tag the video's colours: {e}"));
        }
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
    /// pills and the cursor on it (in that order; the cursor stays on top). NV12 samples get
    /// the image converted instead, the overlays drawn on the canvas before.
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
            let overlays = pointer || ripples || !pills.is_empty();
            let rtv = match (&self.nv12, overlays) {
                (_, false) => None,
                (Some(n), true) => n.canvas.as_ref().map(|(_, v)| v.clone()),
                (None, true) => {
                    let mut desc = D3D11_RENDER_TARGET_VIEW_DESC {
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                        ..Default::default()
                    };
                    desc.Anonymous.Texture2DArray = D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: sub, ArraySize: 1 };
                    let mut rtv = None;
                    self.device.CreateRenderTargetView(&tex, Some(&desc), Some(&mut rtv)).ctx("CreateRenderTargetView")?;
                    rtv
                }
            };

            self.mt.Enter();
            match &self.nv12 {
                None => self.ctx.CopySubresourceRegion(&tex, sub, 0, 0, 0, &self.clean, 0, None),
                Some(Nv12 { canvas: Some((canvas, _)), .. }) if rtv.is_some() => self.ctx.CopyResource(canvas, &self.clean),
                Some(_) => {}
            }
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
            if drawn.is_ok()
                && let Some(Nv12 { converter, canvas }) = &mut self.nv12
            {
                let src = match canvas {
                    Some((canvas, _)) if rtv.is_some() => &*canvas,
                    _ => &self.clean,
                };
                drawn = converter.convert(src, &tex, sub);
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

/// The video encoder MFT the sink writer picked.
struct Picked {
    /// Software MFTs don't always say.
    name: Option<String>,
    /// A GPU's encoder (it has a hardware URL).
    hardware: bool,
    /// Its vendor, `VEN_10DE`, when it says.
    vendor: Option<String>,
}

/// The encoder MFT the sink writer picked for `stream` (`expected` when it has no name); logs its
/// effective settings.
unsafe fn describe_encoder(writer: &IMFSinkWriter, stream: u32, expected: &str) -> Option<Picked> {
    unsafe {
        let ex = writer.cast::<IMFSinkWriterEx>().ok()?;
        let mut i = 0;
        loop {
            let mut cat = GUID::zeroed();
            let mut t = None;
            ex.GetTransformForStream(stream, i, Some(&mut cat), &mut t).ok()?;
            i += 1;
            let Some(t) = t else { continue };
            if cat != MFT_CATEGORY_VIDEO_ENCODER {
                continue;
            }
            let mut picked = Picked { name: None, hardware: false, vendor: None };
            if let Ok(a) = t.GetAttributes() {
                picked.name = string(&a, &MFT_FRIENDLY_NAME_Attribute);
                picked.hardware = string(&a, &MFT_ENUM_HARDWARE_URL_Attribute).is_some();
                picked.vendor = string(&a, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute);
            }
            if let Ok(api) = t.cast::<ICodecAPI>() {
                let get = |g: &GUID| api.GetValue(g).ok().map(|v| variant_u64(&v));
                log(&format!(
                    "{}: rc={:?} qp={:?} effort={:?} gop={:?}",
                    picked.name.as_deref().unwrap_or(expected),
                    get(&CODECAPI_AVEncCommonRateControlMode),
                    get(&CODECAPI_AVEncVideoEncodeQP),
                    get(&CODECAPI_AVEncCommonQualityVsSpeed),
                    get(&CODECAPI_AVEncMPVGOPSize),
                ));
            }
            return Some(picked);
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
