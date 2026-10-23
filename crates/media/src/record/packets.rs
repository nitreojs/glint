//! The packet path on Windows: the hardware encoder MFT driven directly ([`HwVideoEncoder`]),
//! Media Foundation's AAC encoder ([`AacEncoder`]) and an MP4 writer that takes their packets
//! as they are ([`Mp4Sink`], a sink writer in passthrough). Joined by
//! [`glint_core::record::pipeline::Encoded`], they replace the sink writer's encode-and-mux in
//! one step, so encoded packets can go elsewhere first (afterglow's replay buffer).

use std::mem::ManuallyDrop;
use std::path::{Path, PathBuf};

use glint_core::log::log;
use glint_core::record::Codec;
use glint_core::record::mix::{RATE, frames_to_hns};
use glint_core::record::mp4::name_tracks;
use glint_core::record::pipeline::{AudioEncoder, Packet, PacketSink, Stream, VideoConfig, VideoEncoder};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{HSTRING, Interface, PWSTR};

use super::Ctx;
use super::encoder::{AAC_BYTES_PER_SECOND, attributes, gop, input_type, pack};

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
unsafe fn video_type(config: &VideoConfig) -> Result<IMFMediaType, String> {
    let (w, h) = (config.width, config.height);
    let (subtype, profile) = video_subtype(config.codec);
    // Only a header hint in constant-QP mode (~0.1 bit per pixel).
    let nominal_bps = ((w as f64 * h as f64 * config.fps) / 10.0).clamp(2e6, 400e6) as u32;
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
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32).ctx("mt")?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32).ctx("mt")?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32).ctx("mt")?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32).ctx("mt")?;
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

fn friendly_name(a: &IMFActivate) -> String {
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0;
        if a.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut p, &mut len).is_err() {
            return "video encoder".into();
        }
        let name = p.to_string().unwrap_or_default();
        CoTaskMemFree(Some(p.0 as _));
        name
    }
}

/// The GPU's video encoder MFT, asynchronous, fed D3D11 textures from the device behind `mgr`.
/// Constant QP like the sink writer path, no B-frames (packets come out in display order, so
/// decode time equals presentation time), keyframes as the config says.
pub struct HwVideoEncoder {
    mgr: IMFDXGIDeviceManager,
    mft: Option<(IMFActivate, IMFTransform, IMFMediaEventGenerator)>,
    /// Inputs the encoder has asked for and not been given yet.
    wanted: u32,
    name: String,
}

impl HwVideoEncoder {
    pub fn new(mgr: &IMFDXGIDeviceManager) -> HwVideoEncoder {
        HwVideoEncoder { mgr: mgr.clone(), mft: None, wanted: 0, name: String::new() }
    }

    /// The encoder MFT's friendly name, once configured.
    pub fn name(&self) -> &str {
        &self.name
    }

    fn parts(&self) -> Result<&(IMFActivate, IMFTransform, IMFMediaEventGenerator), String> {
        self.mft.as_ref().ok_or_else(|| "the video encoder isn't configured".to_string())
    }

