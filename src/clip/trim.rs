use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};
use windows::Win32::Media::MediaFoundation::*;
use windows::core::HSTRING;

use super::math::Range;
use super::media::{self, ANY, Got, Gpu, Info, Track, attributes, pack};
use super::pcm::{self, Lane};
use crate::platform::log::log;
use crate::record::{Codec, Ctx};

const AAC_BYTES_PER_SECOND: u32 = 24_000;

#[derive(Clone, Copy)]
pub(super) struct Quality {
    pub qp: u32,
    pub effort: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum AudioExport {
    #[default]
    All,
    Unmuted,
    Mix,
}

#[derive(Clone, Debug)]
pub(super) struct Plan {
    pub kept: Vec<Range>,
    pub unmuted: Vec<bool>,
    pub audio: AudioExport,
}

impl Plan {
    fn len(&self) -> i64 {
        self.kept.iter().map(|r| r.len()).sum()
    }

    fn keeps(&self, track: usize) -> bool {
        self.audio == AudioExport::All || self.unmuted.get(track).copied().unwrap_or(true)
    }
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

pub(super) fn trim(src: &Path, info: &Info, plan: &Plan, out: &Path, q: Quality, p: &Progress) -> Result<bool, String> {
    let res = match Gpu::new() {
        Ok(gpu) => match run(src, info, plan, out, q, p, Some(&gpu)) {
            Err(e) if !p.cancelled() => {
                log(&format!("trim on the gpu failed, retrying in software: {e}"));
                let _ = std::fs::remove_file(out);
                run(src, info, plan, out, q, p, None)
            }
            r => r,
        },
        Err(e) => {
            log(&format!("no gpu for trimming: {e}"));
            run(src, info, plan, out, q, p, None)
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

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Video(u32),
    Copy(u32),
    Mix(usize),
}

struct Mixdown {
    stream: u32,
    rate: u32,
    lanes: Vec<(usize, Lane)>,
    written: i64,
    scratch: Vec<f32>,
    stereo: Vec<f32>,
}

impl Mixdown {
    unsafe fn new(writer: &IMFSinkWriter, rate: u32) -> Result<Mixdown, String> {
        if rate != 44_100 && rate != 48_000 {
            return Err(format!("audio at {rate} Hz can't be mixed into one track"));
        }
        unsafe {
            let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
            out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
            out_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND).ctx("mt")?;
            out_t.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29).ctx("mt")?;
            let stream = writer.AddStream(&out_t).ctx("IMFSinkWriter::AddStream (audio)")?;
            let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
            in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
            in_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4).ctx("mt")?;
            in_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * 4).ctx("mt")?;
            writer.SetInputMediaType(stream, &in_t, None).ctx("IMFSinkWriter::SetInputMediaType (audio)")?;
            Ok(Mixdown { stream, rate, lanes: Vec::new(), written: 0, scratch: Vec::new(), stereo: Vec::new() })
        }
    }

    fn push(&mut self, lane: usize, sample: &IMFSample, at: i64, seg: Range, base: i64) -> Result<(), String> {
        media::floats(sample, &mut self.scratch)?;
        let channels = self.lanes[lane].0;
        pcm::to_stereo(&self.scratch, channels, &mut self.stereo);
        let n = self.stereo.len() as i64 / 2;
        let first = pcm::frames(base, self.rate);
        let f0 = pcm::frames(at - seg.start, self.rate);
        let (lo, hi) = ((-f0).max(0), n.min(pcm::frames(base + seg.len(), self.rate) - first - f0));
        if lo < hi {
            self.lanes[lane].1.push(first + f0 + lo, &self.stereo[lo as usize * 2..hi as usize * 2]);
        }
        Ok(())
    }

    unsafe fn flush(&mut self, writer: &IMFSinkWriter, upto: i64) -> Result<(), String> {
        let n = upto - self.written;
        if n <= 0 {
            return Ok(());
        }
        let mut out = vec![0.0; n as usize * 2];
        for (_, lane) in &mut self.lanes {
            lane.take_into(&mut out, 1.0);
        }
        let bytes = pcm::to_i16(&out);
        unsafe {
            let buf = MFCreateMemoryBuffer(bytes.len() as u32).ctx("MFCreateMemoryBuffer")?;
            let mut ptr = std::ptr::null_mut();
            buf.Lock(&mut ptr, None, None).ctx("IMFMediaBuffer::Lock")?;
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            let _ = buf.Unlock();
            buf.SetCurrentLength(bytes.len() as u32).ctx("SetCurrentLength")?;
            let sample = MFCreateSample().ctx("MFCreateSample")?;
            sample.AddBuffer(&buf).ctx("AddBuffer")?;
            let (t0, t1) = (pcm::hns(self.written, self.rate), pcm::hns(upto, self.rate));
            sample.SetSampleTime(t0).ctx("SetSampleTime")?;
            sample.SetSampleDuration(t1 - t0).ctx("SetSampleDuration")?;
            writer.WriteSample(self.stream, &sample).ctx("WriteSample (audio)")?;
        }
        self.written = upto;
        Ok(())
    }
}

