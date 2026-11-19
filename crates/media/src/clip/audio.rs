use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use glint_core::clip::edit::{Speed, TimedRange};
use glint_core::clip::math::{Range, SECOND};
use glint_core::clip::pcm::{self, Lane};
use glint_core::log::log;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::IMFSourceReader;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

use super::media::{self, ALL, ANY, Com, Got};
use crate::record::Ctx;

const BUFFER: i64 = SECOND / 12;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

pub struct Gains {
    volume: AtomicU32,
    tracks: Vec<AtomicU32>,
}

impl Gains {
    fn new(tracks: usize, volume: f32) -> Self {
        Gains {
            volume: AtomicU32::new(volume.to_bits()),
            tracks: (0..tracks).map(|_| AtomicU32::new(1f32.to_bits())).collect(),
        }
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn set_track(&self, i: usize, g: f32) {
        if let Some(t) = self.tracks.get(i) {
            t.store(g.to_bits(), Ordering::Relaxed);
        }
    }

    fn get(&self, i: usize) -> f32 {
        let track = self.tracks.get(i).map_or(0.0, |t| f32::from_bits(t.load(Ordering::Relaxed)));
        track * f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
}

pub enum Clock {
    Wall,
    Waiting,
    At(i64),
}

#[derive(Default)]
struct Shared {
    failed: bool,
    epoch: u64,
    at: Option<(i64, Instant, i64)>,
}

enum Cmd {
    Prepare { pieces: Arc<Vec<TimedRange>>, epoch: u64 },
    Play(u64),
    Stop,
}

pub struct Audio {
    tx: Option<Sender<Cmd>>,
    shared: Arc<Mutex<Shared>>,
    pub gains: Arc<Gains>,
    thread: Option<JoinHandle<()>>,
}

impl Audio {
    pub fn start(path: PathBuf, streams: Vec<u32>, volume: f32) -> Self {
        let (tx, cmds) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let gains = Arc::new(Gains::new(streams.len(), volume));
        let (s, g) = (shared.clone(), gains.clone());
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            if let Err(e) = run(&path, &streams, &cmds, &s, &g) {
                log(&format!("clip: audio playback: {e}"));
                if let Ok(mut s) = s.lock() {
                    s.failed = true;
                }
            }
        });
        Audio { tx: Some(tx), shared, gains, thread: Some(thread) }
    }

