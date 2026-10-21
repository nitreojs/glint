use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::Audio::*;
use windows::Win32::Media::MediaFoundation::IMFSourceReader;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

use super::math::SECOND;
use super::media::{self, ALL, ANY, Com, Got};
use super::pcm::{self, Lane};
use crate::platform::log::log;
use crate::record::Ctx;

const BUFFER: i64 = SECOND / 12;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

pub(super) struct Gains {
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

    pub(super) fn set_volume(&self, v: f32) {
        self.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn set_track(&self, i: usize, g: f32) {
        if let Some(t) = self.tracks.get(i) {
            t.store(g.to_bits(), Ordering::Relaxed);
        }
    }

    fn get(&self, i: usize) -> f32 {
        let track = self.tracks.get(i).map_or(0.0, |t| f32::from_bits(t.load(Ordering::Relaxed)));
        track * f32::from_bits(self.volume.load(Ordering::Relaxed))
    }
}

pub(super) enum Clock {
    Wall,
    Waiting,
    At(i64),
}

#[derive(Default)]
struct Shared {
    failed: bool,
    epoch: u64,
    at: Option<(i64, Instant)>,
}

enum Cmd {
    Play { t: i64, epoch: u64 },
    Stop,
}

pub(super) struct Audio {
    tx: Option<Sender<Cmd>>,
    shared: Arc<Mutex<Shared>>,
    pub gains: Arc<Gains>,
    thread: Option<JoinHandle<()>>,
}

impl Audio {
    pub(super) fn start(path: PathBuf, streams: Vec<u32>, volume: f32) -> Self {
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

    pub(super) fn play(&self, t: i64, epoch: u64) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Play { t, epoch });
        }
    }

    pub(super) fn stop(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Stop);
        }
    }

    pub(super) fn clock(&self, epoch: u64) -> Clock {
        let Ok(s) = self.shared.lock() else { return Clock::Wall };
        match s.at {
            _ if s.failed => Clock::Wall,
            Some((t, at)) if s.epoch == epoch => Clock::At(t + (at.elapsed().as_secs_f64() * SECOND as f64) as i64),
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
                    Ok(channels) => Some(Track { stream, channels, lane: Lane::new(0), ended: false }),
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

    fn seek(&mut self, t: i64) -> Result<(), String> {
        media::seek(&self.reader, t)?;
        let at = pcm::frames(t, self.rate);
        for tr in self.tracks.iter_mut().flatten() {
            tr.lane.reset(at);
            tr.ended = false;
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
        for (i, tr) in self.tracks.iter_mut().enumerate() {
            if let Some(tr) = tr {
                tr.lane.take_into(out, gains.get(i));
            }
        }
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        Ok(())
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
    let mut playing: Option<i64> = None;
    let mut cursor = 0;
    let mut out = Vec::new();
    loop {
        let first = if playing.is_some() {
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
            unsafe {
                let _ = dev.client.Stop();
                let _ = dev.client.Reset();
            }
            match cmds.try_iter().last().unwrap_or(first) {
                Cmd::Play { t, epoch } => {
                    src.seek(t)?;
                    cursor = pcm::frames(t, src.rate);
                    playing = Some(t);
                    publish(&mut |s| {
                        s.epoch = epoch;
                        s.at = None;
                    });
                    out.resize(dev.size as usize * 2, 0.0);
                    src.mix(cursor + dev.size as i64, gains, &mut out)?;
                    unsafe { dev.write(&out)? };
                    cursor += dev.size as i64;
                    unsafe { dev.client.Start().ctx("IAudioClient::Start")? };
                }
                Cmd::Stop => {
                    playing = None;
                    publish(&mut |s| s.at = None);
                }
            }
        }
        let Some(t0) = playing else { continue };
        let padding = unsafe { dev.client.GetCurrentPadding().ctx("IAudioClient::GetCurrentPadding")? };
        let free = (dev.size - padding.min(dev.size)) as usize;
        if free > 0 {
            out.resize(free * 2, 0.0);
            src.mix(cursor + free as i64, gains, &mut out)?;
            unsafe { dev.write(&out)? };
            cursor += free as i64;
        }
        let played = unsafe { dev.played()? };
        publish(&mut |s| s.at = Some((t0 + played, Instant::now())));
    }
    unsafe {
        let _ = dev.client.Stop();
    }
    Ok(())
}
