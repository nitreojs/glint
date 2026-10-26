//! The packet path on Windows: the video encoder MFT driven directly ([`MfVideoEncoder`]),
//! Media Foundation's AAC encoder ([`AacEncoder`]) and an MP4 writer that takes their packets
//! as they are ([`Mp4Sink`], a sink writer in passthrough). Joined by
//! [`glint_core::record::pipeline::Encoded`], they replace the sink writer's encode-and-mux in
//! one step, so encoded packets can go elsewhere first (afterglow's replay buffer).

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use glint_core::log::log;
use glint_core::record::Codec;
use glint_core::record::mix::{RATE, frames_to_hns};
use glint_core::record::mp4::{name_tracks, tag_colours};
use glint_core::record::pipeline::{AudioEncoder, Packet, PacketSink, Stream, VideoConfig, VideoEncoder};
use windows::Win32::Media::MediaFoundation::*;
use glint_core::record::negotiate::{Engine, Force, Matrix, Refusal, Route, describe, log_tried, matrix, negotiate_rate, notice};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{GUID, HSTRING, Interface};

use super::Ctx;
use super::encoder::{AAC_BYTES_PER_SECOND, attributes, colour_tags, gop, input_type, pack};
use super::gpu::{Adapter, Gpu, friendly_name, hardware_encoders, plain, software_encoders, subtype_name};

/// The fields of `HEAACWAVEINFO` after `WAVEFORMATEX` that MF keeps in `MF_MT_USER_DATA` before the
/// AudioSpecificConfig: raw AAC (payload type 0), AAC-LC level 2 (0x29).
const AAC_INFO: [u8; 12] = [0, 0, 0x29, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn video_subtype(codec: Codec) -> (windows::core::GUID, u32) {
    match codec {
        Codec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
        Codec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
    }
}

/// The compressed video type both the encoder's output and the MP4 stream use.
unsafe fn video_type(config: &VideoConfig, matrix: Matrix) -> Result<IMFMediaType, String> {
    let (w, h) = (config.width, config.height);
    let (subtype, profile) = video_subtype(config.codec);
    let nominal_bps = config.nominal_bps();
    let fps_milli = (config.fps * 1000.0).round() as u32;
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, nominal_bps).ctx("mt")?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
        t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile).ctx("mt")?;
        colour_tags(&t, matrix)?;
        Ok(t)
    }
}

/// AAC-LC, 48 kHz stereo; `config` is the AudioSpecificConfig, when known.
unsafe fn aac_type(config: Option<&[u8]>) -> Result<IMFMediaType, String> {
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND).ctx("mt")?;
        t.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0).ctx("mt")?;
        t.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29).ctx("mt")?;
        if let Some(asc) = config {
            t.SetBlob(&MF_MT_USER_DATA, &[&AAC_INFO[..], asc].concat()).ctx("mt")?;
        }
        Ok(t)
    }
}

unsafe fn pcm_type() -> Result<IMFMediaType, String> {
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).ctx("mt")?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, RATE * 4).ctx("mt")?;
        Ok(t)
    }
}

unsafe fn blob(t: &IMFMediaType, key: &windows::core::GUID) -> Vec<u8> {
    unsafe {
        let Ok(size) = t.GetBlobSize(key) else { return Vec::new() };
        let mut buf = vec![0u8; size as usize];
        match t.GetBlob(key, &mut buf, None) {
            Ok(()) => buf,
            Err(_) => Vec::new(),
        }
    }
}

/// A packet holding `sample`'s bytes; video says whether it's a keyframe, audio frames all are.
unsafe fn packet(sample: &IMFSample, video: bool) -> Result<Packet, String> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().ctx("ConvertToContiguousBuffer")?;
        let mut p = std::ptr::null_mut();
        let mut len = 0;
        buf.Lock(&mut p, None, Some(&mut len)).ctx("IMFMediaBuffer::Lock")?;
        let data = std::slice::from_raw_parts(p, len as usize).to_vec();
        let _ = buf.Unlock();
        let pts = sample.GetSampleTime().ctx("GetSampleTime")?;
        let duration = sample.GetSampleDuration().unwrap_or(0);
        let dts = sample.GetUINT64(&MFSampleExtension_DecodeTimestamp).map_or(pts, |d| d as i64);
        let key = !video || sample.GetUINT32(&MFSampleExtension_CleanPoint).is_ok_and(|v| v != 0);
        Ok(Packet { data, pts, dts, duration, key })
    }
}

