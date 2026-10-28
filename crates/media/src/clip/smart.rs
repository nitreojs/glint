//! Smart cut: a copied video that starts on any frame. The frames from the cut up to the next
//! keyframe are encoded again the way the recording was (codec, profile, size, colour tags) and the
//! rest is copied packet for packet; when the new start's parameter sets differ from the
//! recording's, each carries its own in-band, the recording's coming back on the keyframe after.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use glint_core::clip::export::KEEP_QP;
use glint_core::clip::info::{Info, Snap};
use glint_core::clip::math::SECOND;
use glint_core::clip::nal;
use glint_core::log::log;
use glint_core::record::Codec;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::{GUID, HSTRING};

use super::media::{self, Got, Gpu, attributes, pack};
use super::trim::Progress;
use crate::record::Ctx;

/// The bitrate the new start's type names (encoders want one); it's encoded at a steady quality.
const NOMINAL_BPS: f64 = 2_000_000.0;
const COLOUR: [GUID; 4] = [MF_MT_VIDEO_PRIMARIES, MF_MT_TRANSFER_FUNCTION, MF_MT_YUV_MATRIX, MF_MT_VIDEO_NOMINAL_RANGE];

pub(super) fn codec(native: &IMFMediaType) -> Option<Codec> {
    match unsafe { native.GetGUID(&MF_MT_SUBTYPE) } {
        Ok(g) if g == MFVideoFormat_H264 => Some(Codec::H264),
        Ok(g) if g == MFVideoFormat_HEVC => Some(Codec::Hevc),
        _ => None,
    }
}

pub(super) fn blob(t: &IMFMediaType, key: &GUID) -> Vec<u8> {
    unsafe {
        let Ok(size) = t.GetBlobSize(key) else { return Vec::new() };
        let mut buf = vec![0u8; size as usize];
        match t.GetBlob(key, &mut buf, None) {
            Ok(()) => buf,
            Err(_) => Vec::new(),
        }
    }
}

/// Why a copy of the video `native` describes (stream `video` of `reader`) can't start on any frame;
/// none when it can.
pub(super) fn snap(reader: &IMFSourceReader, video: u32, native: &IMFMediaType) -> Option<Snap> {
    let Ok(rec) = Recorded::of(reader, video, native) else { return Some(Snap::Setup) };
    let subtype = if rec.codec == Codec::H264 { MFVideoFormat_H264 } else { MFVideoFormat_HEVC };
    (!media::has_decoder(subtype) || !media::has_encoder(subtype)).then_some(Snap::Codec)
}

/// The keyframe a smart-cut piece copies from: the first after `from` (`to` when none comes before
/// it), and the recording's bitrate over the stretch it replaces, in bits per second.
pub(super) fn next_key(reader: &IMFSourceReader, video: u32, from: i64, to: i64) -> Result<(i64, f64), String> {
    unsafe {
        reader.SetStreamSelection(media::ALL, false).ctx("SetStreamSelection")?;
        reader.SetStreamSelection(video, true).ctx("SetStreamSelection")?;
    }
    media::seek(reader, from)?;
    let (mut first, mut bytes) = (None, 0u64);
    loop {
        let (ts, sample) = match media::read(reader, video)? {
            Got::Sample { ts, sample, .. } => (ts, sample),
            Got::End(_) => break,
        };
        let key = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
        if ts >= to || ts > from && key {
            let at = ts.min(to);
            let secs = (at - first.unwrap_or(from)).max(1) as f64 / SECOND as f64;
            return Ok((at, bytes as f64 * 8.0 / secs));
        }
        first.get_or_insert(ts);
        bytes += unsafe { sample.GetTotalLength() }.unwrap_or(0) as u64;
    }
    let secs = (to - first.unwrap_or(from)).max(1) as f64 / SECOND as f64;
    Ok((to, bytes as f64 * 8.0 / secs))
}

/// A sample of the new start: when it's shown (in the recording's time) and for how long.
pub(super) struct Frame {
    pub ts: i64,
    pub dur: i64,
    pub sample: IMFSample,
}

/// The parameter sets of a stream, each a NAL unit.
pub(super) type Sets = Vec<Vec<u8>>;

