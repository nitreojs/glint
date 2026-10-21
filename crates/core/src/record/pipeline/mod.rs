//! Screen recording pipeline, independent of the platform that captures and encodes.
//!
//! * Capture thread: [`Capture::run`] sends region images (GPU textures or CPU buffers the
//!   backend owns, handed back on the `free` channel once the encoder thread has latched
//!   them), pointer shapes and positions, stamped with the capture clock.
//! * Encoder thread: snaps timestamps to the frame grid, merges what lands in one slot,
//!   cuts paused time and hands frames and placed audio to the [`Output`].
//! * Audio thread (when enabled): [`AudioInput`]s placed on the capture clock and mixed into
//!   up to six 48 kHz stereo tracks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

use super::Codec;
use super::pointer::PointerShape;
use super::timeline::Pauses;
use crate::config::audio::AudioSource;
use crate::log::log;

mod audio;
mod encoded;
mod video;

#[cfg(test)]
mod tests;

pub use audio::{OpenedAudio, open_audio};
pub use encoded::Encoded;

pub enum Event<I> {
    /// A new state of the region: `image` holds a fresh image (None when only the cursor
    /// moved), `at` the capture time, `cursor` the pointer's desktop position if visible.
    Frame { image: Option<I>, at: i64, cursor: Option<(i32, i32)> },
    Pointer(PointerShape),
    End { at: i64 },
    /// `start`: capture time of the first frame in 100 ns units.
    Audio { start: i64, tracks: Vec<Vec<i16>> },
    AudioEnd,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureStats {
    pub dropped: u64,
    pub coalesced: u64,
}

pub trait Capture: Send {
    type Image: Send + 'static;

    fn run(&mut self, stop: &AtomicBool, out: &Sender<Event<Self::Image>>, free: &Receiver<Self::Image>)
    -> Result<CaptureStats, String>;
}

pub trait Frames {
    type Image;
    type Frame;

    fn latch(&mut self, image: &Self::Image) -> Result<(), String>;
    fn set_pointer(&mut self, shape: &PointerShape);
    fn alloc(&mut self) -> Result<Option<Self::Frame>, String>;
    fn compose(&mut self, frame: &Self::Frame, cursor: Option<(i32, i32)>) -> Result<(), String>;
}

pub trait Output {
    type Frame;

    fn video(&mut self, frame: Self::Frame, time: i64, duration: i64) -> Result<(), String>;
    fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoConfig {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Constant quantizer: lower = better quality and bigger files.
    pub qp: u32,
    /// Encoder quality-vs-speed knob, 0 (fastest) ..= 100 (best).
    pub effort: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stream {
    Video { config: VideoConfig, headers: Vec<u8> },
    Audio { name: String, headers: Vec<u8> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub data: Vec<u8>,
    pub pts: i64,
    pub dts: i64,
    pub duration: i64,
    pub key: bool,
}

pub trait VideoEncoder {
    type Frame;

    fn configure(&mut self, config: &VideoConfig) -> Result<Stream, String>;
    fn encode(&mut self, frame: Self::Frame, time: i64, duration: i64, out: &mut Vec<Packet>) -> Result<(), String>;
    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), String>;
}

pub trait AudioEncoder {
    fn configure(&mut self, name: &str) -> Result<Stream, String>;
    fn encode(&mut self, at: i64, pcm: &[i16], out: &mut Vec<Packet>) -> Result<(), String>;
    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), String>;
}

pub trait PacketSink {
    fn add_stream(&mut self, stream: Stream) -> Result<usize, String>;
    fn write(&mut self, stream: usize, packet: Packet) -> Result<(), String>;
    fn finalize(&mut self) -> Result<(), String>;
}

pub trait AudioInput: Send + Sized {
    fn open(config: &AudioSource) -> Result<Self, String>;
    fn name(&self) -> &str;
    fn gain(&self) -> f32;
    fn watches(&self) -> bool;
    fn start(&mut self, issues: &mut Vec<String>);
    fn read(&mut self, origin: i64, issues: &mut Vec<String>);
    fn rescan(sources: &mut [Self], origin: i64);
    fn take(&mut self, upto: i64) -> Vec<f32>;
    fn finish(&mut self, issues: &mut Vec<String>);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Encode,
    Capture,
    Audio,
}

pub trait Backend: Send + Sized + 'static {
    type Capture: Capture;
    type Frames: Frames<Image = <Self::Capture as Capture>::Image>;
    type Output: Output<Frame = <Self::Frames as Frames>::Frame>;
    type Audio: AudioInput;
    type Guard;

