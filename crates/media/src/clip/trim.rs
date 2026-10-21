use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use glint_core::clip::crop::Crop;
use glint_core::clip::export::{AAC_BYTES_PER_SECOND, Job, Video, VideoJob};
use glint_core::clip::info::Info;
use glint_core::clip::math::{Range, SECOND};
use glint_core::clip::pcm::{self, Lane};
use glint_core::log::log;
use glint_core::record::Codec;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::HSTRING;

use super::gif;
use super::media::{self, ANY, Got, Gpu, attributes, pack};
use super::process::Processor;
use crate::record::Ctx;

pub struct Progress<'a> {
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

fn target(job: &Job) -> Option<u64> {
    match job {
        Job::Video(v) => v.target,
        Job::Gif(g) => g.target,
    }
}

/// Writes `job` to `out`. When a size target was missed, one more pass runs with the bitrate (or the gif's
/// size) cut by the overshoot.
pub fn export(src: &Path, info: &Info, job: &Job, out: &Path, effort: u32, p: &Progress) -> Result<bool, String> {
    passes(src, info, job, out, effort, p, true)
}

#[cfg(test)]
pub(super) fn export_in_software(src: &Path, info: &Info, job: &Job, out: &Path, p: &Progress) -> Result<bool, String> {
    passes(src, info, job, out, 60, p, false)
}

fn passes(src: &Path, info: &Info, job: &Job, out: &Path, effort: u32, p: &Progress, gpu: bool) -> Result<bool, String> {
    let mut job = job.clone();
    let mut pass = 0;
    loop {
        let res = match &job {
            Job::Video(v) => video(src, info, v, out, effort, p, gpu),
            Job::Gif(g) => gif::export(src, info, g, out, p),
        };
        let (Ok(true), Some(limit)) = (&res, target(&job)) else { return res };
        let size = std::fs::metadata(out).map_or(0, |m| m.len());
        if size <= limit || pass > 0 {
            return res;
        }
        let k = limit as f64 / size as f64 * 0.95;
        log(&format!("clip: {size} bytes missed the {limit} byte target, another pass at {k:.2}×"));
        match &mut job {
            Job::Video(VideoJob { video: Video::Encode { bitrate, .. }, .. }) => {
                *bitrate = ((*bitrate as f64 * k) as u32).max(100_000);
            }
            Job::Gif(g) => {
                let s = k.sqrt();
                g.size = ((((g.size.0 as f64 * s) as u32) & !1).max(64), (((g.size.1 as f64 * s) as u32) & !1).max(64));
            }
            Job::Video(_) => return res,
        }
        pass += 1;
        p.set(0, 1);
    }
}

fn video(src: &Path, info: &Info, job: &VideoJob, out: &Path, effort: u32, p: &Progress, gpu: bool) -> Result<bool, String> {
    let res = if job.video == Video::Copy || !gpu {
        run(src, info, job, out, effort, p, None)
    } else {
        match Gpu::new() {
            Ok(gpu) => match run(src, info, job, out, effort, p, Some(&gpu)) {
                Err(e) if !p.cancelled() => {
                    log(&format!("export on the gpu failed, retrying in software: {e}"));
                    let _ = std::fs::remove_file(out);
                    run(src, info, job, out, effort, p, None)
                }
                r => r,
            },
            Err(e) => {
                log(&format!("no gpu for exporting: {e}"));
                run(src, info, job, out, effort, p, None)
            }
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
    Encode(u32),
    Remux(u32),
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

// the aac encoder only takes 12000, 16000, 20000 or 24000 bytes per second.
const SILENCE_AAC_BYTES_PER_SECOND: u64 = 12_000;

unsafe fn aac_stream(writer: &IMFSinkWriter, rate: u32, channels: u32, bytes_per_second: u64) -> Result<u32, String> {
    unsafe {
        let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        out_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bytes_per_second as u32).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29).ctx("mt")?;
        let stream = writer.AddStream(&out_t).ctx("IMFSinkWriter::AddStream (audio)")?;
        let in_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        in_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ctx("mt")?;
        in_t.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, rate).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, channels).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 2 * channels).ctx("mt")?;
        in_t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, rate * 2 * channels).ctx("mt")?;
        writer.SetInputMediaType(stream, &in_t, None).ctx("IMFSinkWriter::SetInputMediaType (audio)")?;
        Ok(stream)
    }
}