/// Takes the sample (and drops the events) an output call filled in.
fn take(mut buf: MFT_OUTPUT_DATA_BUFFER) -> Option<IMFSample> {
    drop(unsafe { ManuallyDrop::take(&mut buf.pEvents) });
    unsafe { ManuallyDrop::take(&mut buf.pSample) }
}

/// The input subtypes an encoder offers once its output type is set.
unsafe fn input_subtypes(mft: &IMFTransform) -> Vec<GUID> {
    (0..64).map_while(|i| unsafe { mft.GetInputAvailableType(0, i) }.ok()).filter_map(|t| unsafe { t.GetGUID(&MF_MT_SUBTYPE) }.ok()).collect()
}

/// An encoder MFT, set up.
struct Opened {
    activate: IMFActivate,
    mft: IMFTransform,
    /// Asynchronous (hardware) encoders say when they want input and have output; synchronous
    /// (software) ones are called in turn.
    events: Option<IMFMediaEventGenerator>,
    /// The sample a synchronous encoder fills, reused, unless it brings its own.
    out: Option<(IMFSample, IMFMediaBuffer)>,
}

/// A video encoder MFT fed D3D11 textures from the capturing GPU's device: that GPU's hardware
/// encoder (asynchronous) on ARGB32 or NV12 frames, or when none takes them, Windows' software
/// encoder (synchronous); see [`negotiate`]. Constant QP or a bitrate target like the sink
/// writer path, no B-frames (packets come out in display order, so decode time equals
/// presentation time), keyframes as the config says.
pub struct MfVideoEncoder {
    mgr: IMFDXGIDeviceManager,
    adapter: Adapter,
    routes: Vec<Route>,
    opened: Option<Opened>,
    /// Inputs an asynchronous encoder has asked for and not been given yet.
    wanted: u32,
    name: String,
    route: Option<Route>,
    notice: Option<&'static str>,
    /// The rate it was configured at: lower than asked when no encoder took that.
    fps: f64,
}

impl MfVideoEncoder {
    /// An encoder for `gpu`'s frames, trying the routes `force` leaves.
    pub(super) fn new(gpu: &Gpu, force: Force) -> MfVideoEncoder {
        MfVideoEncoder {
            mgr: gpu.mgr.clone(),
            adapter: gpu.adapter.clone(),
            routes: gpu.routes(force),
            opened: None,
            wanted: 0,
            name: String::new(),
            route: None,
            notice: None,
            fps: 0.0,
        }
    }

    /// The encoder MFT's friendly name, once configured.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The engine and input format, once configured.
    pub fn route(&self) -> Option<Route> {
        self.route
    }

