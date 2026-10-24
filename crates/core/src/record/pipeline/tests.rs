use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};

use super::video::{self, Clicks, Control, Stats};
use super::*;
use crate::config::audio::SourceKind;
use crate::record::clicks::{Button, Click};
use crate::record::keys::{Key, KeyFrame, KeyLabel, Mods, Named, Stroke};
use crate::record::pointer::PointerKind;

const MS: i64 = 10_000;

type Content = (Option<usize>, Option<(i32, i32)>);

#[derive(Default)]
struct Log {
    latched: Vec<usize>,
    pointers: Vec<(i32, i32)>,
    contents: Vec<Content>,
    drawn: Vec<Vec<Shape>>,
    video: Vec<(u32, i64, i64)>,
    audio: Vec<(usize, i64, usize)>,
    finished: bool,
}

type Shared = Arc<Mutex<Log>>;

fn get(log: &Shared) -> MutexGuard<'_, Log> {
    log.lock().unwrap()
}

struct FakeFrames {
    log: Shared,
    image: Option<usize>,
    allocs: u32,
    fail: Vec<u32>,
}

impl FakeFrames {
    fn new(log: &Shared) -> FakeFrames {
        FakeFrames { log: log.clone(), image: None, allocs: 0, fail: Vec::new() }
    }
}

impl Frames for FakeFrames {
    type Image = usize;
    type Frame = u32;

    fn latch(&mut self, image: &usize) -> Result<(), String> {
        self.image = Some(*image);
        get(&self.log).latched.push(*image);
        Ok(())
    }

    fn set_pointer(&mut self, shape: &PointerShape) {
        get(&self.log).pointers.push(shape.size());
    }

    fn alloc(&mut self) -> Result<Option<u32>, String> {
        self.allocs += 1;
        if self.fail.contains(&self.allocs) {
            return Ok(None);
        }
        let mut log = get(&self.log);
        log.contents.push((None, None));
        log.drawn.push(Vec::new());
        Ok(Some(log.contents.len() as u32 - 1))
    }

    fn compose(&mut self, frame: &u32, cursor: Option<(i32, i32)>, clicks: &[Shape], _keys: Option<KeyFrame>) -> Result<(), String> {
        let mut log = get(&self.log);
        log.contents[*frame as usize] = (self.image, cursor);
        log.drawn[*frame as usize] = clicks.to_vec();
        Ok(())
    }
}

struct FakeOutput {
    log: Shared,
}

impl Output for FakeOutput {
    type Frame = u32;

    fn video(&mut self, frame: u32, time: i64, duration: i64) -> Result<(), String> {
        get(&self.log).video.push((frame, time, duration));
        Ok(())
    }

    fn audio(&mut self, track: usize, at: i64, pcm: &[i16]) -> Result<(), String> {
        get(&self.log).audio.push((track, at, pcm.len() / 2));
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        get(&self.log).finished = true;
        Ok(())
    }
}

fn frame(image: Option<usize>, at: i64, cursor: Option<(i32, i32)>) -> Event<usize> {
    Event::Frame { image, at, cursor }
}

fn mid_pause() -> i64 {
    200 * MS
}

fn after_resume() -> i64 {
    250 * MS
}

struct Run {
    stats: Result<Stats, String>,
    freed: Vec<usize>,
}

fn encode<O: Output<Frame = u32>>(
    frames: &mut FakeFrames,
    output: &mut O,
    events: Vec<Event<usize>>,
    pauses: Pauses,
    abort: bool,
    now: fn() -> i64,
    tracks: usize,
) -> Run {
    let (tx, rx) = mpsc::channel();
    for e in events {
        tx.send(e).unwrap();
    }
    drop(tx);
    let (free_tx, free_rx) = mpsc::channel();
    let abort = AtomicBool::new(abort);
    let pauses = Mutex::new(pauses);
    let ctl = Control { abort: &abort, pauses: &pauses, now, clicks: Clicks::Off, keys: video::Keys::Off };
    let stats = video::run(frames, output, &rx, &free_tx, &ctl, 10.0, tracks);
    Run { stats, freed: free_rx.try_iter().collect() }
}