    fn enter(role: Role) -> Self::Guard;
    fn now() -> i64;
    fn open(self) -> Result<Parts<Self>, String>;
}

pub struct Parts<B: Backend> {
    pub capture: B::Capture,
    pub frames: B::Frames,
    pub output: B::Output,
    pub audio: OpenedAudio<B::Audio>,
    pub fps: f64,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub path: PathBuf,
    /// Frames lost because the encoder could not keep up.
    pub dropped: u64,
    pub duration_s: f64,
    /// Capture ended on its own (e.g. resolution change); the file is still valid.
    pub stopped_early: Option<String>,
    pub tracks: Vec<String>,
    pub audio_issues: Vec<String>,
}

#[derive(Debug)]
pub enum Outcome {
    Saved(Summary),
    /// [`Recorder::abort`] was called; the file has been deleted.
    Discarded,
}

pub struct Recorder {
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    pauses: Arc<Mutex<Pauses>>,
    now: fn() -> i64,
}

/// A recorder whose pipeline is initializing; see [`Starting::wait`].
pub struct Starting {
    ready: mpsc::Receiver<Result<(), String>>,
    go: mpsc::SyncSender<()>,
    recorder: Recorder,
}

impl Starting {
    /// Waits for initialization, then starts capturing. Call it once anything that must not
    /// appear in the video (the region picker) is gone.
    pub fn wait(self) -> Result<Recorder, String> {
        match self.ready.recv() {
            Ok(Ok(())) => {
                let _ = self.go.send(());
                Ok(self.recorder)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err("encoder thread exited during startup".into()),
        }
    }
}

/// Initializes the capture/encode pipeline in the background. `on_done` runs on the encoder
/// thread once the recording ends (on failure the partial file is deleted).
pub fn start<B: Backend>(
    backend: B,
    on_done: impl FnOnce(Result<Outcome, String>) + Send + 'static,
) -> Result<Starting, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let abort = Arc::new(AtomicBool::new(false));
    let pauses = Arc::new(Mutex::new(Pauses::default()));
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let (go_tx, go_rx) = mpsc::sync_channel::<()>(1);
    let (st, ab, ps) = (stop.clone(), abort.clone(), pauses.clone());
    thread::Builder::new()
        .name("glint-encode".into())
        .spawn(move || {
            let _thread = B::enter(Role::Encode);
            match backend.open() {
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
                Ok(parts) => {
                    let _ = ready_tx.send(Ok(()));
                    let path = parts.path.clone();
                    let res = if go_rx.recv().is_ok() {
                        run_session(parts, &st, &ab, &ps)
                    } else {
                        drop(parts);
                        Err("recording was cancelled before it started".into())
                    };
                    let discarded = ab.load(Ordering::SeqCst);
                    if res.is_err() || discarded {
                        let _ = std::fs::remove_file(&path);
                    }
                    on_done(if discarded { Ok(Outcome::Discarded) } else { res.map(Outcome::Saved) });
                }
            }
        })
        .map_err(|e| format!("failed to spawn encoder thread: {e}"))?;
    Ok(Starting { ready: ready_rx, go: go_tx, recorder: Recorder { stop, abort, pauses, now: B::now } })
}

impl Recorder {
    /// Requests a graceful stop; `on_done` fires once the file is finalized.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Stops without saving: the file is not finalized and gets deleted.
    pub fn abort(&self) {
        self.abort.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn pause(&self) {
        lock(&self.pauses).pause((self.now)());
    }

    pub fn resume(&self) {
        lock(&self.pauses).resume((self.now)());
    }

    pub fn paused(&self) -> bool {
        lock(&self.pauses).paused()
    }
}

fn lock(m: &Mutex<Pauses>) -> MutexGuard<'_, Pauses> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn run_session<B: Backend>(
    parts: Parts<B>,
    stop: &Arc<AtomicBool>,
    abort: &AtomicBool,
    pauses: &Mutex<Pauses>,
) -> Result<Summary, String> {
    let Parts { capture, mut frames, mut output, audio, fps, path } = parts;
    let OpenedAudio { sources, tracks, issues: mut audio_issues } = audio;
    let names: Vec<String> = tracks.iter().map(|t| t.name.clone()).collect();
    let routes: Vec<Vec<usize>> = tracks.into_iter().map(|t| t.sources).collect();
    let (tx, rx) = mpsc::channel::<Event<<B::Capture as Capture>::Image>>();
    let (free_tx, free_rx) = mpsc::channel();
    let mixer = if sources.is_empty() {
        None
    } else {
        let (st, tx) = (stop.clone(), tx.clone());
        let spawned = thread::Builder::new().name("glint-audio".into()).spawn(move || {
            let _thread = B::enter(Role::Audio);
            audio::run(sources, &routes, &st, &tx, B::now)
        });
        Some(spawned.map_err(|e| format!("failed to spawn audio thread: {e}"))?)
    };
    let st = stop.clone();
    let capture = thread::Builder::new()
        .name("glint-capture".into())
        .spawn(move || {
            let _thread = B::enter(Role::Capture);
            let mut cap = capture;
            let r = cap.run(&st, &tx, &free_rx);
            st.store(true, Ordering::SeqCst);
            let _ = tx.send(Event::End { at: B::now() });
            r
        })
        .map_err(|e| {
            stop.store(true, Ordering::SeqCst);
            format!("failed to spawn capture thread: {e}")
        })?;
    let control = video::Control { abort, pauses, now: B::now };
    let enc_res = video::run(&mut frames, &mut output, &rx, &free_tx, &control, fps, names.len());
    if enc_res.is_err() {
        stop.store(true, Ordering::SeqCst);
    }
    drop(rx);
    let cap_res = capture.join().unwrap_or_else(|_| Err("capture thread panicked".into()));
    if let Some(m) = mixer {
        audio_issues.extend(m.join().unwrap_or_else(|_| vec!["audio thread panicked".into()]));
    }
    let video::Stats { frames: written, dropped: enc_dropped, duration_s } = enc_res?;
    let (cap_dropped, coalesced, stopped_early) = match cap_res {
        Ok(s) => (s.dropped, s.coalesced, None),
        Err(e) => (0, 0, Some(e)),
    };
    let dropped = enc_dropped + cap_dropped;
    log(&format!(
        "finished: {written} frames, {dropped} dropped, {coalesced} coalesced by the capture, {duration_s:.2}s{}",
        stopped_early.as_deref().map(|e| format!(", stopped early: {e}")).unwrap_or_default()
    ));
    Ok(Summary { path, dropped, duration_s, stopped_early, tracks: names, audio_issues })
}