    /// What to tell the user about the route, once configured.
    pub fn notice(&self) -> Option<&'static str> {
        self.notice
    }

    /// The rate it encodes at, once configured.
    pub fn fps(&self) -> f64 {
        self.fps
    }

    fn parts(&self) -> Result<&Opened, String> {
        self.opened.as_ref().ok_or_else(|| "the video encoder isn't configured".to_string())
    }

    /// The first encoder of `route`'s engine that takes our device, types and settings.
    unsafe fn open_route(&self, config: &VideoConfig, route: Route) -> Result<(Opened, String, Vec<u8>), Refusal> {
        let codec = config.codec.name();
        let list = unsafe {
            match route.engine {
                Engine::Hardware => hardware_encoders(config.codec, &self.adapter),
                Engine::Software => software_encoders(config.codec),
            }
        };
        if list.is_empty() {
            return Err(Refusal::Missing(match route.engine {
                Engine::Hardware => format!("{} has no {codec} encoder", self.adapter.name),
                Engine::Software => format!("Windows has no software {codec} encoder"),
            }));
        }
        let mut why = String::new();
        for a in list {
            let name = friendly_name(&a);
            match unsafe { self.try_open(&a, config, route) } {
                Ok((mft, events, out, headers)) => return Ok((Opened { activate: a, mft, events, out }, name, headers)),
                Err(e) => {
                    let _ = unsafe { a.ShutdownObject() };
                    // the best encoder's reason is the one worth telling
                    if why.is_empty() {
                        why = format!("{name} {e}");
                    }
                }
            }
        }
        Err(Refusal::Format(why))
    }

    #[allow(clippy::type_complexity)]
    unsafe fn try_open(
        &self,
        a: &IMFActivate,
        config: &VideoConfig,
        route: Route,
    ) -> Result<(IMFTransform, Option<IMFMediaEventGenerator>, Option<(IMFSample, IMFMediaBuffer)>, Vec<u8>), String> {
        unsafe {
            let mft: IMFTransform = a.ActivateObject().map_err(|e| plain(&e, "ActivateObject"))?;
            let attrs = mft.GetAttributes().ok();
            let flag = |key: &GUID| attrs.as_ref().is_some_and(|a| a.GetUINT32(key).is_ok_and(|v| v != 0));
            let asynchronous = flag(&MF_TRANSFORM_ASYNC);
            if let (true, Some(attrs)) = (asynchronous, &attrs) {
                attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(|e| plain(&e, "MF_TRANSFORM_ASYNC_UNLOCK"))?;
            }
            if route.engine == Engine::Hardware || flag(&MF_SA_D3D11_AWARE) {
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, self.mgr.as_raw() as usize)
                    .map_err(|e| plain(&e, "MFT_MESSAGE_SET_D3D_MANAGER"))?;
            }
            let api: ICodecAPI = mft.cast().map_err(|e| plain(&e, "ICodecAPI"))?;
            let set = |key: &GUID, v: VARIANT, what: &str| api.SetValue(key, &v).map_err(|e| plain(&e, what));
            match (config.bitrate, config.peak_bps()) {
                (Some(mean), Some(peak)) => {
                    let mode = eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32;
                    set(&CODECAPI_AVEncCommonRateControlMode, mode.into(), "rate control")?;
                    set(&CODECAPI_AVEncCommonMeanBitRate, mean.into(), "bitrate")?;
                    set(&CODECAPI_AVEncCommonMaxBitRate, peak.into(), "peak bitrate")?;
                }
                _ => {
                    let quality = eAVEncCommonRateControlMode_Quality.0 as u32;
                    set(&CODECAPI_AVEncCommonRateControlMode, quality.into(), "rate control")?;
                    set(&CODECAPI_AVEncVideoEncodeQP, (config.qp.min(51) as u64).into(), "qp")?;
                }
            }
            // the rest are hints some encoders don't take
            let _ = set(&CODECAPI_AVEncCommonQualityVsSpeed, config.effort.min(100).into(), "effort");
            let _ = set(&CODECAPI_AVEncMPVGOPSize, gop(config).into(), "gop");
            let _ = set(&CODECAPI_AVEncMPVDefaultBPictureCount, 0u32.into(), "b-frames");
            let matrix = matrix(route, config.height);
            mft.SetOutputType(0, &video_type(config, matrix)?, 0).map_err(|e| plain(&e, "SetOutputType"))?;
            let in_t = input_type(config.width, config.height, config.fps, route.input, matrix)?;
            if let Err(e) = mft.SetInputType(0, &in_t, 0) {
                let takes = input_subtypes(&mft);
                let wanted = in_t.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default();
                return Err(match takes.is_empty() || takes.contains(&wanted) {
                    true => plain(&e, "SetInputType"),
                    false => {
                        let names: Vec<String> = takes.iter().map(subtype_name).collect();
                        format!("takes {} frames, not {} ({})", names.join("/"), route.input.name(), plain(&e, "SetInputType"))
                    }
                });
            }
            let headers = blob(&mft.GetOutputCurrentType(0).map_err(|e| plain(&e, "GetOutputCurrentType"))?, &MF_MT_MPEG_SEQUENCE_HEADER);
            let out = match asynchronous {
                true => None,
                false => {
                    let info = mft.GetOutputStreamInfo(0).map_err(|e| plain(&e, "GetOutputStreamInfo"))?;
                    let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
                    match info.dwFlags & provides != 0 {
                        true => None,
                        false => {
                            // room for a frame that doesn't compress at all
                            let size = info.cbSize.max(config.width * config.height * 3 / 2 + 4096);
                            let buf = MFCreateMemoryBuffer(size).map_err(|e| plain(&e, "MFCreateMemoryBuffer"))?;
                            let sample = MFCreateSample().map_err(|e| plain(&e, "MFCreateSample"))?;
                            sample.AddBuffer(&buf).map_err(|e| plain(&e, "AddBuffer"))?;
                            Some((sample, buf))
                        }
                    }
                }
            };
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(|e| plain(&e, "NOTIFY_BEGIN_STREAMING"))?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(|e| plain(&e, "NOTIFY_START_OF_STREAM"))?;
            let events = match asynchronous {
                true => Some(mft.cast::<IMFMediaEventGenerator>().map_err(|e| plain(&e, "IMFMediaEventGenerator"))?),
                false => None,
            };
            Ok((mft, events, out, headers))
        }
    }

    /// Handles one event of an asynchronous encoder: an input request is counted, an output
    /// becomes a packet. Returns false once draining has finished. With `wait` false, returns
    /// `Ok(None)` when no event is queued.
    unsafe fn event(&mut self, wait: bool, out: &mut Vec<Packet>) -> Result<Option<bool>, String> {
        let opened = self.parts()?;
        let mft = opened.mft.clone();
        let events = opened.events.clone().ok_or("a synchronous encoder has no events")?;
        let flags = if wait { MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0) } else { MF_EVENT_FLAG_NO_WAIT };
        unsafe {
            let e = match events.GetEvent(flags) {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(None),
                Err(e) => return Err(format!("IMFMediaEventGenerator::GetEvent: {}", e.message())),
            };
            let kind = e.GetType().ctx("IMFMediaEvent::GetType")?;
            if kind == METransformNeedInput.0 as u32 {
                self.wanted += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                let mut buf = [MFT_OUTPUT_DATA_BUFFER::default()];
                let mut status = 0;
                match mft.ProcessOutput(0, &mut buf, &mut status) {
                    Ok(()) => {
                        let [b] = buf;
                        if let Some(sample) = take(b) {
                            out.push(packet(&sample, true)?);
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let [b] = buf;
                        drop(take(b));
                        let t = mft.GetOutputAvailableType(0, 0).ctx("GetOutputAvailableType")?;
                        mft.SetOutputType(0, &t, 0).ctx("SetOutputType (stream change)")?;
                    }
                    Err(e) => return Err(format!("IMFTransform::ProcessOutput: {}", e.message())),
                }
            } else if kind == METransformDrainComplete.0 as u32 {
                return Ok(Some(false));
            }
            Ok(Some(true))
        }
    }

    /// Every packet a synchronous encoder has ready.
    unsafe fn drain(&mut self, out: &mut Vec<Packet>) -> Result<(), String> {
        let opened = self.parts()?;
        unsafe {
            loop {
                let sample = match &opened.out {
                    Some((sample, buf)) => {
                        buf.SetCurrentLength(0).ctx("SetCurrentLength")?;
                        // the last packet's flags (a keyframe's clean point) mustn't stick
                        sample.DeleteAllItems().ctx("DeleteAllItems")?;
                        Some(sample.clone())
                    }
                    None => None,
                };
                let mut buf = [MFT_OUTPUT_DATA_BUFFER { pSample: ManuallyDrop::new(sample), ..Default::default() }];
                let mut status = 0;
                let r = opened.mft.ProcessOutput(0, &mut buf, &mut status);
                let [b] = buf;
                let got = take(b);
                match r {
                    Ok(()) => {
                        if let Some(sample) = got {
                            out.push(packet(&sample, true)?);
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        let t = opened.mft.GetOutputAvailableType(0, 0).ctx("GetOutputAvailableType")?;
                        opened.mft.SetOutputType(0, &t, 0).ctx("SetOutputType (stream change)")?;
                    }
                    Err(e) => return Err(format!("IMFTransform::ProcessOutput: {}", e.message())),
                }
            }
        }
    }
}