fn run_video(events: Vec<Event<usize>>, pauses: Pauses, now: fn() -> i64, fail: Vec<u32>) -> (Run, Shared) {
    let log = Shared::default();
    let mut frames = FakeFrames { fail, ..FakeFrames::new(&log) };
    let run = encode(&mut frames, &mut FakeOutput { log: log.clone() }, events, pauses, false, now, 0);
    (run, log)
}

fn stats(frames: u64, dropped: u64, duration_s: f64) -> Result<Stats, String> {
    Ok(Stats { frames, dropped, duration_s, clicks: None, keys: None })
}

#[test]
fn frames_snap_to_the_grid_and_merge_per_slot() {
    let events = vec![
        Event::Pointer(PointerShape { kind: PointerKind::Monochrome, width: 32, height: 64, pitch: 4, data: vec![0; 256] }),
        frame(Some(0), 0, Some((1, 1))),
        frame(Some(1), 30 * MS, Some((2, 2))),
        frame(Some(0), 100 * MS, None),
        frame(None, 310 * MS, Some((5, 5))),
        Event::End { at: 400 * MS },
    ];
    let (run, log) = run_video(events, Pauses::default(), mid_pause, Vec::new());
    assert_eq!(run.stats, stats(3, 0, 0.4));
    assert_eq!(run.freed, vec![0, 1, 0]);
    let log = get(&log);
    assert_eq!(log.pointers, vec![(32, 32)]);
    assert_eq!(log.latched, vec![0, 1, 0]);
    assert_eq!(log.video, vec![(0, 0, 100 * MS), (1, 100 * MS, 200 * MS), (2, 300 * MS, 100 * MS)]);
    assert_eq!(log.contents, vec![(Some(1), Some((2, 2))), (Some(0), None), (Some(0), Some((5, 5)))]);
    assert!(log.finished);
}

#[test]
fn cursor_moves_before_the_first_image_are_ignored() {
    let events = vec![frame(None, 0, Some((1, 1))), frame(Some(3), 50 * MS, None), Event::End { at: 60 * MS }];
    let (run, log) = run_video(events, Pauses::default(), mid_pause, Vec::new());
    assert_eq!(run.stats, stats(1, 0, 0.1));
    assert_eq!(get(&log).video, vec![(0, 0, 100 * MS)]);
}

#[test]
fn an_empty_frame_pool_drops_the_frame() {
    let events = vec![
        frame(Some(0), 0, None),
        frame(Some(1), 100 * MS, None),
        frame(Some(0), 200 * MS, None),
        Event::End { at: 300 * MS },
    ];
    let (run, log) = run_video(events, Pauses::default(), mid_pause, vec![2]);
    assert_eq!(run.stats, stats(2, 1, 0.3));
    assert_eq!(run.freed, vec![0, 1, 0]);
    assert_eq!(get(&log).video, vec![(0, 0, 200 * MS), (1, 200 * MS, 100 * MS)]);
}

#[test]
fn paused_time_is_cut() {
    let mut pauses = Pauses::default();
    pauses.pause(100 * MS);
    pauses.resume(300 * MS);
    let events = vec![
        frame(Some(0), 0, None),
        frame(Some(1), 150 * MS, None),
        frame(Some(0), 340 * MS, None),
        Event::End { at: 400 * MS },
    ];
    let (run, log) = run_video(events, pauses, mid_pause, Vec::new());
    assert_eq!(run.stats, stats(2, 0, 0.2));
    assert_eq!(get(&log).video, vec![(0, 0, 100 * MS), (1, 100 * MS, 100 * MS)]);
}