    pub fn prepare(&self, pieces: Arc<Vec<TimedRange>>, epoch: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Prepare { pieces, epoch });
        }
    }

    pub fn play(&self, epoch: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Play(epoch));
        }
    }

    pub fn stop(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Stop);
        }
    }

    pub fn clock(&self, epoch: u64) -> Clock {
        let Ok(s) = self.shared.lock() else { return Clock::Wall };
        match s.at {
            _ if s.failed => Clock::Wall,
            Some((t, at, submitted)) if s.epoch == epoch => {
                let elapsed = (at.elapsed().as_nanos() / 100) as i64;

                Clock::At((t + elapsed).min(submitted))
            }
            _ => Clock::Waiting,
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Device {
    client: IAudioClient,
    render: IAudioRenderClient,
    clock: IAudioClock,
    freq: u64,
    size: u32,
}

impl Device {
    unsafe fn open(rate: u32) -> Result<Device, String> {
        unsafe {
            let en: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ctx("MMDeviceEnumerator")?;
            let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ctx("no default playback device")?;
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).ctx("IMMDevice::Activate")?;
            let wf = WAVEFORMATEX {
                wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
                nChannels: 2,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * 8,
                nBlockAlign: 8,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER, 0, &wf, None).ctx("IAudioClient::Initialize")?;
            let size = client.GetBufferSize().ctx("IAudioClient::GetBufferSize")?;
            let render: IAudioRenderClient = client.GetService().ctx("IAudioRenderClient")?;
            let clock: IAudioClock = client.GetService().ctx("IAudioClock")?;
            let freq = clock.GetFrequency().ctx("IAudioClock::GetFrequency")?.max(1);
            Ok(Device { client, render, clock, freq, size })
        }
    }

    unsafe fn played(&self) -> Result<i64, String> {
        let mut pos = 0;
        unsafe { self.clock.GetPosition(&mut pos, None).ctx("IAudioClock::GetPosition")? };
        Ok((pos as i128 * SECOND as i128 / self.freq as i128) as i64)
    }

    unsafe fn write(&self, data: &[f32]) -> Result<(), String> {
        let n = (data.len() / 2) as u32;
        unsafe {
            let buf = self.render.GetBuffer(n).ctx("IAudioRenderClient::GetBuffer")?;
            std::ptr::copy_nonoverlapping(data.as_ptr().cast::<u8>(), buf, data.len() * 4);
            self.render.ReleaseBuffer(n, 0).ctx("IAudioRenderClient::ReleaseBuffer")
        }
    }
}

struct Track {
    stream: u32,
    channels: usize,
    lane: Lane,
    ended: bool,
    /// The gain it's playing at, sliding toward the one set.
    gain: f32,
}

struct Source {
    reader: IMFSourceReader,
    rate: u32,
    tracks: Vec<Option<Track>>,
    scratch: Vec<f32>,
    stereo: Vec<f32>,
}

impl Source {
    fn open(path: &std::path::Path, streams: &[u32]) -> Result<Source, String> {
        let reader = media::open(path, None)?;
        unsafe { reader.SetStreamSelection(ALL, false).ctx("SetStreamSelection")? };
        let rate = media::audio_rate(&reader, streams[0]);
        let tracks = streams
            .iter()
            .map(|&stream| {
                unsafe { reader.SetStreamSelection(stream, true).ok()? };
                match media::float_output(&reader, stream, rate) {
                    Ok(channels) => Some(Track { stream, channels, lane: Lane::new(0), ended: false, gain: 0.0 }),
                    Err(e) => {
                        log(&format!("clip: audio stream {stream}: {e}"));
                        unsafe {
                            let _ = reader.SetStreamSelection(stream, false);
                        }
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        if tracks.iter().all(Option::is_none) {
            return Err("no playable audio track".into());
        }
        Ok(Source { reader, rate, tracks, scratch: Vec::new(), stereo: Vec::new() })
    }

    /// Starts over at `t`, at the gains as they're set now: nothing is playing to slide from.
    fn seek(&mut self, t: i64, gains: &Gains) -> Result<(), String> {
        media::seek(&self.reader, t)?;
        let at = pcm::frames(t, self.rate);
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if let Some(tr) = tr {
                tr.lane.reset(at);
                tr.ended = false;
                tr.gain = gains.get(i);
            }
        }
        Ok(())
    }

    fn mix(&mut self, upto: i64, gains: &Gains, out: &mut [f32]) -> Result<(), String> {
        while self.tracks.iter().flatten().any(|t| !t.ended && t.lane.end() < upto) {
            match media::read(&self.reader, ANY)? {
                Got::Sample { stream, ts, sample } => {
                    let Some(tr) = self.tracks.iter_mut().flatten().find(|t| t.stream == stream) else { continue };
                    media::floats(&sample, &mut self.scratch)?;
                    pcm::to_stereo(&self.scratch, tr.channels, &mut self.stereo);
                    tr.lane.push(pcm::frames(ts, self.rate), &self.stereo);
                }
                Got::End(stream) => {
                    if let Some(tr) = self.tracks.iter_mut().flatten().find(|t| t.stream == stream) {
                        tr.ended = true;
                    }
                }
            }
        }
        out.fill(0.0);
        let step = pcm::ramp_step(self.rate);
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if let Some(tr) = tr {
                tr.lane.take_ramped(out, &mut tr.gain, gains.get(i), step);
            }
        }
        for s in out.iter_mut() {
            *s = pcm::soft_clip(*s);
        }
        Ok(())
    }
}

struct Mixer {
    pieces: Arc<Vec<TimedRange>>,
    index: usize,
    offset: i64,
    frame: i64,
    source: PcmCursor,
}

impl Mixer {
    fn new(pieces: Arc<Vec<TimedRange>>) -> Self {
        Self {
            pieces,
            index: 0,
            offset: 0,
            frame: 0,
            source: PcmCursor::default(),
        }
    }

    fn fill(
        &mut self,
        src: &mut Source,
        gains: &Gains,
        mixed: &mut Vec<f32>,
        mut out: &mut [f32],
    ) -> Result<(), String> {
        while !out.is_empty() {
            let Some(&piece) = self.pieces.get(self.index) else {
                out.fill(0.0);
                break;
            };
            let end = self.offset + piece.len();
            let span = AudioSpan::new(
                piece.range,
                Range { start: self.offset, end },
                piece.speed,
                src.rate,
            );
            if self.frame == span.output.start
                && (self.index == 0 || self.pieces[self.index - 1].range.end != piece.range.start)
            {
                src.seek(piece.range.start, gains)?;
                self.source.reset(span.source.start);
            }

            let count = (span.output.end - self.frame).min((out.len() / 2) as i64);
            if count > 0 {
                let (target, tail) = out.split_at_mut(count as usize * 2);
                self.render_piece(span, src, gains, mixed, target)?;
                self.frame += count;
                out = tail;
            }
            if self.frame == span.output.end {
                self.index += 1;
                self.offset = end;
            }
        }

        Ok(())
    }

    fn render_piece(
        &mut self,
        span: AudioSpan,
        src: &mut Source,
        gains: &Gains,
        mixed: &mut Vec<f32>,
        out: &mut [f32],
    ) -> Result<(), String> {
        let to = self.frame + (out.len() / 2) as i64;
        let window = span.window(self.frame, to);
        if span.step == PHASES
            && window.start == self.source.next
            && window.len() == to - self.frame
        {
            src.mix(window.end, gains, out)?;
            self.source.advance(out);

            return Ok(());
        }

        let base = self.source.mix(window, mixed, |end, fresh| src.mix(end, gains, fresh))?;
        for (frame, sample) in out.chunks_exact_mut(2).zip(span.samples(mixed, base, self.frame, to)) {
            frame.copy_from_slice(&sample);
        }

        Ok(())
    }
}

const PHASES: i64 = 1_000;

#[derive(Clone, Copy)]
pub(super) struct AudioSpan {
    pub(super) source: Range,
    pub(super) output: Range,
    step: i64,
}

impl AudioSpan {
    pub(super) fn new(source: Range, output: Range, speed: Speed, rate: u32) -> Self {
        let frames = |t| pcm::frames(t, rate);
        let start = frames(source.start);

        Self {
            source: Range { start, end: frames(source.end).max(start + 1) },
            output: Range { start: frames(output.start), end: frames(output.end) },
            step: i64::from(speed.milli()),
        }
    }

    fn phase(self, output: i64) -> i64 {
        (output - self.output.start) * self.step
    }

    pub(super) fn available(self, ready: i64) -> i64 {
        if ready >= self.source.end {
            return self.output.end;
        }

        let last = (ready - self.source.start - 1) * PHASES;
        self.output.start + (last.div_euclid(self.step) + 1).clamp(0, self.output.len())
    }

    pub(super) fn window(self, from: i64, to: i64) -> Range {
        let last = self.source.end - 1;
        let at = |phase: i64| (self.source.start + phase / PHASES).min(last);

        Range {
            start: at(self.phase(from)),
            end: at(self.phase(to - 1) + PHASES - 1) + 1,
        }
    }

    pub(super) fn samples(
        self,
        mixed: &[f32],
        base: i64,
        from: i64,
        to: i64,
    ) -> impl Iterator<Item = [f32; 2]> + '_ {
        let (mixed, _) = mixed.as_chunks::<2>();
        let last = mixed.len() - 1;
        let mut phase = self.phase(from) + (self.source.start - base) * PHASES;

        (from..to).map(move |_| {
            let left = ((phase / PHASES) as usize).min(last);
            let subframe = phase % PHASES;
            phase += self.step;
            let [l, r] = mixed[left];
            if subframe == 0 || left == last {
                return [l, r];
            }

            let [next_l, next_r] = mixed[left + 1];
            let fraction = subframe as f32 / PHASES as f32;

            [l + (next_l - l) * fraction, r + (next_r - r) * fraction]
        })
    }
}

#[derive(Default)]
pub(super) struct PcmCursor {
    next: i64,
    tail: [[f32; 2]; 2],
}

impl PcmCursor {
    pub(super) fn reset(&mut self, start: i64) {
        self.next = start;
        self.tail = [[0.0; 2]; 2];
    }

    fn advance(&mut self, fresh: &[f32]) {
        let (frames, _) = fresh.as_chunks::<2>();
        match frames {
            [] => {}
            [only] => self.tail = [self.tail[1], *only],
            [.., previous, last] => self.tail = [*previous, *last],
        }
        self.next += frames.len() as i64;
    }

    pub(super) fn mix(
        &mut self,
        window: Range,
        scratch: &mut Vec<f32>,
        mix: impl FnOnce(i64, &mut [f32]) -> Result<(), String>,
    ) -> Result<i64, String> {
        let first = window.start.min(self.next);
        let carry = (self.next - first) as usize;
        scratch.resize((window.end - first) as usize * 2, 0.0);
        // several output frames below 1x can share both sides of the same interpolation.
        scratch[..carry * 2].copy_from_slice(self.tail[2 - carry..].as_flattened());
        let fresh = &mut scratch[carry * 2..];
        if !fresh.is_empty() {
            mix(window.end, fresh)?;
            self.advance(fresh);
        }

        Ok(first)
    }
}

fn run(
    path: &std::path::Path,
    streams: &[u32],
    cmds: &Receiver<Cmd>,
    shared: &Mutex<Shared>,
    gains: &Gains,
) -> Result<(), String> {
    let mut src = Source::open(path, streams)?;
    let dev = unsafe { Device::open(src.rate)? };
    let publish = |f: &mut dyn FnMut(&mut Shared)| {
        if let Ok(mut s) = shared.lock() {
            f(&mut s);
        }
    };
    let mut playing: Option<(u64, Mixer)> = None;
    let mut prepared = None;
    let mut ready = None;
    let mut submitted = 0;
    let mut mixed = Vec::new();
    let mut out = Vec::new();
    loop {
        let first = if prepared.as_ref().is_some_and(|(epoch, _)| ready == Some(*epoch)) {
            match cmds.try_recv() {
                Ok(c) => Some(c),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        } else if playing.is_some() {
            match cmds.recv_timeout(Duration::from_millis(5)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match cmds.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };
        if let Some(first) = first {
            let mut replace = None;
            for cmd in std::iter::once(first).chain(cmds.try_iter()) {
                match cmd {
                    Cmd::Prepare { pieces, epoch } => {
                        replace = Some(Some((epoch, pieces)));
                        ready = None;
                    }
                    Cmd::Stop => {
                        replace = Some(None);
                        ready = None;
                    }
                    Cmd::Play(epoch) => ready = Some(epoch),
                }
            }
            if let Some(next) = replace {
                if playing.take().is_some() {
                    unsafe {
                        dev.client.Stop().ctx("IAudioClient::Stop")?;
                        dev.client.Reset().ctx("IAudioClient::Reset")?;
                    }
                }
                publish(&mut |s| s.at = None);
                prepared = None;
                if let Some((epoch, pieces)) = next {
                    let mut mixer = Mixer::new(pieces);
                    out.resize(dev.size as usize * 2, 0.0);
                    mixer.fill(&mut src, gains, &mut mixed, &mut out)?;
                    prepared = Some((epoch, mixer));
                }
                continue;
            }
        }
        if prepared.as_ref().is_some_and(|(epoch, _)| ready == Some(*epoch)) {
            unsafe {
                dev.write(&out)?;
                dev.client.Start().ctx("IAudioClient::Start")?;
            }
            submitted = i64::from(dev.size);
            playing = prepared.take();
            ready = None;
        }
        let Some((epoch, mixer)) = &mut playing else { continue };
        let padding = unsafe { dev.client.GetCurrentPadding().ctx("IAudioClient::GetCurrentPadding")? };
        let free = (dev.size - padding.min(dev.size)) as usize;
        if free > 0 {
            out.resize(free * 2, 0.0);
            mixer.fill(&mut src, gains, &mut mixed, &mut out)?;
            unsafe { dev.write(&out)? };
            submitted += free as i64;
        }
        let played = unsafe { dev.played()? };
        let at = Instant::now();
        let submitted = pcm::hns(submitted, src.rate);
        publish(&mut |s| {
            s.epoch = *epoch;
            s.at = Some((played, at, submitted));
        });
    }
    unsafe {
        let _ = dev.client.Stop();
    }
    Ok(())
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod tests;
