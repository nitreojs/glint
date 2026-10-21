use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use windows::Win32::Devices::FunctionDiscovery::{PKEY_Device_FriendlyName, PKEY_DeviceInterface_FriendlyName};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, STGM_READ,
};
use windows::Win32::System::Variant::{VT_BLOB, VT_LPWSTR};
use windows::core::{HSTRING, Interface, Ref, implement};

use super::apps::{Proc, processes, roots};
use super::mix::{Format, HNS, Mix, Placement, RATE, Sample, frames_to_hns, hns_to_frames, mix_tracks};
use super::tracks::{Track, plan};
use super::{Ctx, Msg, hns, qpc};
use crate::config::audio::{AudioSource, SourceKind, app_name};
use crate::platform::log::log;

const LATENCY: i64 = HNS / 10;
const BUFFER: i64 = HNS / 2;
const RESCAN: Duration = Duration::from_secs(1);
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(2);
const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

struct Client {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: Format,
    placement: Placement,
    pid: Option<u32>,
}

enum Target {
    Endpoint,
    App { exe: String },
    DesktopExcept { exe: String },
}

pub(super) struct Source {
    pub name: String,
    pub gain: f32,
    target: Target,
    clients: Vec<Client>,
    mix: Mix,
    seen_app: bool,
    failed: Vec<u32>,
    live: bool,
}

pub(super) struct Opened {
    pub sources: Vec<Source>,
    pub tracks: Vec<Track>,
    pub issues: Vec<String>,
}

pub(super) unsafe fn open_all(cfg: &[AudioSource], track_names: &[String]) -> Opened {
    let (mut sources, mut opened, mut issues) = (Vec::new(), Vec::new(), Vec::new());
    for c in cfg.iter().filter(|c| c.enabled && c.records()) {
        match unsafe { open(c) } {
            Ok(s) => {
                sources.push(s);
                opened.push(c);
            }
            Err(e) => {
                log(&format!("audio: {e}"));
                issues.push(e);
            }
        }
    }
    let names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
    Opened { tracks: plan(&names, &opened, track_names), sources, issues }
}

unsafe fn open(cfg: &AudioSource) -> Result<Source, String> {
    unsafe {
        let gain = cfg.volume as f32 / 100.0;
        let source = |name: String, target, clients: Vec<Client>| {
            let seen_app = !clients.is_empty();
            Source { name, gain, target, clients, mix: Mix::default(), seen_app, failed: Vec::new(), live: true }
        };
        match cfg.kind {
            SourceKind::Desktop if !app_name(&cfg.exclude).is_empty() => {
                let exe = app_name(&cfg.exclude);
                let client = match roots(&processes(), &exe).first() {
                    Some(&pid) => process_loopback(pid, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
                    None => endpoint(eRender, None),
                };
                let client = client.map_err(|e| format!("desktop audio unavailable: {e}"))?;
                Ok(source(cfg.label(None), Target::DesktopExcept { exe }, vec![client]))
            }
            SourceKind::Desktop => {
                let client = endpoint(eRender, None).map_err(|e| format!("desktop audio unavailable: {e}"))?;
                Ok(source(cfg.label(None), Target::Endpoint, vec![client]))
            }
            SourceKind::Mic => {
                let id = (!cfg.device.is_empty()).then_some(cfg.device.as_str());
                let (client, name) = mic(id).map_err(|e| format!("microphone unavailable: {e}"))?;
                Ok(source(cfg.label(name.as_deref()), Target::Endpoint, vec![client]))
            }
            SourceKind::App => {
                let exe = app_name(&cfg.app);
                let (mut clients, mut failed, mut error) = (Vec::new(), Vec::new(), None);
                for pid in roots(&processes(), &exe) {
                    match process_loopback(pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE) {
                        Ok(c) => clients.push(c),
                        Err(e) => {
                            log(&format!("audio: {exe} (pid {pid}): {e}"));
                            failed.push(pid);
                            error = Some(e);
                        }
                    }
                }
                if let (true, Some(e)) = (clients.is_empty(), error) {
                    return Err(format!("{exe} unavailable: {e}"));
                }
                let mut s = source(exe.clone(), Target::App { exe }, clients);
                s.failed = failed;
                Ok(s)
            }
        }
    }
}

/// the mic uses the console role: opening the communications device makes windows duck other sounds.
unsafe fn mic(id: Option<&str>) -> Result<(Client, Option<String>), String> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ctx("MMDeviceEnumerator")?;
        let dev = match id {
            Some(id) => en.GetDevice(&HSTRING::from(id)).ctx("the chosen microphone isn't connected")?,
            None => en.GetDefaultAudioEndpoint(eCapture, eConsole).ctx("no default microphone")?,
        };
        let id = device_id(&dev);
        let label = microphones().0.into_iter().find(|m| Some(&m.id) == id.as_ref()).map(|m| m.label);
        Ok((endpoint(eCapture, Some(dev))?, label))
    }
}