#[test]
fn a_frame_captured_while_paused_shows_after_resume() {
    let mut pauses = Pauses::default();
    pauses.pause(100 * MS);
    pauses.resume(200 * MS);
    let events = vec![frame(Some(0), 0, None), frame(Some(1), 150 * MS, None), Event::End { at: 300 * MS }];
    let (run, log) = run_video(events, pauses, after_resume, Vec::new());
    assert_eq!(run.stats, stats(2, 0, 0.3));
    let log = get(&log);
    assert_eq!(log.video, vec![(0, 0, 200 * MS), (1, 200 * MS, 100 * MS)]);
    assert_eq!(log.contents[1], (Some(1), None));
}

#[test]
fn audio_waits_for_the_first_frame_and_pads_to_the_end() {
    let tracks = |frames: usize| vec![vec![1i16; 2 * frames], vec![2i16; 2 * frames]];
    let events = vec![
        Event::Audio { start: 0, tracks: tracks(480) },
        frame(Some(0), 0, None),
        Event::Audio { start: 10 * MS, tracks: tracks(480) },
        Event::End { at: 100 * MS },
        Event::AudioEnd,
    ];
    let log = Shared::default();
    let mut frames = FakeFrames::new(&log);
    let mut output = FakeOutput { log: log.clone() };
    let run = encode(&mut frames, &mut output, events, Pauses::default(), false, mid_pause, 2);
    assert_eq!(run.stats, stats(1, 0, 0.1));
    let log = get(&log);
    assert_eq!(log.audio, vec![(0, 0, 480), (1, 0, 480), (0, 480, 480), (1, 480, 480), (0, 960, 3840), (1, 960, 3840)]);
}

#[test]
fn abort_skips_the_last_frame_and_finalizing() {
    let log = Shared::default();
    let mut frames = FakeFrames::new(&log);
    let mut output = FakeOutput { log: log.clone() };
    let events = vec![frame(Some(0), 0, None), Event::End { at: 100 * MS }];
    let run = encode(&mut frames, &mut output, events, Pauses::default(), true, mid_pause, 0);
    assert_eq!(run.stats, stats(0, 0, 0.0));
    let log = get(&log);
    assert!(log.video.is_empty() && !log.finished);
}

#[test]
fn no_frames_is_an_error() {
    let (run, log) = run_video(vec![Event::End { at: 0 }], Pauses::default(), mid_pause, Vec::new());
    assert_eq!(run.stats, Err("no frames were captured".into()));
    assert!(!get(&log).finished);
}

fn press(button: Button, down: bool, at: i64) -> Event<usize> {
    Event::Click(Press { button, down, at, x: 1.5, y: 2.5 })
}

fn run_clicks(events: Vec<Event<usize>>, pauses: Pauses, now: fn() -> i64, clicks: Clicks) -> (Run, Shared) {
    let log = Shared::default();
    let (tx, rx) = mpsc::channel();
    for e in events {
        tx.send(e).unwrap();
    }
    drop(tx);
    let (free_tx, free_rx) = mpsc::channel();
    let (abort, pauses) = (AtomicBool::new(false), Mutex::new(pauses));
    let ctl = Control { abort: &abort, pauses: &pauses, now, clicks, keys: video::Keys::Off };
    let stats = video::run(&mut FakeFrames::new(&log), &mut FakeOutput { log: log.clone() }, &rx, &free_tx, &ctl, 10.0, 0);
    (Run { stats, freed: free_rx.try_iter().collect() }, log)
}