/// The recording's video as a smart cut matches it.
pub(super) struct Recorded {
    pub native: IMFMediaType,
    pub codec: Codec,
    pub sets: Sets,
    /// What its sequence parameter set asks of an encoder that would continue it.
    pub setup: nal::Setup,
}

impl Recorded {
    /// Stream `video` of `reader`, `native`, when it's h.264 or hevc an encoder here can match.
    pub(super) fn of(reader: &IMFSourceReader, video: u32, native: &IMFMediaType) -> Result<Recorded, String> {
        let (codec, sets) = sets_of(reader, video, native)?;
        let setup = nal::setup(codec, &annex_b(&sets)).ok_or("a codec setup no encoder here matches")?;
        Ok(Recorded { native: native.clone(), codec, sets, setup })
    }
}

/// The type a sink writer copies stream `video` of `reader`, `native`, with: that one, or, when media
/// foundation gave no sequence header (hevc), a copy with the parameter sets from its first sample as
/// one and without the recording's sample description. Given that description instead, the mp4 sink
/// writes it where the whole `stsd` box belongs, and no player finds the video.
pub(super) fn copy_type(reader: &IMFSourceReader, video: u32, native: &IMFMediaType) -> Result<IMFMediaType, String> {
    if !blob(native, &MF_MT_MPEG_SEQUENCE_HEADER).is_empty() || codec(native).is_none() {
        return Ok(native.clone());
    }
    let Ok((_, sets)) = sets_of(reader, video, native) else { return Ok(native.clone()) };
    unsafe {
        let t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        native.CopyAllItems(&t).ctx("CopyAllItems")?;
        let _ = t.DeleteItem(&MF_MT_MPEG4_SAMPLE_DESCRIPTION);
        t.SetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &annex_b(&sets)).ctx("mt")?;
        Ok(t)
    }
}

fn annex_b(sets: &Sets) -> Vec<u8> {
    sets.iter().flat_map(|s| [0, 0, 0, 1].into_iter().chain(s.iter().copied())).collect()
}

/// The parameter sets of stream `video` of `reader`, `native`: from its sequence header, or (media
/// foundation gives none for hevc) the ones in-band in the next sample `reader` reads.
fn sets_of(reader: &IMFSourceReader, video: u32, native: &IMFMediaType) -> Result<(Codec, Sets), String> {
    let codec = codec(native).ok_or("not h.264 or hevc")?;
    let header = blob(native, &MF_MT_MPEG_SEQUENCE_HEADER);
    let mut sets: Sets = nal::parameter_sets(codec, &header).into_iter().map(<[u8]>::to_vec).collect();
    if sets.is_empty() {
        unsafe { reader.SetStreamSelection(video, true).ctx("SetStreamSelection")? };
        if let Got::Sample { sample, .. } = media::read(reader, video)? {
            sets = nal::parameter_sets(codec, &bytes(&sample)?).into_iter().map(<[u8]>::to_vec).collect();
        }
    }
    if sets.is_empty() {
        return Err("the recording doesn't say its parameter sets".into());
    }
    Ok((codec, sets))
}

/// `sample` (Annex B) with `sets` in-band, as a new keyframe sample at the same time. They're hidden
/// from media foundation's mp4 sink, which takes parameter sets out of the samples; once the file is
/// written, [`glint_core::clip::index::reveal_sets`] brings them back.
pub(super) fn with_sets(codec: Codec, sample: &IMFSample, sets: &[Vec<u8>]) -> Result<IMFSample, String> {
    let data = bytes(sample)?;
    let hidden: Vec<Vec<u8>> = sets.iter().map(|s| nal::hide(codec, s)).collect();
    let hidden: Vec<&[u8]> = hidden.iter().map(Vec::as_slice).collect();
    let out = sample_of(&nal::with_sets(codec, &data, &hidden))?;
    unsafe {
        out.SetUINT32(&MFSampleExtension_CleanPoint, 1).ctx("CleanPoint")?;
        if let Ok(t) = sample.GetSampleTime() {
            out.SetSampleTime(t).ctx("SetSampleTime")?;
        }
        if let Ok(d) = sample.GetSampleDuration() {
            out.SetSampleDuration(d).ctx("SetSampleDuration")?;
        }
        if let Ok(dts) = sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) {
            out.SetUINT64(&MFSampleExtension_DecodeTimestamp, dts).ctx("dts")?;
        }
    }
    Ok(out)
}