unsafe fn endpoint(flow: EDataFlow, dev: Option<IMMDevice>) -> Result<Client, String> {
    unsafe {
        let dev = match dev {
            Some(d) => d,
            None => {
                let en: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ctx("MMDeviceEnumerator")?;
                en.GetDefaultAudioEndpoint(flow, eConsole).ctx("no default playback device")?
            }
        };
        let flags = if flow == eRender { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };
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
        Ok(Client { client, capture, format, placement, pid: None })
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Activated(mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _: Ref<IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

// process loopback has no mix format (GetMixFormat fails); it delivers the format Initialize asks for.
unsafe fn process_loopback(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Result<Client, String> {
    unsafe {
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: mode,
                },
            },
        };
        // the windows crate drops PROPVARIANT with PropVariantClear, which would CoTaskMemFree the stack blob.
        let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
        let v = &mut pv.Anonymous.Anonymous;
        v.vt = VT_BLOB;
        v.Anonymous.blob = BLOB {
            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: &params as *const _ as *mut u8,
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv), &handler)
            .ctx("ActivateAudioInterfaceAsync")?;
        rx.recv_timeout(ACTIVATE_TIMEOUT).map_err(|_| "process loopback activation timed out".to_string())?;
        let mut hr = windows::core::HRESULT(0);
        let mut iface = None;
        op.GetActivateResult(&mut hr, &mut iface).ctx("GetActivateResult")?;
        hr.ok().ctx("process loopback activation")?;
        let client: IAudioClient = iface.ok_or("process loopback activation returned nothing")?.cast().ctx("IAudioClient")?;
        let wf = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: 2,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, BUFFER, 0, &wf, None)
            .ctx("IAudioClient::Initialize (process loopback)")?;
        let capture: IAudioCaptureClient = client.GetService().ctx("IAudioClient::GetService")?;
        let format = Format::new(RATE, Sample::F32, 2, 0)?;
        let placement = Placement::new(RATE);
        Ok(Client { client, capture, format, placement, pid: Some(pid) })
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

impl Client {
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
                let at = (flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 == 0 && at != 0)
                    .then(|| hns_to_frames(at as i64 - origin));
                let gap = flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0;
                self.placement.push(at, scratch, gap, mix);
            }
            Ok(())
        }
    }
}

impl Source {
    unsafe fn read(&mut self, origin: i64, scratch: &mut Vec<f32>, issues: &mut Vec<String>) {
        if !self.live {
            return;
        }
        let mut i = 0;
        while i < self.clients.len() {
            let c = &mut self.clients[i];
            let Err(e) = (unsafe { c.read(origin, &mut self.mix, scratch) }) else {
                i += 1;
                continue;
            };
            log(&format!("audio: {} stopped: {} (0x{:08X})", self.name, e.message(), e.code().0 as u32));
            let gone = self.clients.remove(i);
            self.failed.extend(gone.pid);
            if !matches!(self.target, Target::App { .. }) {
                issues.push(format!("{} stopped: {}", self.name, e.message()));
                self.live = false;
                return;
            }
        }
    }

    unsafe fn rescan(&mut self, procs: &[Proc], origin: i64, scratch: &mut Vec<f32>) {
        if !self.live {
            return;
        }
        unsafe {
            match &self.target {
                Target::Endpoint => {}
                Target::App { exe } => {
                    for pid in roots(procs, exe) {
                        if self.clients.iter().any(|c| c.pid == Some(pid)) || self.failed.contains(&pid) {
                            continue;
                        }
                        match process_loopback(pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE) {
                            Ok(c) if c.client.Start().is_ok() => {
                                log(&format!("audio: {exe} started (pid {pid}), recording it"));
                                self.seen_app = true;
                                self.clients.push(c);
                            }
                            Ok(_) => {
                                log(&format!("audio: {exe} (pid {pid}) failed to start"));
                                self.failed.push(pid);
                            }
                            Err(e) => {
                                log(&format!("audio: {exe} (pid {pid}): {e}"));
                                self.failed.push(pid);
                            }
                        }
                    }
                }
                Target::DesktopExcept { exe } => {
                    let Some(&pid) = roots(procs, exe).first() else { return };
                    if self.clients.iter().any(|c| c.pid == Some(pid)) || self.failed.contains(&pid) {
                        return;
                    }
                    match process_loopback(pid, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE) {
                        Ok(c) if c.client.Start().is_ok() => {
                            log(&format!("audio: {exe} started (pid {pid}), leaving it out of desktop audio"));
                            for mut old in std::mem::take(&mut self.clients) {
                                let _ = old.read(origin, &mut self.mix, scratch);
                                let _ = old.client.Stop();
                            }
                            self.clients.push(c);
                        }
                        Ok(_) => {
                            log(&format!("audio: desktop without {exe} failed to start"));
                            self.failed.push(pid);
                        }
                        Err(e) => {
                            log(&format!("audio: desktop without {exe}: {e}"));
                            self.failed.push(pid);
                        }
                    }
                }
            }
        }
    }
}