#[test]
fn clicks_land_on_the_video_timeline_without_the_paused_time() {
    let mut pauses = Pauses::default();
    pauses.pause(300 * MS);
    pauses.resume(500 * MS);
    let events = vec![
        press(Button::Left, true, 50 * MS),
        press(Button::Left, false, 60 * MS),
        frame(Some(0), 100 * MS, None),
        press(Button::Left, true, 150 * MS),
        press(Button::Left, false, 200 * MS),
        press(Button::Right, true, 280 * MS),
        press(Button::Middle, true, 350 * MS),
        press(Button::Middle, false, 360 * MS),
        press(Button::Right, false, 400 * MS),
        frame(Some(1), 600 * MS, None),
        press(Button::Left, true, 650 * MS),
        press(Button::Right, true, 720 * MS),
        Event::End { at: 700 * MS },
    ];
    let (run, log) = run_clicks(events, pauses, mid_pause, Clicks::Keep);
    let click = |button, down, up: Option<i64>| Click { button, down: down * MS, up: up.map(|u| u * MS), x: 1.5, y: 2.5 };
    let clicks = vec![click(Button::Left, 50, Some(100)), click(Button::Right, 180, Some(200)), click(Button::Left, 350, None)];
    assert_eq!(run.stats, Ok(Stats { frames: 2, dropped: 0, duration_s: 0.4, clicks: Some(clicks), keys: None }));
    assert!(get(&log).drawn.iter().all(Vec::is_empty));
}

#[test]
fn burnt_in_clicks_are_drawn_at_each_frames_time() {
    let style = Style::new([255, 0, 0], 1.0);
    let events = vec![
        frame(Some(0), 0, None),
        frame(Some(1), 100 * MS, None),
        press(Button::Left, true, 120 * MS),
        press(Button::Left, false, 150 * MS),
        frame(Some(0), 200 * MS, None),
        frame(Some(1), 300 * MS, None),
        Event::End { at: 400 * MS },
    ];
    let (run, log) = run_clicks(events, Pauses::default(), || 0, Clicks::Burn(style));
    assert_eq!(run.stats, stats(4, 0, 0.4));
    let log = get(&log);
    assert_eq!(log.drawn.iter().map(Vec::len).collect::<Vec<_>>(), [0, 0, 2, 1]);
    assert_eq!(log.drawn[2], clicks::state_at(&[Click { button: Button::Left, down: 0, up: Some(30 * MS), x: 1.5, y: 2.5 }], 80 * MS, &style));
    let ring = log.drawn[3][0];
    assert!((ring.radius - 28.0 * (1.0 - (1.0 - 180.0f32 / 450.0).powi(3))).abs() < 1e-3, "{ring:?}");
}

#[test]
fn burnt_in_ripples_get_frames_on_a_still_screen() {
    let events = vec![
        frame(Some(0), 0, None),
        frame(Some(1), 100 * MS, None),
        press(Button::Right, true, 150 * MS),
        Event::End { at: 400 * MS },
    ];
    let (run, log) = run_clicks(events, Pauses::default(), || 260 * MS, Clicks::Burn(Style::new([255, 0, 0], 1.0)));
    assert_eq!(run.stats, stats(3, 0, 0.4));
    let log = get(&log);
    assert_eq!(log.latched, [0, 1]);
    assert_eq!(log.video, [(0, 0, 100 * MS), (1, 100 * MS, 200 * MS), (2, 300 * MS, 100 * MS)]);
    assert_eq!(log.contents[2], (Some(1), None));
    assert_eq!(log.drawn[2].len(), 2);
}


fn kstroke(vk: u16, down: bool, at: i64, main: KeyLabel) -> Event<usize> {
    Event::Key(Stroke { vk, down, at, mods: Mods::default(), main })
}

fn run_keys(events: Vec<Event<usize>>, pauses: Pauses, now: fn() -> i64, keys: video::Keys) -> (Run, Shared) {
    let log = Shared::default();
    let (tx, rx) = mpsc::channel();
    for e in events {
        tx.send(e).unwrap();
    }
    drop(tx);
    let (free_tx, free_rx) = mpsc::channel();
    let (abort, pauses) = (AtomicBool::new(false), Mutex::new(pauses));
    let ctl = Control { abort: &abort, pauses: &pauses, now, clicks: Clicks::Off, keys };
    let stats = video::run(&mut FakeFrames::new(&log), &mut FakeOutput { log: log.clone() }, &rx, &free_tx, &ctl, 10.0, 0);
    (Run { stats, freed: free_rx.try_iter().collect() }, log)
}