impl VideoEncoder for MfVideoEncoder {
    type Frame = IMFSample;

    fn configure(&mut self, config: &VideoConfig) -> Result<Stream, String> {
        let routes = self.routes.clone();
        let attempt = |fps, route| {
            let config = VideoConfig { fps, ..*config };
            unsafe { self.open_route(&config, route) }
        };
        match negotiate_rate(config.fps, &routes, attempt) {
            Ok(((opened, name, headers), route, fps, tried)) => {
                log_tried(&tried);
                log(&format!("video: {}", describe(&name, &self.adapter.name, route)));
                self.opened = Some(opened);
                self.name = name;
                self.route = Some(route);
                self.notice = notice(route, &tried, (config.width, config.height));
                self.fps = fps;
                Ok(Stream::Video { config: VideoConfig { fps, ..*config }, headers, matrix: matrix(route, config.height) })
            }
            Err(failed) => {
                log_tried(&failed.tried);
                Err(failed.explain(config, &self.adapter.name))
            }
        }
    }

    fn encode(&mut self, frame: IMFSample, time: i64, duration: i64, out: &mut Vec<Packet>) -> Result<(), String> {
        unsafe {
            frame.SetSampleTime(time).ctx("SetSampleTime")?;
            frame.SetSampleDuration(duration).ctx("SetSampleDuration")?;
            let mft = self.parts()?.mft.clone();
            if self.parts()?.events.is_none() {
                if let Err(e) = mft.ProcessInput(0, &frame, 0) {
                    if e.code() != MF_E_NOTACCEPTING {
                        return Err(format!("IMFTransform::ProcessInput: {}", e.message()));
                    }
                    self.drain(out)?;
                    mft.ProcessInput(0, &frame, 0).ctx("IMFTransform::ProcessInput")?;
                }
                return self.drain(out);
            }
            while self.wanted == 0 {
                self.event(true, out)?;
            }
            self.wanted -= 1;
            mft.ProcessInput(0, &frame, 0).ctx("IMFTransform::ProcessInput")?;
            // whatever is ready now; the rest comes with the next frame
            while self.event(false, out)?.is_some() {}
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), String> {
        unsafe {
            let mft = self.parts()?.mft.clone();
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0).ctx("NOTIFY_END_OF_STREAM")?;
            mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).ctx("COMMAND_DRAIN")?;
            if self.parts()?.events.is_some() {
                while self.event(true, out)? != Some(false) {}
            } else {
                self.drain(out)?;
            }
            let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
        Ok(())
    }
}

