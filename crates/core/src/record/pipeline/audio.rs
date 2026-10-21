use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::{AudioInput, Event};
use crate::config::audio::AudioSource;
use crate::log::log;
use crate::record::mix::{HNS, frames_to_hns, hns_to_frames, mix_tracks};
use crate::record::tracks::{Track, plan};

const LATENCY: i64 = HNS / 10;
const RESCAN: Duration = Duration::from_secs(1);

pub struct OpenedAudio<A> {
    pub sources: Vec<A>,
    pub tracks: Vec<Track>,
    pub issues: Vec<String>,
}

pub fn open_audio<A: AudioInput>(config: &[AudioSource], track_names: &[String]) -> OpenedAudio<A> {
    let (mut sources, mut opened, mut issues) = (Vec::new(), Vec::new(), Vec::new());
    for c in config.iter().filter(|c| c.enabled && c.records()) {
        match A::open(c) {
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
    let names: Vec<String> = sources.iter().map(|s| s.name().to_owned()).collect();
    OpenedAudio { tracks: plan(&names, &opened, track_names), sources, issues }
}

pub(super) fn run<A: AudioInput, I>(
    mut sources: Vec<A>,
    tracks: &[Vec<usize>],
    stop: &AtomicBool,
    tx: &Sender<Event<I>>,
    now: fn() -> i64,
) -> Vec<String> {
    let mut issues = Vec::new();
    for s in &mut sources {
        s.start(&mut issues);
    }
    let gains: Vec<f32> = sources.iter().map(|s| s.gain()).collect();
    let watch = sources.iter().any(|s| s.watches());
    let origin = now();
    let mut sent = 0;
    let mut scanned = Instant::now();
    loop {
        let stopping = stop.load(Ordering::SeqCst);
        for s in &mut sources {
            s.read(origin, &mut issues);
        }
        if watch && !stopping && scanned.elapsed() >= RESCAN {
            scanned = Instant::now();
            A::rescan(&mut sources, origin);
        }
        let behind = if stopping { 0 } else { LATENCY };
        let upto = hns_to_frames(now() - origin - behind);
        if upto > sent {
            let start = origin + frames_to_hns(sent);
            let bufs: Vec<Vec<f32>> = sources.iter_mut().map(|s| s.take(upto)).collect();
            sent = upto;
            if tx.send(Event::Audio { start, tracks: mix_tracks(&bufs, &gains, tracks) }).is_err() {
                break;
            }
        }
        if stopping {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for s in &mut sources {
        s.finish(&mut issues);
    }
    let _ = tx.send(Event::AudioEnd);
    issues
}