#[test]
fn keys_land_on_the_video_timeline_without_the_paused_time() {
    let mut pauses = Pauses::default();
    pauses.pause(300 * MS);
    pauses.resume(500 * MS);
    let txt = |s: &str| KeyLabel::Text(s.into());
    let events = vec![
        kstroke(0x41, true, 50 * MS, txt("a")), // before the first frame, dropped
        kstroke(0x41, false, 60 * MS, txt("a")),
        frame(Some(0), 100 * MS, None),
        kstroke(0x42, true, 150 * MS, txt("b")),
        kstroke(0x42, true, 170 * MS, txt("b")), // the OS repeating the held key, collapsed
        kstroke(0x42, false, 200 * MS, txt("b")),
        kstroke(0x1B, true, 350 * MS, KeyLabel::Named(Named::Esc)), // pressed while paused, dropped
        frame(Some(1), 600 * MS, None),
        kstroke(0x43, true, 650 * MS, txt("c")),
        Event::End { at: 700 * MS },
    ];
    let (run, log) = run_keys(events, pauses, mid_pause, video::Keys::Keep);
    let key = |at: i64, hold: Option<i64>, s: &str| Key { at, hold, mods: Mods::default(), main: KeyLabel::Text(s.into()) };
    let keys = vec![key(50 * MS, Some(100 * MS), "b"), key(350 * MS, None, "c")];
    assert_eq!(run.stats, Ok(Stats { frames: 2, dropped: 0, duration_s: 0.4, clicks: None, keys: Some(keys) }));
    assert!(get(&log).drawn.iter().all(Vec::is_empty), "nothing is burnt in when the keystrokes are kept");
}
struct FakeAudio {
    name: String,
    gain: f32,
    base: i64,
    started: bool,
}

impl AudioInput for FakeAudio {
    fn open(config: &AudioSource) -> Result<Self, String> {
        if config.kind == SourceKind::App {
            return Err(format!("{} unavailable", config.app));
        }
        Ok(FakeAudio { name: config.label(None), gain: config.volume as f32 / 100.0, base: 0, started: false })
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn gain(&self) -> f32 {
        self.gain
    }

    fn watches(&self) -> bool {
        false
    }

    fn start(&mut self, _: &mut Vec<String>) {
        self.started = true;
    }

    fn read(&mut self, _: i64, _: &mut Vec<String>) {}

    fn rescan(_: &mut [Self], _: i64) {}

    fn take(&mut self, upto: i64) -> Vec<f32> {
        assert!(self.started);
        let n = (upto - self.base).max(0) as usize;
        self.base = self.base.max(upto);
        vec![1.0; 2 * n]
    }

    fn finish(&mut self, issues: &mut Vec<String>) {
        issues.push(format!("{} finished", self.name));
    }
}

static STEP: AtomicI64 = AtomicI64::new(0);

fn stepping() -> i64 {
    STEP.fetch_add(10 * MS, Ordering::SeqCst)
}

#[test]
fn audio_is_mixed_into_tracks_up_to_the_clock() {
    let mut mic = FakeAudio::open(&AudioSource::new(SourceKind::Mic, 0)).unwrap();
    mic.gain = 0.5;
    let desktop = FakeAudio::open(&AudioSource::new(SourceKind::Desktop, 0)).unwrap();
    let (tx, rx) = mpsc::channel::<Event<usize>>();
    let stop = AtomicBool::new(true);
    let issues = audio::run(vec![mic, desktop], &[vec![0], vec![0, 1]], &stop, &tx, stepping);
    assert_eq!(issues, vec!["mic finished".to_string(), "desktop finished".to_string()]);
    let events: Vec<Event<usize>> = rx.try_iter().collect();
    let [Event::Audio { start: 0, tracks }, Event::AudioEnd] = events.as_slice() else { panic!("unexpected events") };
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0], vec![16384; 960]);
    assert_eq!(tracks[1], vec![32767; 960]);
}