pub(super) unsafe fn run(mut sources: Vec<Source>, tracks: &[Vec<usize>], stop: &AtomicBool, tx: &Sender<Msg>) -> Vec<String> {
    unsafe {
        let mut issues = Vec::new();
        for s in &mut sources {
            s.clients.retain(|c| match c.client.Start() {
                Ok(()) => true,
                Err(e) => {
                    issues.push(format!("{} failed to start: {}", s.name, e.message()));
                    false
                }
            });
        }
        let gains: Vec<f32> = sources.iter().map(|s| s.gain).collect();
        let watch = sources.iter().any(|s| !matches!(s.target, Target::Endpoint));
        let origin = hns(qpc());
        let mut sent = 0;
        let mut scratch = Vec::new();
        let mut scanned = Instant::now();
        loop {
            let stopping = stop.load(Ordering::SeqCst);
            for s in &mut sources {
                s.read(origin, &mut scratch, &mut issues);
            }
            if watch && !stopping && scanned.elapsed() >= RESCAN {
                scanned = Instant::now();
                let procs = processes();
                for s in &mut sources {
                    s.rescan(&procs, origin, &mut scratch);
                }
            }
            let behind = if stopping { 0 } else { LATENCY };
            let upto = hns_to_frames(hns(qpc()) - origin - behind);
            if upto > sent {
                let start = origin + frames_to_hns(sent);
                let bufs: Vec<Vec<f32>> = sources.iter_mut().map(|s| s.mix.take(upto)).collect();
                sent = upto;
                if tx.send(Msg::Audio { start, tracks: mix_tracks(&bufs, &gains, tracks) }).is_err() {
                    break;
                }
            }
            if stopping {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        for s in &sources {
            for c in &s.clients {
                let _ = c.client.Stop();
            }
            if !s.seen_app {
                issues.push(format!("{} wasn't running, its track is silent", s.name));
            }
        }
        let _ = tx.send(Msg::AudioEnd);
        issues
    }
}

unsafe fn property(dev: &IMMDevice, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let mut v = store.GetValue(key).ok()?;
        let inner = &v.Anonymous.Anonymous;
        let name = (inner.vt == VT_LPWSTR).then(|| inner.Anonymous.pwszVal.to_string().ok()).flatten();
        let _ = PropVariantClear(&mut v);
        name.filter(|n| !n.is_empty())
    }
}

unsafe fn device_id(dev: &IMMDevice) -> Option<String> {
    unsafe {
        let pid = dev.GetId().ok()?;
        let id = pid.to_string().ok();
        CoTaskMemFree(Some(pid.0 as _));
        id
    }
}

pub(crate) struct Microphone {
    pub id: String,
    pub name: String,
    pub label: String,
}

// the friendly name is "<endpoint> (<device>)", e.g. "Microphone (Shure MV7)".
pub(crate) fn microphones() -> (Vec<Microphone>, Option<String>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    let found = capture_devices();
    if com {
        unsafe { CoUninitialize() };
    }
    found
}

fn capture_devices() -> (Vec<Microphone>, Option<String>) {
    unsafe {
        let Ok(en) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else {
            return (Vec::new(), None);
        };
        let mut mics = Vec::new();
        if let Ok(all) = en.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
            for i in 0..all.GetCount().unwrap_or(0) {
                let Ok(dev) = all.Item(i) else { continue };
                let Some(id) = device_id(&dev) else { continue };
                let name = property(&dev, &PKEY_Device_FriendlyName).unwrap_or_else(|| id.clone());
                let label = property(&dev, &PKEY_DeviceInterface_FriendlyName).unwrap_or_else(|| name.clone());
                mics.push(Microphone { id, name, label });
            }
        }
        let shared: Vec<String> =
            mics.iter().filter(|m| mics.iter().filter(|o| o.label == m.label).count() > 1).map(|m| m.id.clone()).collect();
        for m in mics.iter_mut().filter(|m| shared.contains(&m.id)) {
            m.label = m.name.clone();
        }
        let default = en.GetDefaultAudioEndpoint(eCapture, eConsole).ok().and_then(|d| device_id(&d));
        (mics, default)
    }
}