impl Drop for MfVideoEncoder {
    fn drop(&mut self) {
        if let Some(Opened { activate, mft, .. }) = self.opened.take() {
            unsafe {
                if let Ok(s) = mft.cast::<IMFShutdown>() {
                    let _ = s.Shutdown();
                }
                let _ = activate.ShutdownObject();
            }
        }
    }
}

/// Media Foundation's AAC encoder (synchronous): 48 kHz stereo 16-bit PCM in, AAC-LC at 192
/// kbit/s out, one packet per 1024-sample frame.
pub struct AacEncoder {
    mft: IMFTransform,
    /// The output sample the encoder fills, reused.
    out: IMFSample,
    out_buf: IMFMediaBuffer,
}

impl AacEncoder {
    pub fn new() -> Result<AacEncoder, String> {
        unsafe {
            let mft: IMFTransform =
                CoCreateInstance(&AACMFTEncoder, None, CLSCTX_INPROC_SERVER).ctx("CoCreateInstance (aac encoder)")?;
            mft.SetInputType(0, &pcm_type()?, 0).ctx("SetInputType (pcm)")?;
            mft.SetOutputType(0, &aac_type(None)?, 0).ctx("SetOutputType (aac)")?;
            let info = mft.GetOutputStreamInfo(0).ctx("GetOutputStreamInfo")?;
            let out_buf = MFCreateMemoryBuffer(info.cbSize.max(8192)).ctx("MFCreateMemoryBuffer")?;
            let out = MFCreateSample().ctx("MFCreateSample")?;
            out.AddBuffer(&out_buf).ctx("AddBuffer")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).ctx("NOTIFY_BEGIN_STREAMING")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).ctx("NOTIFY_START_OF_STREAM")?;
            Ok(AacEncoder { mft, out, out_buf })
        }
    }

    /// Every frame the encoder has ready.
    unsafe fn drain(&mut self, out: &mut Vec<Packet>) -> Result<(), String> {
        unsafe {
            loop {
                self.out_buf.SetCurrentLength(0).ctx("SetCurrentLength")?;
                let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                    pSample: ManuallyDrop::new(Some(self.out.clone())),
                    ..Default::default()
                }];
                let mut status = 0;
                let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
                let [b] = buf;
                drop(take(b));
                match r {
                    Ok(()) => out.push(packet(&self.out, false)?),
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) => return Err(format!("IMFTransform::ProcessOutput (aac): {}", e.message())),
                }
            }
        }
    }
}