#[test]
fn audio_sources_open_into_planned_tracks() {
    let mut app = AudioSource::new(SourceKind::App, 2);
    app.app = "x.exe".into();
    let mut off = AudioSource::new(SourceKind::Mic, 3);
    off.enabled = false;
    let config = [AudioSource::new(SourceKind::Mic, 0), AudioSource::new(SourceKind::Desktop, 1), app, off];
    let opened = open_audio::<FakeAudio>(&config, &[]);
    let names: Vec<&str> = opened.sources.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["mic", "desktop"]);
    let tracks: Vec<(&str, &[usize])> = opened.tracks.iter().map(|t| (t.name.as_str(), t.sources.as_slice())).collect();
    assert_eq!(tracks, [("mix", &[0, 1][..]), ("mic", &[0][..]), ("desktop", &[1][..])]);
    assert_eq!(opened.issues, ["x.exe unavailable"]);
}

struct FakeCapture {
    frames: i64,
    images: Vec<usize>,
    mouse_done: Option<Arc<AtomicBool>>,
}

impl Capture for FakeCapture {
    type Image = usize;

    fn run(&mut self, _: &AtomicBool, out: &Sender<Event<usize>>, free: &Receiver<usize>) -> Result<CaptureStats, String> {
        out.send(Event::Pointer(PointerShape { kind: PointerKind::Color, width: 2, height: 3, pitch: 8, data: vec![0; 24] }))
            .unwrap();
        for k in 0..self.frames {
            let image = match self.images.pop() {
                Some(i) => i,
                None => free.recv().map_err(|e| e.to_string())?,
            };
            out.send(frame(Some(image), k * 100 * MS, Some((k as i32, 0)))).unwrap();
        }
        while self.mouse_done.as_ref().is_some_and(|d| !d.load(Ordering::SeqCst)) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(CaptureStats { dropped: 1, coalesced: 2 })
    }
}

struct FakeMouse {
    done: Arc<AtomicBool>,
}