unsafe fn write_pcm(writer: &IMFSinkWriter, stream: u32, rate: u32, from: i64, to: i64, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let buf = MFCreateMemoryBuffer(bytes.len() as u32).ctx("MFCreateMemoryBuffer")?;
        let mut ptr = std::ptr::null_mut();
        buf.Lock(&mut ptr, None, None).ctx("IMFMediaBuffer::Lock")?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = buf.Unlock();
        buf.SetCurrentLength(bytes.len() as u32).ctx("SetCurrentLength")?;
        let sample = MFCreateSample().ctx("MFCreateSample")?;
        sample.AddBuffer(&buf).ctx("AddBuffer")?;
        let (t0, t1) = (pcm::hns(from, rate), pcm::hns(to, rate));
        sample.SetSampleTime(t0).ctx("SetSampleTime")?;
        sample.SetSampleDuration(t1 - t0).ctx("SetSampleDuration")?;
        writer.WriteSample(stream, &sample).ctx("WriteSample (audio)")
    }
}

fn aac_rate(rate: u32) -> bool {
    rate == 44_100 || rate == 48_000
}

impl Mixdown {
    unsafe fn new(writer: &IMFSinkWriter, rate: u32) -> Result<Mixdown, String> {
        if !aac_rate(rate) {
            return Err(format!("audio at {rate} Hz can't be mixed into one track"));
        }
        let stream = unsafe { aac_stream(writer, rate, 2, AAC_BYTES_PER_SECOND)? };
        Ok(Mixdown { stream, rate, lanes: Vec::new(), written: 0, scratch: Vec::new(), stereo: Vec::new() })
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
        unsafe { write_pcm(writer, self.stream, self.rate, self.written, upto, &pcm::to_i16(&out))? };
        self.written = upto;
        Ok(())
    }
}

struct Silence {
    stream: u32,
    rate: u32,
    written: i64,
    zeros: Vec<u8>,
}

impl Silence {
    unsafe fn new(writer: &IMFSinkWriter, rate: u32, channels: u32) -> Result<Silence, String> {
        let stream = unsafe { aac_stream(writer, rate, channels, SILENCE_AAC_BYTES_PER_SECOND)? };
        Ok(Silence { stream, rate, written: 0, zeros: vec![0; (rate / 10 * 2 * channels) as usize] })
    }

    unsafe fn fill(&mut self, writer: &IMFSinkWriter, upto: i64) -> Result<(), String> {
        let upto = pcm::frames(upto, self.rate);
        let chunk = self.rate as i64 / 10;
        while self.written < upto {
            let to = upto.min(self.written + chunk);
            let bytes = self.zeros.len() * (to - self.written) as usize / chunk as usize;
            unsafe { write_pcm(writer, self.stream, self.rate, self.written, to, &self.zeros[..bytes])? };
            self.written = to;
        }
        Ok(())
    }
}

fn silence_format(reader: &IMFSourceReader, info: &Info, job: &VideoJob) -> (u32, u32) {
    let first = job.tracks.iter().find_map(|t| t.source).or((!info.tracks.is_empty()).then_some(0));
    let native = first.and_then(|k| info.tracks.get(k)).and_then(|t| unsafe { reader.GetNativeMediaType(t.stream, 0).ok() });
    let Some(native) = native else { return (48_000, 2) };
    let rate = unsafe { native.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND).unwrap_or(48_000) };
    let channels = unsafe { native.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(2) };
    (if aac_rate(rate) { rate } else { 48_000 }, if channels == 1 && !job.combine { 1 } else { 2 })
}

/// Where a copied piece really starts: the keyframe media foundation lands on when seeking to `start`.
fn keyframe(reader: &IMFSourceReader, video: u32, start: i64) -> Result<Option<i64>, String> {
    unsafe {
        reader.SetStreamSelection(media::ALL, false).ctx("SetStreamSelection")?;
        reader.SetStreamSelection(video, true).ctx("SetStreamSelection")?;
    }
    media::seek(reader, start)?;
    match media::read(reader, video)? {
        Got::Sample { ts, .. } => Ok(Some(ts)),
        Got::End(_) => Ok(None),
    }
}