/// Whether `sample` (Annex B) is an IDR picture, which the copy can follow a new start with.
pub(super) fn starts_anew(codec: Codec, sample: &IMFSample) -> Result<bool, String> {
    Ok(nal::starts_anew(codec, &bytes(sample)?))
}

fn bytes(sample: &IMFSample) -> Result<Vec<u8>, String> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().ctx("ConvertToContiguousBuffer")?;
        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut p, None, Some(&mut len)).ctx("Lock")?;
        let data = std::slice::from_raw_parts(p, len as usize).to_vec();
        let _ = buf.Unlock();
        Ok(data)
    }
}

fn sample_of(data: &[u8]) -> Result<IMFSample, String> {
    unsafe {
        let buf = MFCreateMemoryBuffer(data.len() as u32).ctx("MFCreateMemoryBuffer")?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None).ctx("Lock")?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = buf.Unlock();
        buf.SetCurrentLength(data.len() as u32).ctx("SetCurrentLength")?;
        let sample = MFCreateSample().ctx("MFCreateSample")?;
        sample.AddBuffer(&buf).ctx("AddBuffer")?;
        Ok(sample)
    }
}

/// A temp file removed when dropped.
struct Temp(PathBuf);

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn temp() -> Temp {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    Temp(std::env::temp_dir().join(format!("glint-cut-{}-{n}.mp4", std::process::id())))
}

/// The recording's frames shown from `from` up to `to` encoded again as the start of a copy of its
/// video, `rec`: in order, the first a keyframe at `from` (`bps`, the recording's own rate there,
/// only names a rate); on `gpu` when it can, in software otherwise. With the parameter sets they need.
pub(super) fn head(
    src: &Path,
    rec: &Recorded,
    info: &Info,
    (from, to): (i64, i64),
    bps: f64,
    gpu: Option<&Gpu>,
    p: &Progress,
) -> Result<(Vec<Frame>, Sets), String> {
    let run = |gpu| encode(src, rec, info, (from, to), bps, gpu, p);
    match gpu {
        Some(g) => run(Some(g)).or_else(|e| {
            if p.cancelled() {
                return Err(e);
            }
            log(&format!("clip: encoding the cut on the gpu failed, trying in software: {e}"));
            run(None).map_err(|soft| format!("{e}; in software: {soft}"))
        }),
        None => run(None),
    }
}

fn encode(
    src: &Path,
    rec: &Recorded,
    info: &Info,
    (from, to): (i64, i64),
    bps: f64,
    gpu: Option<&Gpu>,
    p: &Progress,
) -> Result<(Vec<Frame>, Sets), String> {
    let codec = rec.codec;
    let file = temp();
    let mut times = Vec::new();
    unsafe {
        let reader = media::open(src, gpu)?;
        media::video_only(&reader, MFVideoFormat_NV12, info.size)?;
        let attrs = attributes(2)?;
        if let Some(g) = gpu {
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).ctx("attr")?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &g.mgr).ctx("attr")?;
        }
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(file.0.as_os_str()), None, &attrs).ctx("MFCreateSinkWriterFromURL")?;
        let bitrate = bps.clamp(NOMINAL_BPS, 400e6) as u32;
        let frames = ((to - from) as f64 / SECOND as f64 * info.fps).ceil() as u32 + 2;
        let stream = stream(&writer, rec, info, bitrate, frames)?;
        writer.BeginWriting().ctx("IMFSinkWriter::BeginWriting")?;

        media::seek(&reader, from)?;
        let mut held: Option<(i64, IMFSample)> = None;
        let mut put = |ts: i64, dur: i64, s: &IMFSample| -> Result<(), String> {
            s.SetSampleTime(ts - from).ctx("SetSampleTime")?;
            s.SetSampleDuration(dur).ctx("SetSampleDuration")?;
            writer.WriteSample(stream, s).ctx("WriteSample")?;
            times.push((ts, dur));
            Ok(())
        };
        loop {
            if p.cancelled() {
                return Err("cancelled".into());
            }
            let (ts, sample) = match media::read(&reader, media::VIDEO)? {
                Got::Sample { ts, sample, .. } => (ts, sample),
                Got::End(_) => break,
            };
            if ts >= to {
                break;
            }
            if ts <= from {
                held = Some((from, sample));
                continue;
            }
            if let Some((t, s)) = held.take() {
                put(t, ts - t, &s)?;
            }
            held = Some((ts, sample));
        }
        if let Some((t, s)) = held {
            put(t, to - t, &s)?;
        }
        writer.Finalize().ctx("IMFSinkWriter::Finalize")?;
    }
    if times.is_empty() {
        return Err("no frames to encode at the cut".into());
    }
    let (encoded, new_sets) = read_back(&file.0, codec)?;
    if encoded.len() != times.len() {
        return Err(format!("{} frames went into the encoder and {} came out", times.len(), encoded.len()));
    }
    let mut out = Vec::with_capacity(encoded.len());
    for (k, (s, (ts, dur))) in encoded.into_iter().zip(times).enumerate() {
        unsafe {
            let _ = s.DeleteItem(&MFSampleExtension_DecodeTimestamp);
            if k == 0 {
                s.SetUINT32(&MFSampleExtension_CleanPoint, 1).ctx("CleanPoint")?;
            }
        }
        out.push(Frame { ts, dur, sample: s });
    }
    Ok((out, new_sets))
}