fn run(
    src: &Path,
    info: &Info,
    plan: &Plan,
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

        let kept: Vec<&Track> = info.tracks.iter().enumerate().filter(|(k, _)| plan.keeps(*k)).map(|(_, t)| t).collect();
        let mut targets: Vec<Option<Target>> = vec![None; streams.len()];
        for (i, ..) in &streams {
            reader.SetStreamSelection(*i, false).ctx("SetStreamSelection")?;
        }
        let Some(&(video, ..)) = streams.iter().find(|s| s.1 == MFMediaType_Video) else {
            return Err("the file has no video".into());
        };
        reader.SetStreamSelection(video, true).ctx("SetStreamSelection")?;
        media::set_video_output(&reader, MFVideoFormat_NV12, info.size)?;
        targets[video as usize] = Some(Target::Video(video_stream(&writer, info, q)?));
        let mut mix = if plan.audio == AudioExport::Mix && kept.len() > 1 {
            Some(Mixdown::new(&writer, media::audio_rate(&reader, kept[0].stream))?)
        } else {
            None
        };
        for t in &kept {
            let (i, native) = (t.stream, &streams[t.stream as usize].2);
            reader.SetStreamSelection(i, true).ctx("SetStreamSelection")?;
            targets[i as usize] = Some(match &mut mix {
                Some(m) => {
                    let channels = media::float_output(&reader, i, m.rate)?;
                    m.lanes.push((channels, Lane::new(0)));
                    Target::Mix(m.lanes.len() - 1)
                }
                None => {
                    reader.SetCurrentMediaType(i, None, native).ctx("audio type")?;
                    let w = writer.AddStream(native).ctx("can't copy the audio")?;
                    writer.SetInputMediaType(w, native, None).ctx("can't copy the audio")?;
                    Target::Copy(w)
                }
            });
        }
        writer.BeginWriting().ctx("IMFSinkWriter::BeginWriting")?;

        let write = |stream: u32, sample: &IMFSample, ts: i64, dur: Option<i64>| -> Result<(), String> {
            sample.SetSampleTime(ts).ctx("SetSampleTime")?;
            if let Some(d) = dur {
                sample.SetSampleDuration(d).ctx("SetSampleDuration")?;
            }
            writer.WriteSample(stream, sample).ctx("WriteSample")
        };
        let total = plan.len();
        let mut copied_to = vec![0i64; streams.len()];
        let mut any_video = false;
        let mut base = 0;
        for &seg in &plan.kept {
            let mut active: Vec<u32> = (0..streams.len() as u32).filter(|&i| targets[i as usize].is_some()).collect();
            for &i in &active {
                reader.SetStreamSelection(i, true).ctx("SetStreamSelection")?;
            }
            media::seek(&reader, seg.start)?;
            let mut open: Vec<bool> = mix.as_ref().map_or(Vec::new(), |m| vec![true; m.lanes.len()]);
            let mut held: Option<Held> = None;
            let mut started = false;
            let out_ts = |ts: i64| ts - seg.start + base;
            while !active.is_empty() {
                if p.cancelled() {
                    return Ok(false);
                }
                let (i, ts, sample) = match media::read(&reader, ANY)? {
                    Got::Sample { stream, ts, sample } => (stream, ts, sample),
                    Got::End(i) => {
                        match targets.get(i as usize).copied().flatten() {
                            Some(Target::Video(w)) => {
                                if let Some(h) = held.take() {
                                    let d = media::duration(&h.sample);
                                    let stop = if d > 0 { (h.ts + d).min(seg.end) } else { seg.end };
                                    write(w, &h.sample, base, Some((stop - seg.start).max(1)))?;
                                    started = true;
                                }
                            }
                            Some(Target::Mix(l)) => open[l] = false,
                            _ => {}
                        }
                        active.retain(|&a| a != i);
                        let _ = reader.SetStreamSelection(i, false);
                        continue;
                    }
                };
                let Some(target) = targets.get(i as usize).copied().flatten() else { continue };
                let mut done = ts >= seg.end;
                match target {
                    Target::Copy(w) if !done && ts >= seg.start => {
                        let at = out_ts(ts).max(copied_to[i as usize]);
                        write(w, &sample, at, None)?;
                        copied_to[i as usize] = at + media::duration(&sample);
                    }
                    Target::Copy(_) => {}
                    Target::Mix(l) => {
                        let m = mix.as_mut().unwrap();
                        if done {
                            open[l] = false;
                        } else {
                            m.push(l, &sample, ts, seg, base)?;
                        }
                        let ready = m
                            .lanes
                            .iter()
                            .zip(&open)
                            .filter(|(_, o)| **o)
                            .map(|((_, lane), _)| lane.end())
                            .min()
                            .unwrap_or(pcm::frames(base + seg.len(), m.rate));
                        m.flush(&writer, ready.min(pcm::frames(base + seg.len(), m.rate)))?;
                    }
                    Target::Video(w) => {
                        if !started {
                            if ts <= seg.start {
                                held = Some(Held { ts, sample });
                                continue;
                            }
                            started = true;
                            if let Some(h) = held.take() {
                                write(w, &h.sample, base, Some(ts.min(seg.end) - seg.start))?;
                            }
                        }
                        let dur = media::duration(&sample);
                        done |= seg.end - ts < dur / 2;
                        if !done {
                            write(w, &sample, out_ts(ts), (dur > 0).then(|| dur.min(seg.end - ts)))?;
                            p.set(out_ts(ts), total);
                        }
                    }
                }
                if done {
                    active.retain(|&a| a != i);
                    let _ = reader.SetStreamSelection(i, false);
                }
            }
            any_video |= started || held.is_some();
            base += seg.len();
            if let Some(m) = &mut mix {
                let end = pcm::frames(base, m.rate);
                for (_, lane) in &mut m.lanes {
                    lane.pad_to(end);
                }
                m.flush(&writer, end)?;
            }
        }
        if !any_video {
            return Err("the selection has no frames".into());
        }
        writer.Finalize().ctx("IMFSinkWriter::Finalize")?;
        let names: Vec<String> = kept.iter().map(|t| t.name.clone()).collect();
        let names = if mix.is_some() { vec![names.join(" + ")] } else { names };
        if !names.is_empty()
            && let Err(e) = crate::record::mp4::name_tracks(out, &names)
        {
            log(&format!("clip: naming the exported audio tracks: {e}"));
        }
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