impl AudioEncoder for AacEncoder {
    fn configure(&mut self, name: &str) -> Result<Stream, String> {
        let user = unsafe { blob(&self.mft.GetOutputCurrentType(0).ctx("GetOutputCurrentType (aac)")?, &MF_MT_USER_DATA) };
        let headers = user.get(AAC_INFO.len()..).unwrap_or_default().to_vec();
        Ok(Stream::Audio { name: name.to_string(), headers })
    }

    fn encode(&mut self, at: i64, pcm: &[i16], out: &mut Vec<Packet>) -> Result<(), String> {
        if pcm.is_empty() {
            return Ok(());
        }
        unsafe {
            let bytes = std::mem::size_of_val(pcm);
            let buf = MFCreateMemoryBuffer(bytes as u32).ctx("MFCreateMemoryBuffer")?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).ctx("IMFMediaBuffer::Lock")?;
            std::ptr::copy_nonoverlapping(pcm.as_ptr() as *const u8, p, bytes);
            let _ = buf.Unlock();
            buf.SetCurrentLength(bytes as u32).ctx("SetCurrentLength")?;
            let sample = MFCreateSample().ctx("MFCreateSample")?;
            sample.AddBuffer(&buf).ctx("AddBuffer")?;
            let end = at + (pcm.len() / 2) as i64;
            sample.SetSampleTime(frames_to_hns(at)).ctx("SetSampleTime")?;
            sample.SetSampleDuration(frames_to_hns(end) - frames_to_hns(at)).ctx("SetSampleDuration")?;
            self.mft.ProcessInput(0, &sample, 0).ctx("IMFTransform::ProcessInput (aac)")?;
            self.drain(out)
        }
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), String> {
        unsafe {
            self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0).ctx("NOTIFY_END_OF_STREAM (aac)")?;
            self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).ctx("COMMAND_DRAIN (aac)")?;
            self.drain(out)?;
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
        Ok(())
    }
}