/// The samples of the video in `file` as they're stored, and its parameter sets.
fn read_back(file: &Path, codec: Codec) -> Result<(Vec<IMFSample>, Sets), String> {
    unsafe {
        let reader = MFCreateSourceReaderFromURL(&HSTRING::from(file.as_os_str()), None).ctx("reading the encoded cut")?;
        reader.SetStreamSelection(media::ALL, false).ctx("SetStreamSelection")?;
        reader.SetStreamSelection(media::VIDEO, true).ctx("SetStreamSelection")?;
        let native = reader.GetNativeMediaType(media::VIDEO, 0).ctx("the encoded cut's type")?;
        if self::codec(&native) != Some(codec) {
            return Err("the encoder wrote another codec".into());
        }
        let mut out = Vec::new();
        while let Got::Sample { sample, .. } = media::read(&reader, media::VIDEO)? {
            out.push(sample);
        }
        let first = bytes(out.first().ok_or("the encoder wrote nothing")?)?;
        if !first.starts_with(&[0, 0]) {
            return Err("the encoded cut isn't an annex b stream".into());
        }
        let header = blob(&native, &MF_MT_MPEG_SEQUENCE_HEADER);
        let from = if header.is_empty() { &first } else { &header };
        let sets: Sets = nal::parameter_sets(codec, from).into_iter().map(<[u8]>::to_vec).collect();
        if sets.is_empty() {
            return Err("the encoded cut has no parameter sets".into());
        }
        Ok((out, sets))
    }
}

/// An encoder stream for frames like the recording's: its codec and profile, size and colour tags,
/// at the quality of [`KEEP_QP`] (`bitrate` only names a rate), with one keyframe (`frames` long
/// groups).
unsafe fn stream(writer: &IMFSinkWriter, rec: &Recorded, info: &Info, bitrate: u32, frames: u32) -> Result<u32, String> {
    unsafe {
        let ((w, h), fps_milli) = (info.size, (info.fps * 1000.0).round() as u32);
        let subtype = if rec.codec == Codec::H264 { MFVideoFormat_H264 } else { MFVideoFormat_HEVC };
        let tags = |t: &IMFMediaType| -> Result<(), String> {
            for key in &COLOUR {
                if let Ok(v) = rec.native.GetUINT32(key) {
                    t.SetUINT32(key, v).ctx("mt")?;
                }
            }
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
            t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")
        };
        let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        out_t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, rec.setup.profile).ctx("mt")?;
        tags(&out_t)?;
        let stream = writer.AddStream(&out_t).ctx("IMFSinkWriter::AddStream")?;

        let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        in_t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).ctx("mt")?;
        tags(&in_t)?;
        let enc = attributes(6)?;
        let mode = eAVEncCommonRateControlMode_Quality.0 as u32;
        enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, mode).ctx("enc")?;
        enc.SetUINT64(&CODECAPI_AVEncVideoEncodeQP, KEEP_QP as u64).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, 100).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncMPVGOPSize, frames).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0).ctx("enc")?;
        writer.SetInputMediaType(stream, &in_t, &enc).ctx("IMFSinkWriter::SetInputMediaType")?;
        Ok(stream)
    }
}