impl Mouse for FakeMouse {
    fn run(&mut self, stop: &AtomicBool, listening: &dyn Fn() -> bool, out: &mut dyn FnMut(Press)) {
        if listening() {
            out(Press { button: Button::Left, down: true, at: 150 * MS, x: 4.5, y: 5.5 });
            out(Press { button: Button::Left, down: false, at: 250 * MS, x: 0.0, y: 0.0 });
            out(Press { button: Button::Right, down: true, at: 380 * MS, x: 6.5, y: 7.5 });
        }
        self.done.store(true, Ordering::SeqCst);
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

struct FakeKeys;

impl Keys for FakeKeys {
    fn run(&mut self, stop: &AtomicBool, _listening: &dyn Fn() -> bool, _out: &mut dyn FnMut(Stroke)) {
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
}

struct Fake {
    log: Shared,
    path: PathBuf,
    fail: bool,
    mouse: bool,
}

impl Backend for Fake {
    type Capture = FakeCapture;
    type Frames = FakeFrames;
    type Output = FakeOutput;
    type Audio = FakeAudio;
    type Mouse = FakeMouse;
    type Keys = FakeKeys;
    type Guard = ();

    fn enter(_: Role) {}

    fn now() -> i64 {
        0
    }

    fn open(self) -> Result<Parts<Self>, String> {
        if self.fail {
            return Err("no encoder".into());
        }
        let mut app = AudioSource::new(SourceKind::App, 2);
        app.app = "x.exe".into();
        let config = [AudioSource::new(SourceKind::Mic, 0), AudioSource::new(SourceKind::Desktop, 1), app];
        let done = Arc::new(AtomicBool::new(false));
        let style = Style::new([255, 0, 0], 1.25);
        let clicks = ClickCapture { mouse: FakeMouse { done: done.clone() }, style, scale: 1.25, burn_in: false };
        Ok(Parts {
            capture: FakeCapture { frames: 5, images: vec![0, 1], mouse_done: self.mouse.then_some(done) },
            frames: FakeFrames::new(&self.log),
            output: FakeOutput { log: self.log },
            audio: open_audio(&config, &[]),
            fps: 10.0,
            path: self.path,
            clicks: self.mouse.then_some(clicks),
            keys: None,
        })
    }
}

struct Recording {
    started: Result<Recorder, String>,
    done: Receiver<Result<Outcome, String>>,
    log: Shared,
    path: PathBuf,
}

fn record(name: &str, fail: bool) -> Recording {
    record_with(name, fail, false, b"partial")
}

fn record_with(name: &str, fail: bool, mouse: bool, file: &[u8]) -> Recording {
    let path = std::env::temp_dir().join(format!("glint-pipeline-{name}-{}.mp4", std::process::id()));
    std::fs::write(&path, file).unwrap();
    let log = Shared::default();
    let (tx, done) = mpsc::channel();
    let backend = Fake { log: log.clone(), path: path.clone(), fail, mouse };
    let started = start(backend, move |res| tx.send(res).unwrap()).and_then(Starting::wait);
    Recording { started, done, log, path }
}

#[test]
fn clicks_from_the_mouse_thread_are_saved_in_the_file() {
    let atom = |kind: &[u8], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
    let mp4 = [atom(b"ftyp", b"isom"), atom(b"mdat", &[7; 64]), atom(b"moov", &atom(b"mvhd", &[0; 100]))].concat();
    let Recording { started, done, path, .. } = record_with("clicks", false, true, &mp4);
    let _rec = started.unwrap();
    let Ok(Outcome::Saved(s)) = done.recv().unwrap() else { panic!("not saved") };
    let saved = clicks::load(&path);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(s.duration_s, 0.5);
    let clicks = vec![
        Click { button: Button::Left, down: 150 * MS, up: Some(250 * MS), x: 4.5, y: 5.5 },
        Click { button: Button::Right, down: 380 * MS, up: None, x: 6.5, y: 7.5 },
    ];
    assert_eq!(saved, Ok(Some(Payload { version: 1, scale: 1.25, clicks })));
}

#[test]
fn a_recording_runs_through_every_thread() {
    let Recording { started, done, log, path } = record("saved", false);
    let rec = started.unwrap();
    let Ok(Outcome::Saved(s)) = done.recv().unwrap() else { panic!("not saved") };
    rec.pause();
    assert!(rec.paused());
    rec.resume();
    assert!(!rec.paused());
    assert_eq!(s.path, path);
    assert_eq!((s.dropped, s.duration_s, s.stopped_early.as_deref()), (1, 0.5, None));
    assert_eq!(s.tracks, ["mix", "mic", "desktop"]);
    assert_eq!(s.audio_issues, ["x.exe unavailable", "mic finished", "desktop finished"]);
    let log = get(&log);
    assert_eq!(log.pointers, vec![(2, 3)]);
    assert_eq!(log.latched, vec![1, 0, 1, 0, 1]);
    assert_eq!(log.video.iter().map(|v| (v.1, v.2)).collect::<Vec<_>>(), (0..5).map(|k| (k * 100 * MS, 100 * MS)).collect::<Vec<_>>());
    assert_eq!(log.audio, vec![(0, 0, 24_000), (1, 0, 24_000), (2, 0, 24_000)]);
    assert!(log.finished && path.exists());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn an_aborted_recording_deletes_the_file() {
    let Recording { started, done, log, path } = record("aborted", false);
    started.unwrap().abort();
    assert!(matches!(done.recv().unwrap(), Ok(Outcome::Discarded)));
    assert!(!path.exists() && !get(&log).finished);
}

#[test]
fn a_failed_setup_is_reported_by_wait() {
    let Recording { started, path, .. } = record("failed", true);
    assert_eq!(started.err().as_deref(), Some("no encoder"));
    std::fs::remove_file(&path).unwrap();
}

struct FakeEncoder {
    frames: u32,
}

impl VideoEncoder for FakeEncoder {
    type Frame = u32;

    fn configure(&mut self, config: &VideoConfig) -> Result<Stream, String> {
        Ok(Stream::Video { config: *config, headers: vec![0x67] })
    }

    fn encode(&mut self, frame: u32, time: i64, duration: i64, out: &mut Vec<Packet>) -> Result<(), String> {
        let key = self.frames.is_multiple_of(2);
        self.frames += 1;
        out.push(Packet { data: vec![frame as u8], pts: time, dts: time, duration, key });
        Ok(())
    }

    fn finish(&mut self, _: &mut Vec<Packet>) -> Result<(), String> {
        Ok(())
    }
}

struct FakeAac {
    tail: bool,
}

impl AudioEncoder for FakeAac {
    fn configure(&mut self, name: &str) -> Result<Stream, String> {
        Ok(Stream::Audio { name: name.into(), headers: vec![0x12, 0x10] })
    }

    fn encode(&mut self, at: i64, pcm: &[i16], out: &mut Vec<Packet>) -> Result<(), String> {
        let pts = crate::record::mix::frames_to_hns(at);
        out.push(Packet { data: vec![pcm.len() as u8], pts, dts: pts, duration: 0, key: true });
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Packet>) -> Result<(), String> {
        if self.tail {
            out.push(Packet { data: vec![], pts: -1, dts: -1, duration: 0, key: true });
        }
        Ok(())
    }
}

#[derive(Default)]
struct Ring {
    streams: Vec<Stream>,
    packets: Vec<(usize, i64, bool)>,
    finalized: bool,
}

impl PacketSink for Arc<Mutex<Ring>> {
    fn add_stream(&mut self, stream: Stream) -> Result<usize, String> {
        let mut ring = self.lock().unwrap();
        ring.streams.push(stream);
        Ok(ring.streams.len() - 1)
    }

    fn write(&mut self, stream: usize, packet: Packet) -> Result<(), String> {
        self.lock().unwrap().packets.push((stream, packet.pts, packet.key));
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), String> {
        self.lock().unwrap().finalized = true;
        Ok(())
    }
}

#[test]
fn encoded_packets_reach_a_sink_between_encoder_and_muxer() {
    let config = VideoConfig { codec: Codec::Hevc, width: 640, height: 360, fps: 10.0, qp: 20, effort: 50, bitrate: None, keyframe_secs: 2.0 };
    let ring = Arc::new(Mutex::new(Ring::default()));
    let audio = vec![("mic".to_string(), FakeAac { tail: false }), ("desktop".to_string(), FakeAac { tail: true })];
    let mut output = Encoded::new(FakeEncoder { frames: 0 }, &config, audio, ring.clone()).unwrap();
    let log = Shared::default();
    let mut frames = FakeFrames::new(&log);
    let events = vec![
        frame(Some(0), 0, None),
        Event::Audio { start: 0, tracks: vec![vec![0; 960], vec![0; 960]] },
        frame(Some(1), 100 * MS, None),
        Event::End { at: 200 * MS },
        Event::AudioEnd,
    ];
    let run = encode(&mut frames, &mut output, events, Pauses::default(), false, mid_pause, 2);
    assert_eq!(run.stats, stats(2, 0, 0.2));
    let ring = ring.lock().unwrap();
    assert_eq!(
        ring.streams,
        [
            Stream::Video { config, headers: vec![0x67] },
            Stream::Audio { name: "mic".into(), headers: vec![0x12, 0x10] },
            Stream::Audio { name: "desktop".into(), headers: vec![0x12, 0x10] },
        ]
    );
    assert_eq!(
        ring.packets,
        [
            (1, 0, true),
            (2, 0, true),
            (0, 0, true),
            (0, 100 * MS, false),
            (1, 10 * MS, true),
            (2, 10 * MS, true),
            (2, -1, true),
        ]
    );
    assert!(ring.finalized);
}