/// An MP4 file written from encoded packets: a sink writer whose streams take their input
/// already compressed, so nothing is decoded or encoded again. Audio tracks get their names.
pub struct Mp4Sink {
    writer: IMFSinkWriter,
    path: PathBuf,
    /// Each stream's index in the writer and whether it's video.
    streams: Vec<(u32, bool)>,
    names: Vec<String>,
    writing: bool,
    /// How the video stream's YUV reads back, for the file's `colr`.
    matrix: Matrix,
}

impl Mp4Sink {
    pub fn create(path: &Path) -> Result<Mp4Sink, String> {
        unsafe {
            let attrs = attributes(2)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).ctx("attr")?;
            // mp4 whatever the name ends in (afterglow writes `.mp4.part` until a clip is whole)
            attrs.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4).ctx("attr")?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let writer =
                MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs).ctx("MFCreateSinkWriterFromURL")?;
            Ok(Mp4Sink { writer, path: path.to_path_buf(), streams: Vec::new(), names: Vec::new(), writing: false, matrix: Matrix::Bt709 })
        }
    }
}

impl PacketSink for Mp4Sink {
    fn add_stream(&mut self, stream: Stream) -> Result<usize, String> {
        if self.writing {
            return Err("streams can't be added once packets are written".into());
        }
        unsafe {
            let (t, video) = match &stream {
                Stream::Video { config, headers, matrix } => {
                    self.matrix = *matrix;
                    let t = video_type(config, *matrix)?;
                    if !headers.is_empty() {
                        t.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, headers).ctx("mt")?;
                    }
                    (t, true)
                }
                Stream::Audio { name, headers } => {
                    self.names.push(name.clone());
                    (aac_type(Some(headers))?, false)
                }
            };
            let index = self.writer.AddStream(&t).ctx("IMFSinkWriter::AddStream")?;
            self.writer.SetInputMediaType(index, &t, None).ctx("IMFSinkWriter::SetInputMediaType (passthrough)")?;
            self.streams.push((index, video));
        }
        Ok(self.streams.len() - 1)
    }

    fn write(&mut self, stream: usize, packet: Packet) -> Result<(), String> {
        let (index, video) = *self.streams.get(stream).ok_or("no such stream")?;
        unsafe {
            if !self.writing {
                self.writer.BeginWriting().ctx("IMFSinkWriter::BeginWriting")?;
                self.writing = true;
            }
            let buf = MFCreateMemoryBuffer(packet.data.len() as u32).ctx("MFCreateMemoryBuffer")?;
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).ctx("IMFMediaBuffer::Lock")?;
            std::ptr::copy_nonoverlapping(packet.data.as_ptr(), p, packet.data.len());
            let _ = buf.Unlock();
            buf.SetCurrentLength(packet.data.len() as u32).ctx("SetCurrentLength")?;
            let sample = MFCreateSample().ctx("MFCreateSample")?;
            sample.AddBuffer(&buf).ctx("AddBuffer")?;
            sample.SetSampleTime(packet.pts).ctx("SetSampleTime")?;
            sample.SetSampleDuration(packet.duration).ctx("SetSampleDuration")?;
            if video {
                sample.SetUINT32(&MFSampleExtension_CleanPoint, packet.key as u32).ctx("CleanPoint")?;
                if packet.dts != packet.pts {
                    sample.SetUINT64(&MFSampleExtension_DecodeTimestamp, packet.dts as u64).ctx("DecodeTimestamp")?;
                }
            }
            self.writer.WriteSample(index, &sample).ctx("IMFSinkWriter::WriteSample")
        }
    }

    fn finalize(&mut self) -> Result<(), String> {
        if !self.writing {
            return Err("nothing was written".into());
        }
        unsafe { self.writer.Finalize().ctx("IMFSinkWriter::Finalize")? };
        if self.streams.iter().any(|&(_, video)| video)
            && let Err(e) = tag_colours(&self.path, self.matrix)
        {
            log(&format!("couldn't tag the video's colours: {e}"));
        }
        if !self.names.is_empty()
            && let Err(e) = name_tracks(&self.path, &self.names)
        {
            log(&format!("couldn't name the audio tracks: {e}"));
        }
        Ok(())
    }
}