fn run(
    src: &Path,
    info: &Info,
    job: &VideoJob,
    out: &Path,
    effort: u32,
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

        let mut targets: Vec<Option<Target>> = vec![None; streams.len()];
        reader.SetStreamSelection(media::ALL, false).ctx("SetStreamSelection")?;
        let Some(&(video, ..)) = streams.iter().find(|s| s.1 == MFMediaType_Video) else {
            return Err("the file has no video".into());
        };
        reader.SetStreamSelection(video, true).ctx("SetStreamSelection")?;
        let mut processor = None;
        targets[video as usize] = Some(match job.video {
            Video::Copy => {
                let native = &streams[video as usize].2;
                let w = writer.AddStream(native).ctx("can't copy the video")?;
                writer.SetInputMediaType(w, native, None).ctx("can't copy the video")?;
                Target::Remux(w)
            }
            Video::Encode { crop, size, bitrate, fps, codec } => {
                media::set_video_output(&reader, MFVideoFormat_NV12, info.size)?;
                let rate = fps.map_or(info.fps, f64::from);
                if crop.is_some() || size != info.size {
                    let area = crop.unwrap_or(Crop::full(info.size));
                    processor = Some(Processor::new(gpu, info.size, area, size, rate)?);
                }
                Target::Encode(video_stream(&writer, codec, size, rate, bitrate, effort)?)
            }
        });
        let (silent_rate, silent_channels) = silence_format(&reader, info, job);
        let mut mix: Option<Mixdown> = None;
        let mut silences = Vec::new();
        let mut names = Vec::with_capacity(job.tracks.len());
        for t in &job.tracks {
            let Some(source) = t.source.and_then(|k| info.tracks.get(k)) else {
                silences.push(Silence::new(&writer, silent_rate, silent_channels)?);
                names.push(t.name.clone());
                continue;
            };
            let (i, native) = (source.stream, &streams[source.stream as usize].2);
            reader.SetStreamSelection(i, true).ctx("SetStreamSelection")?;
            targets[i as usize] = Some(if job.combine {
                let m = match &mut mix {
                    Some(m) => m,
                    None => {
                        let sources = job.tracks.iter().filter(|t| t.source.is_some());
                        names.push(sources.map(|t| t.name.as_str()).collect::<Vec<_>>().join(" + "));
                        mix.insert(Mixdown::new(&writer, media::audio_rate(&reader, i))?)
                    }
                };
                let channels = media::float_output(&reader, i, m.rate)?;
                m.lanes.push((channels, Lane::new(0)));
                Target::Mix(m.lanes.len() - 1)
            } else {
                reader.SetCurrentMediaType(i, None, native).ctx("audio type")?;
                let w = writer.AddStream(native).ctx("can't copy the audio")?;
                writer.SetInputMediaType(w, native, None).ctx("can't copy the audio")?;
                names.push(t.name.clone());
                Target::Copy(w)
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
        let frame = |s: &IMFSample| -> Result<IMFSample, String> {
            match &processor {
                Some(proc) => proc.run(s),
                None => Ok(s.clone()),
            }
        };
        let grid = match job.video {
            Video::Encode { fps, .. } => fps,
            Video::Copy => None,
        };
        let total: i64 = job.pieces.iter().map(|pc| pc.end - pc.lead).sum();
        let mut copied_to = vec![0i64; streams.len()];
        let mut any_video = false;
        let mut base = 0;
        for piece in &job.pieces {
            let lead = match job.video {
                Video::Copy => match keyframe(&reader, video, piece.start)? {
                    Some(k) if k < piece.end => k,
                    _ => continue,
                },
                Video::Encode { .. } => piece.start,
            };
            let seg = Range { start: lead, end: piece.end };
            let mut active: Vec<u32> = (0..streams.len() as u32).filter(|&i| targets[i as usize].is_some()).collect();
            for &i in &active {
                reader.SetStreamSelection(i, true).ctx("SetStreamSelection")?;
            }
            media::seek(&reader, lead)?;
            let mut open: Vec<bool> = mix.as_ref().map_or(Vec::new(), |m| vec![true; m.lanes.len()]);
            let mut held: Option<Held> = None;
            let mut started = false;
            let mut pending: Option<IMFSample> = None;
            let mut tick = 0i64;
            let out_ts = |ts: i64| ts - seg.start + base;
            while !active.is_empty() {
                if p.cancelled() {
                    return Ok(false);
                }
                let (i, ts, sample) = match media::read(&reader, ANY)? {
                    Got::Sample { stream, ts, sample } => (stream, ts, sample),
                    Got::End(i) => {
                        match targets.get(i as usize).copied().flatten() {
                            Some(Target::Encode(w)) => {
                                if let (Some(fps), Some(prev)) = (grid, &pending) {
                                    tick = on_grid(fps, tick, seg.len(), |t0, t1| {
                                        write(w, &copy_of(&frame(prev)?)?, base + t0, Some(t1 - t0))
                                    })?;
                                    started = true;
                                } else if let Some(h) = held.take() {
                                    let d = media::duration(&h.sample);
                                    let stop = if d > 0 { (h.ts + d).min(seg.end) } else { seg.end };
                                    write(w, &frame(&h.sample)?, base, Some((stop - seg.start).max(1)))?;
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
                for s in &mut silences {
                    s.fill(&writer, out_ts(ts.clamp(seg.start, seg.end)))?;
                }
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
                    Target::Remux(w) => {
                        if !done && ts >= seg.start {
                            if let Ok(dts) = sample.GetUINT64(&MFSampleExtension_DecodeTimestamp) {
                                let shifted = dts as i64 - seg.start + base;
                                sample.SetUINT64(&MFSampleExtension_DecodeTimestamp, shifted as u64).ctx("dts")?;
                            }
                            write(w, &sample, out_ts(ts), None)?;
                            started = true;
                            p.set(out_ts(ts), total);
                        }
                    }
                    Target::Encode(w) if grid.is_some() => {
                        let fps = grid.unwrap_or(1);
                        if let Some(prev) = &pending {
                            let until = ts.min(seg.end) - seg.start;
                            let before = tick;
                            tick = on_grid(fps, tick, until, |t0, t1| write(w, &copy_of(&frame(prev)?)?, base + t0, Some(t1 - t0)))?;
                            started |= tick > before;
                            p.set(base + until, total);
                        }
                        if !done {
                            pending = Some(sample);
                        }
                    }
                    Target::Encode(w) => {
                        if !started {
                            if ts <= seg.start {
                                held = Some(Held { ts, sample });
                                continue;
                            }
                            started = true;
                            if let Some(h) = held.take() {
                                write(w, &frame(&h.sample)?, base, Some(ts.min(seg.end) - seg.start))?;
                            }
                        }
                        let dur = media::duration(&sample);
                        done |= seg.end - ts < dur / 2;
                        if !done {
                            write(w, &frame(&sample)?, out_ts(ts), (dur > 0).then(|| dur.min(seg.end - ts)))?;
                            p.set(out_ts(ts), total);
                        }
                    }
                }
                if done {
                    active.retain(|&a| a != i);
                    let _ = reader.SetStreamSelection(i, false);
                }
            }
            any_video |= started || held.is_some() || pending.is_some();
            base += seg.len();
            for s in &mut silences {
                s.fill(&writer, base)?;
            }
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
        drop(processor);
        if !names.is_empty()
            && let Err(e) = glint_core::record::mp4::name_tracks(out, &names)
        {
            log(&format!("clip: naming the exported audio tracks: {e}"));
        }
        p.set(1, 1);
        Ok(true)
    }
}

/// Writes the frames of the `fps` grid from tick `k` up to `until` (from the piece's start) with `emit(start, end)`;
/// returns the next tick.
fn on_grid(fps: u32, mut k: i64, until: i64, mut emit: impl FnMut(i64, i64) -> Result<(), String>) -> Result<i64, String> {
    let at = |k: i64| (k as i128 * SECOND as i128 / fps.max(1) as i128) as i64;
    while at(k) < until {
        emit(at(k), at(k + 1))?;
        k += 1;
    }
    Ok(k)
}

/// A new sample over the same pixels, so one decoded frame can sit on several ticks with its own time.
fn copy_of(s: &IMFSample) -> Result<IMFSample, String> {
    unsafe {
        let out = MFCreateSample().ctx("MFCreateSample")?;
        out.AddBuffer(&s.GetBufferByIndex(0).ctx("GetBufferByIndex")?).ctx("AddBuffer")?;
        Ok(out)
    }
}

unsafe fn video_stream(writer: &IMFSinkWriter, codec: Codec, size: (u32, u32), fps: f64, bitrate: u32, effort: u32) -> Result<u32, String> {
    unsafe {
        let (w, h) = size;
        let (subtype, profile) = match codec {
            Codec::H264 => (MFVideoFormat_H264, eAVEncH264VProfile_High.0 as u32),
            Codec::Hevc => (MFVideoFormat_HEVC, eAVEncH265VProfile_Main_420_8.0 as u32),
        };
        let fps_milli = (fps * 1000.0).round() as u32;
        let out_t = MFCreateMediaType().ctx("MFCreateMediaType")?;
        out_t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ctx("mt")?;
        out_t.SetGUID(&MF_MT_SUBTYPE, &subtype).ctx("mt")?;
        out_t.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).ctx("mt")?;
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

        let enc = attributes(5)?;
        let mode = eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32;
        enc.SetUINT32(&CODECAPI_AVEncCommonRateControlMode, mode).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncCommonMeanBitRate, bitrate).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncCommonMaxBitRate, bitrate.saturating_mul(6) / 5).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncCommonQualityVsSpeed, effort.min(100)).ctx("enc")?;
        enc.SetUINT32(&CODECAPI_AVEncMPVGOPSize, (fps * 2.0).round() as u32).ctx("enc")?;
        writer.SetInputMediaType(stream, &in_t, &enc).ctx("IMFSinkWriter::SetInputMediaType")?;
        Ok(stream)
    }
}
