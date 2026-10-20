use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Media::MediaFoundation::*;
use windows::core::HSTRING;

use super::math::Range;
use super::media::{self, ANY, Got, Gpu, Info, attributes, pack};
use crate::platform::log::log;
use crate::record::{Codec, Ctx};

#[derive(Clone, Copy)]
pub(super) struct Quality {
    pub qp: u32,
    pub effort: u32,
}

pub(super) struct Progress<'a> {
    pub permille: &'a AtomicU32,
    pub cancel: &'a AtomicBool,
}

impl Progress<'_> {
    pub(super) fn set(&self, done: i64, total: i64) {
        let p = if total > 0 { (done.clamp(0, total) as i128 * 1000 / total as i128) as u32 } else { 0 };
        self.permille.store(p, Ordering::Relaxed);
    }

    pub(super) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub(super) fn trim(
    src: &Path,
    info: &Info,
    range: Range,
    out: &Path,
    q: Quality,
    p: &Progress,
) -> Result<bool, String> {
    let res = match Gpu::new() {
        Ok(gpu) => match run(src, info, range, out, q, p, Some(&gpu)) {
            Err(e) if !p.cancelled() => {
                log(&format!("trim on the gpu failed, retrying in software: {e}"));
                let _ = std::fs::remove_file(out);
                run(src, info, range, out, q, p, None)
            }
            r => r,
        },
        Err(e) => {
            log(&format!("no gpu for trimming: {e}"));
            run(src, info, range, out, q, p, None)
        }
    };
    if !matches!(res, Ok(true)) {
        let _ = std::fs::remove_file(out);
    }
    res
}

struct Held {
    ts: i64,
    sample: IMFSample,
}

fn run(
    src: &Path,
    info: &Info,
    range: Range,
    out: &Path,
    q: Quality,
    p: &Progress,
    gpu: Option<&Gpu>,
) -> Result<bool, String> {
    unsafe {
        let reader = media::open(src, gpu)?;
        let streams = media::streams(&reader);
        let attrs = attributes(2)?;
        if let Some(g) = gpu {
            attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1).ctx("attr")?;
            attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &g.mgr).ctx("attr")?;
        }
        let writer = MFCreateSinkWriterFromURL(&HSTRING::from(out.as_os_str()), None, &attrs)
            .ctx("MFCreateSinkWriterFromURL")?;

        let mut targets: Vec<Option<u32>> = vec![None; streams.len()];
        let mut video = None;
        for (i, major, native) in &streams {
            let i = *i;
            reader.SetStreamSelection(i, false).ctx("SetStreamSelection")?;
            if *major == MFMediaType_Video && video.is_none() {
                reader.SetStreamSelection(i, true).ctx("SetStreamSelection")?;
                media::set_video_output(&reader, MFVideoFormat_NV12, info.size)?;
                targets[i as usize] = Some(video_stream(&writer, info, q)?);
                video = Some(i);
            } else if *major == MFMediaType_Audio {
                reader.SetStreamSelection(i, true).ctx("SetStreamSelection")?;
                reader.SetCurrentMediaType(i, None, native).ctx("audio type")?;
                let w = writer.AddStream(native).ctx("can't copy the audio")?;
                writer.SetInputMediaType(w, native, None).ctx("can't copy the audio")?;
                targets[i as usize] = Some(w);
            }
        }
        let Some(video) = video else { return Err("the file has no video".into()) };
        writer.BeginWriting().ctx("IMFSinkWriter::BeginWriting")?;
        media::seek(&reader, range.start)?;

        let mut active: Vec<u32> = (0..streams.len() as u32).filter(|&i| targets[i as usize].is_some()).collect();
        let mut held: Option<Held> = None;
        let mut started = false;
        let write = |stream: u32, sample: &IMFSample, ts: i64, dur: Option<i64>| -> Result<(), String> {
            sample.SetSampleTime(ts).ctx("SetSampleTime")?;
            if let Some(d) = dur {
                sample.SetSampleDuration(d).ctx("SetSampleDuration")?;
            }
            writer.WriteSample(stream, sample).ctx("WriteSample")
        };
        let finish = |i: u32, active: &mut Vec<u32>| {
            active.retain(|&a| a != i);
            let _ = reader.SetStreamSelection(i, false);
        };
        while !active.is_empty() {
            if p.cancelled() {
                return Ok(false);
            }
            let (i, ts, sample) = match media::read(&reader, ANY)? {
                Got::Sample { stream, ts, sample } => (stream, ts, sample),
                Got::End(i) => {
                    if i == video
                        && let Some(h) = held.take()
                    {
                        let d = media::duration(&h.sample);
                        let stop = if d > 0 { (h.ts + d).min(range.end) } else { range.end };
                        write(targets[i as usize].unwrap(), &h.sample, 0, Some((stop - range.start).max(1)))?;
                        started = true;
                    }
                    finish(i, &mut active);
                    continue;
                }
            };
            let Some(w) = targets.get(i as usize).copied().flatten() else { continue };
            if i != video {
                if ts >= range.end {
                    finish(i, &mut active);
                } else if ts >= range.start {
                    write(w, &sample, ts - range.start, None)?;
                }
                continue;
            }
            if !started {
                if ts <= range.start {
                    held = Some(Held { ts, sample });
                    continue;
                }
                started = true;
                if let Some(h) = held.take() {
                    write(w, &h.sample, 0, Some(ts.min(range.end) - range.start))?;
                }
            }
            let dur = media::duration(&sample);
            if ts >= range.end || range.end - ts < dur / 2 {
                finish(i, &mut active);
                continue;
            }
            write(w, &sample, ts - range.start, (dur > 0).then(|| dur.min(range.end - ts)))?;
            p.set(ts - range.start, range.len());
        }
        if !started && held.is_none() {
            return Err("the selection has no frames".into());
        }
        writer.Finalize().ctx("IMFSinkWriter::Finalize")?;
        p.set(1, 1);
        Ok(true)
    }
}

unsafe fn video_stream(writer: &IMFSinkWriter, info: &Info, q: Quality) -> Result<u32, String> {
    unsafe {
        let (w, h) = info.size;
        let (subtype, profile) = match info.codec.unwrap_or(Codec::H264) {
            Codec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
            Codec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
        };
        let nominal_bps = ((w as f64 * h as f64 * info.fps) / 10.0).clamp(2e6, 400e6) as u32;
        let fps_milli = (info.fps * 1000.0).round() as u32;
        let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        out_t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AVG_BITRATE, nominal_bps).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        out_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
        out_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
        out_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_MPEG2_PROFILE, profile).ctx("mt")?;
        let stream = writer.AddStream(&out_t).ctx("IMFSinkWriter::AddStream")?;

        let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        in_t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps_milli, 1000)).ctx("mt")?;
        in_t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).ctx("mt")?;

        let enc = attributes(4)?;
        enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_Quality.0 as u32).ctx("enc")?;
        enc.SetUINT64(&CODECAPI_AVEncVideoEncodeQP, q.qp.min(51) as u64).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, q.effort.min(100)).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncMPVGOPSize, (info.fps * 2.0).round() as u32).ctx("enc")?;
        writer.SetInputMediaType(stream, &in_t, &enc).ctx("IMFSinkWriter::SetInputMediaType")?;
        Ok(stream)
    }
}
