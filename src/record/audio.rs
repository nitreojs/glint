use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};

use super::mix::{Format, HNS, Mix, Placement, Sample, frames_to_hns, hns_to_frames};
use super::{Ctx, Msg, hns, qpc};
use crate::platform::log::log;

const LATENCY: i64 = HNS / 10;
const BUFFER: i64 = HNS / 2;
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioSource {
    Desktop,
    Mic,
}

impl AudioSource {
    pub(crate) fn short_name(self) -> &'static str {
        match self {
            AudioSource::Desktop => "desktop",
            AudioSource::Mic => "mic",
        }
    }

    fn name(self) -> &'static str {
        match self {
            AudioSource::Desktop => "Desktop audio",
            AudioSource::Mic => "Microphone",
        }
    }
}

pub(super) struct Device {
    pub source: AudioSource,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: Format,
    placement: Placement,
}

/// the mic uses the console role: opening the communications device makes windows duck other sounds.
pub(super) unsafe fn open(source: AudioSource) -> Result<Device, String> {
    unsafe {
        let open = || -> Result<Device, String> {
            let en: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ctx("MMDeviceEnumerator")?;
            let (flow, flags, what) = match source {
                AudioSource::Desktop => (eRender, AUDCLNT_STREAMFLAGS_LOOPBACK, "no default playback device"),
                AudioSource::Mic => (eCapture, 0, "no default microphone"),
            };
            let dev = en.GetDefaultAudioEndpoint(flow, eConsole).ctx(what)?;
            let client: IAudioClient = dev.Activate(CLSCTX_ALL, None).ctx("IMMDevice::Activate")?;
            let mix = client.GetMixFormat().ctx("IAudioClient::GetMixFormat")?;
            let format = format_of(mix);
            let init = format.is_ok().then(|| client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER, 0, mix, None));
            CoTaskMemFree(Some(mix as _));
            let format = format?;
            if let Some(r) = init {
                r.ctx("IAudioClient::Initialize")?;
            }
            let capture: IAudioCaptureClient = client.GetService().ctx("IAudioClient::GetService")?;
            let placement = Placement::new(format.rate);
            Ok(Device { source, client, capture, format, placement })
        };
        open().map_err(|e| format!("{} unavailable: {e}", source.name()))
    }
}

unsafe fn format_of(wf: *const WAVEFORMATEX) -> Result<Format, String> {
    unsafe {
        let w = std::ptr::read_unaligned(wf);
        let (tag, mask) = if w.wFormatTag == WAVE_FORMAT_EXTENSIBLE && w.cbSize >= 22 {
            let x = std::ptr::read_unaligned(wf as *const WAVEFORMATEXTENSIBLE);
            (x.SubFormat.data1 as u16, x.dwChannelMask)
        } else {
            (w.wFormatTag, 0)
        };
        let (bits, rate, channels, block) = (w.wBitsPerSample, w.nSamplesPerSec, w.nChannels, w.nBlockAlign);
        let sample = match (tag, bits) {
            (WAVE_FORMAT_IEEE_FLOAT, 32) => Sample::F32,
            (WAVE_FORMAT_PCM, 16) => Sample::I16,
            (WAVE_FORMAT_PCM, 24) => Sample::I24,
            (WAVE_FORMAT_PCM, 32) => Sample::I32,
            _ => return Err(format!("unsupported mix format (tag {tag:#x}, {bits} bits)")),
        };
        let format = Format::new(rate, sample, channels as usize, mask)?;
        if format.block != block as usize {
            return Err(format!("unexpected block size {block} for {channels} channels of {bits} bits"));
        }
        Ok(format)
    }
}

impl Device {
    unsafe fn read(&mut self, origin: i64, mix: &mut Mix, scratch: &mut Vec<f32>) -> windows::core::Result<()> {
        unsafe {
            while self.capture.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags, mut at) = (std::ptr::null_mut(), 0u32, 0u32, 0u64);
                self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut at))?;
                scratch.clear();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    scratch.resize(frames as usize * 2, 0.0);
                } else {
                    let bytes = std::slice::from_raw_parts(data, frames as usize * self.format.block);
                    self.format.decode(bytes, scratch);
                }
                self.capture.ReleaseBuffer(frames)?;
                // u64QPCPosition is the capture time in 100 ns units of the performance counter
                let at = (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0)
                    .then(|| hns_to_frames(at as i64 - origin));
                let gap = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
                self.placement.push(at, scratch, gap, mix);
            }
            Ok(())
        }
    }
}

pub(super) unsafe fn run(mut devices: Vec<Device>, stop: &AtomicBool, tx: &Sender<Msg>) -> Vec<String> {
    unsafe {
        let mut issues = Vec::new();
        devices.retain(|d| match d.client.Start() {
            Ok(()) => true,
            Err(e) => {
                issues.push(format!("{} failed to start: {}", d.source.name(), e.message()));
                false
            }
        });
        let origin = hns(qpc());
        let mut mix = Mix::default();
        let mut scratch = Vec::new();
        let mut live = vec![true; devices.len()];
        loop {
            let stopping = stop.load(Ordering::SeqCst);
            for (d, live) in devices.iter_mut().zip(live.iter_mut()).filter(|(_, l)| **l) {
                if let Err(e) = d.read(origin, &mut mix, &mut scratch) {
                    let text = format!("{} stopped: {}", d.source.name(), e.message());
                    log(&format!("audio: {text} (0x{:08X})", e.code().0 as u32));
                    issues.push(text);
                    *live = false;
                }
            }
            let behind = if stopping { 0 } else { LATENCY };
            let upto = hns_to_frames(hns(qpc()) - origin - behind);
            if upto > mix.base() {
                let start = origin + frames_to_hns(mix.base());
                if tx.send(Msg::Audio { start, data: mix.take(upto) }).is_err() {
                    break;
                }
            }
            if stopping {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for d in &devices {
            let _ = d.client.Stop();
        }
        let _ = tx.send(Msg::AudioEnd);
        issues
    }
}