    /// Sets up the first hardware encoder for `config.codec` that takes our device and types.
    unsafe fn open(&mut self, config: &VideoConfig) -> Result<Vec<u8>, String> {
        let (subtype, _) = video_subtype(config.codec);
        let out = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype };
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut n = 0u32;
        unsafe {
            let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
            MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, None, Some(&out), &mut list, &mut n).ctx("MFTEnumEx")?;
            let found: Vec<IMFActivate> = (0..n as usize).filter_map(|i| (*list.add(i)).take()).collect();
            CoTaskMemFree(Some(list as _));
            let mut last = format!("no hardware {:?} encoder", config.codec);
            for a in found {
                let name = friendly_name(&a);
                match self.try_open(&a, config) {
                    Ok((mft, events, headers)) => {
                        log(&format!("{name}: configured for the packet path"));
                        self.name = name;
                        self.mft = Some((a, mft, events));
                        return Ok(headers);
                    }
                    Err(e) => {
                        let _ = a.ShutdownObject();
                        last = format!("{name}: {e}");
                    }
                }
            }
            Err(last)
        }
    }

    unsafe fn try_open(
        &self,
        a: &IMFActivate,
        config: &VideoConfig,
    ) -> Result<(IMFTransform, IMFMediaEventGenerator, Vec<u8>), String> {
        unsafe {
            let mft: IMFTransform = a.ActivateObject().ctx("ActivateObject")?;
            let attrs = mft.GetAttributes().ctx("GetAttributes")?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).ctx("MF_TRANSFORM_ASYNC_UNLOCK")?;
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, self.mgr.as_raw() as usize)
                .ctx("MFT_MESSAGE_SET_D3D_MANAGER")?;
            let api: ICodecAPI = mft.cast().ctx("ICodecAPI")?;
            let set = |key: &windows::core::GUID, v: VARIANT, what: &str| api.SetValue(key, &v).ctx(what);
            let quality = eAVEncCommonRateControlMode_Quality.0 as u32;
            set(&CODECAPI_AVEncCommonRateControlMode, quality.into(), "rate control")?;
            set(&CODECAPI_AVEncVideoEncodeQP, (config.qp.min(51) as u64).into(), "qp")?;
            // the rest are hints some encoders don't take
            let _ = set(&CODECAPI_AVEncCommonQualityVsSpeed, config.effort.min(100).into(), "effort");
            let _ = set(&CODECAPI_AVEncMPVGOPSize, gop(config).into(), "gop");
            let _ = set(&CODECAPI_AVEncMPVDefaultBPictureCount, 0u32.into(), "b-frames");
            mft.SetOutputType(0, &video_type(config)?, 0).ctx("SetOutputType")?;
            mft.SetInputType(0, &input_type(config.width, config.height, config.fps)?, 0).ctx("SetInputType")?;
            let headers = blob(&mft.GetOutputCurrentType(0).ctx("GetOutputCurrentType")?, &MF_MT_MPEG_SEQUENCE_HEADER);
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).ctx("NOTIFY_BEGIN_STREAMING")?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).ctx("NOTIFY_START_OF_STREAM")?;
            let events: IMFMediaEventGenerator = mft.cast().ctx("IMFMediaEventGenerator")?;
            Ok((mft, events, headers))
        }
    }

    /// Handles one event: an input request is counted, an output becomes a packet. Returns
    /// false once draining has finished. With `wait` false, returns `Ok(None)` when no event is queued.
    unsafe fn event(&mut self, wait: bool, out: &mut Vec<Packet>) -> Result<Option<bool>, String> {
        let (mft, events) = self.parts().map(|(_, m, e)| (m.clone(), e.clone()))?;
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
}

impl VideoEncoder for HwVideoEncoder {
    type Frame = IMFSample;

    fn configure(&mut self, config: &VideoConfig) -> Result<Stream, String> {
        let headers = unsafe { self.open(config)? };
        Ok(Stream::Video { config: *config, headers })
    }

    fn encode(&mut self, frame: IMFSample, time: i64, duration: i64, out: &mut Vec<Packet>) -> Result<(), String> {
        unsafe {
            frame.SetSampleTime(time).ctx("SetSampleTime")?;
            frame.SetSampleDuration(duration).ctx("SetSampleDuration")?;
            while self.wanted == 0 {
                self.event(true, out)?;
            }
            self.wanted -= 1;
            self.parts()?.1.ProcessInput(0, &frame, 0).ctx("IMFTransform::ProcessInput")?;
            // whatever is ready now; the rest comes with the next frame
            while self.event(false, out)?.is_some() {}
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), String> {
        unsafe {
            let mft = self.parts()?.1.clone();
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0).ctx("NOTIFY_END_OF_STREAM")?;
            mft.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0).ctx("COMMAND_DRAIN")?;
            while self.event(true, out)? != Some(false) {}
            let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
        }
        Ok(())
    }
}

impl Drop for HwVideoEncoder {
    fn drop(&mut self) {
        if let Some((a, mft, _)) = self.mft.take() {
            unsafe {
                if let Ok(s) = mft.cast::<IMFShutdown>() {
                    let _ = s.Shutdown();
                }
                let _ = a.ShutdownObject();
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
}

impl Mp4Sink {
    pub fn create(path: &Path) -> Result<Mp4Sink, String> {
        unsafe {
            let attrs = attributes(1)?;
            attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1).ctx("attr")?;
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let writer =
                MFCreateSinkWriterFromURL(&HSTRING::from(path.as_os_str()), None, &attrs).ctx("MFCreateSinkWriterFromURL")?;
            Ok(Mp4Sink { writer, path: path.to_path_buf(), streams: Vec::new(), names: Vec::new(), writing: false })
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
                Stream::Video { config, headers } => {
                    let t = video_type(config)?;
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
        if !self.names.is_empty()
            && let Err(e) = name_tracks(&self.path, &self.names)
        {
            log(&format!("couldn't name the audio tracks: {e}"));
        }
        Ok(())
    }
}
