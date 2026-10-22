use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use super::{Event, Frames, Output, lock};
use crate::record::clicks::{self, Click, Log, Shape, Style};
use crate::record::keys::{self, KeyFrame};
use crate::record::timeline::{AudioPlacer, Pauses};

const IDLE: Duration = Duration::from_millis(50);

pub(super) enum Clicks {
    Off,
    /// Collected and returned in [`Stats::clicks`].
    Keep,
    Burn(Style),
}

pub(super) enum Keys {
    Off,
    /// Collected and returned in [`Stats::keys`].
    Keep,
    Burn(keys::Style),
}

pub(super) struct Control<'a> {
    pub abort: &'a AtomicBool,
    pub pauses: &'a Mutex<Pauses>,
    pub now: fn() -> i64,
    pub clicks: Clicks,
    pub keys: Keys,
}

#[derive(Debug, PartialEq)]
pub(super) struct Stats {
    pub frames: u64,
    pub dropped: u64,
    pub duration_s: f64,
    /// With [`Clicks::Keep`]: the clicks on the video timeline.
    pub clicks: Option<Vec<Click>>,
    /// With [`Keys::Keep`]: the keystrokes on the video timeline.
    pub keys: Option<Vec<keys::Key>>,
}

struct Video<T> {
    grid: f64,
    start: Option<i64>,
    /// The newest frame and its slot; it is handed to the encoder once a later slot begins.
    pending: Option<(T, i64)>,
    have_image: bool,
    frames: u64,
    dropped: u64,
    cursor: Option<(i32, i32)>,
    stale: bool,
    /// Clicks with active capture-clock times.
    clicks: Log,
    burn: Option<Style>,
    shapes: Vec<Shape>,
    /// Keystrokes with active capture-clock times.
    keys: keys::Log,
    kburn: Option<keys::Style>,
}

fn paint(burn: Option<&Style>, clicks: &Log, shapes: &mut Vec<Shape>, t: i64) {
    if let Some(style) = burn {
        clicks::shapes_at(clicks.clicks(), t, style, shapes);
    }
}

impl<T> Video<T> {
    fn slot_time(&self, slot: i64) -> i64 {
        (slot as f64 * self.grid).round() as i64
    }

    fn slot(&self, t: i64) -> i64 {
        self.start.map_or(0, |s| (((t - s) as f64 / self.grid).round() as i64).max(0))
    }

    /// Burnt-in clicks are animating at active time `t`.
    fn animating(&self, t: i64) -> bool {
        self.burn.as_ref().is_some_and(|s| clicks::moving(self.clicks.clicks(), t, s))
    }

    /// Burnt-in keystrokes are animating at active time `t`.
    fn animating_keys(&self, t: i64) -> bool {
        self.kburn.as_ref().is_some_and(|s| keys::moving(self.keys.keys(), t, s))
    }

    /// The keycap stack to burn into a frame stamped `t`, if keystrokes are burnt in.
    fn keyframe(&self, t: i64) -> Option<KeyFrame<'_>> {
        self.kburn.as_ref().map(|style| KeyFrame { keys: self.keys.keys(), t, style })
    }

    /// A frame at `t` would start a new slot.
    fn due(&self, t: i64) -> bool {
        self.pending.as_ref().is_none_or(|(_, ps)| self.slot(t) > *ps)
    }

    fn frame<F, O>(&mut self, frames: &mut F, output: &mut O, t: i64) -> Result<(), String>
    where
        F: Frames<Frame = T>,
        O: Output<Frame = T>,
    {
        let start = *self.start.get_or_insert(t);
        let slot = self.slot(t);
        match &self.pending {
            Some((f, ps)) if slot <= *ps => {
                let at = start + self.slot_time(*ps);
                paint(self.burn.as_ref(), &self.clicks, &mut self.shapes, at);
                frames.compose(f, self.cursor, &self.shapes, self.keyframe(at))?
            }
            _ => {
                let Some(next) = frames.alloc()? else {
                    self.dropped += 1;
                    return Ok(());
                };
                let at = start + self.slot_time(slot);
                paint(self.burn.as_ref(), &self.clicks, &mut self.shapes, at);
                frames.compose(&next, self.cursor, &self.shapes, self.keyframe(at))?;
                if let Some((prev, ps)) = self.pending.take() {
                    output.video(prev, self.slot_time(ps), self.slot_time(slot) - self.slot_time(ps))?;
                    self.frames += 1;
                }
                self.pending = Some((next, slot));
            }
        }
        Ok(())
    }
}

fn write_audio<O: Output>(output: &mut O, at: i64, placed: &[Vec<i16>], tracks: usize) -> Result<(), String> {
    for (track, pcm) in placed.iter().enumerate().take(tracks) {
        output.audio(track, at, pcm)?;
    }
    Ok(())
}

/// Consumes capture and audio events until both have ended, then finalizes the output
/// (skipped when `abort` is set: the file is going to be deleted).
pub(super) fn run<F, O>(
    frames: &mut F,
    output: &mut O,
    rx: &Receiver<Event<F::Image>>,
    free: &Sender<F::Image>,
    ctl: &Control,
    fps: f64,
    tracks: usize,
) -> Result<Stats, String>
where
    F: Frames,
    O: Output<Frame = F::Frame>,
{
    // Capture times jitter by a millisecond or two around vsync (which would show up as
    // judder), cursor-only updates arrive between vsyncs, and a capped fps means several
    // captures per slot: everything that lands in the same slot is merged into one
    // frame, the latest state wins.
    let mut v = Video {
        grid: 1e7 / fps,
        start: None,
        pending: None,
        have_image: false,
        frames: 0,
        dropped: 0,
        cursor: None,
        stale: false,
        clicks: Log::default(),
        burn: match &ctl.clicks {
            Clicks::Burn(style) => Some(*style),
            Clicks::Off | Clicks::Keep => None,
        },
        shapes: Vec::new(),
        keys: keys::Log::default(),
        kburn: match &ctl.keys {
            Keys::Burn(style) => Some(*style),
            Keys::Off | Keys::Keep => None,
        },
    };
    let tick = Duration::from_secs_f64(1.0 / fps).min(IDLE);
    let mut placer = AudioPlacer::default();
    let mut early_audio: Vec<(i64, Vec<Vec<i16>>)> = Vec::new();
    let mut audio_open = tracks > 0;
    let mut end = None;

    loop {
        let active = lock(ctl.pauses).active_clamped((ctl.now)());
        let animating = end.is_none() && (v.animating(active) || v.animating_keys(active));
        let event = match rx.recv_timeout(if animating { tick } else { IDLE }) {
            Ok(e) => Some(e),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match event {
            None => {}
            Some(Event::Pointer(shape)) => frames.set_pointer(&shape),
            Some(Event::End { at }) => {
                end = Some(at);
                if !audio_open {
                    break;
                }
            }
            Some(Event::AudioEnd) => {
                audio_open = false;
                if end.is_some() {
                    break;
                }
            }
            Some(Event::Audio { start, tracks: pcm }) => match v.start {
                Some(origin) => {
                    let at = placer.next;
                    let placed = placer.place(start, &pcm, &lock(ctl.pauses), origin);
                    write_audio(output, at, &placed, tracks)?;
                }
                None => early_audio.push((start, pcm)),
            },
            Some(Event::Click(p)) => {
                let pauses = lock(ctl.pauses);
                if !p.down {
                    v.clicks.release(p.button, pauses.active_clamped(p.at));
                } else if let Some(t) = pauses.active(p.at) {
                    v.clicks.press(p.button, t, p.x, p.y);
                }
            }
            Some(Event::Key(s)) => {
                let pauses = lock(ctl.pauses);
                if !s.down {
                    v.keys.up(s.vk, pauses.active_clamped(s.at));
                } else if let Some(t) = pauses.active(s.at) {
                    v.keys.down(s.vk, t, s.mods, s.main);
                }
            }
            Some(Event::Frame { image, at, cursor }) => {
                if let Some(image) = image {
                    frames.latch(&image)?;
                    let _ = free.send(image);
                    v.have_image = true;
                }
                v.cursor = cursor;
                if !v.have_image {
                    continue;
                }
                let t = lock(ctl.pauses).active(at);
                match t {
                    Some(t) => v.frame(frames, output, t)?,
                    None => v.stale = true,
                }
            }
        }
        if end.is_none() && (v.stale || v.burn.is_some() || v.kburn.is_some()) {
            let t = lock(ctl.pauses).active((ctl.now)());
            if let Some(t) = t
                && (v.stale || (v.have_image && (v.animating(t) || v.animating_keys(t)) && v.due(t)))
            {
                v.stale = false;
                v.frame(frames, output, t)?;
            }
        }
        if let Some(origin) = v.start
            && !early_audio.is_empty()
        {
            for (start, pcm) in std::mem::take(&mut early_audio) {
                let at = placer.next;
                let placed = placer.place(start, &pcm, &lock(ctl.pauses), origin);
                write_audio(output, at, &placed, tracks)?;
            }
        }
    }

    if ctl.abort.load(Ordering::SeqCst) {
        return Ok(Stats { frames: v.frames, dropped: v.dropped, duration_s: 0.0, clicks: None, keys: None });
    }
    let mut duration_s = 0.0;
    let mut video_end = 0;
    if let Some((prev, ps)) = v.pending.take() {
        let end = lock(ctl.pauses).active_clamped(end.unwrap_or_else(ctl.now));
        let end_slot = v.slot(end).max(ps + 1);
        output.video(prev, v.slot_time(ps), v.slot_time(end_slot) - v.slot_time(ps))?;
        v.frames += 1;
        video_end = v.slot_time(end_slot);
        duration_s = video_end as f64 / 1e7;
        let at = placer.next;
        let pad = vec![0; 2 * placer.pad(video_end)];
        write_audio(output, at, &vec![pad; tracks], tracks)?;
    }
    if v.frames == 0 {
        return Err("no frames were captured".into());
    }
    output.finish()?;
    let clicks = matches!(ctl.clicks, Clicks::Keep).then(|| {
        let mut clicks = std::mem::take(&mut v.clicks).finish(v.start.unwrap_or(0));
        clicks.retain(|c| c.down < video_end);
        clicks
    });
    let keys = matches!(ctl.keys, Keys::Keep).then(|| {
        let mut keys = std::mem::take(&mut v.keys).finish(v.start.unwrap_or(0));
        keys.retain(|k| k.at < video_end);
        keys
    });
    Ok(Stats { frames: v.frames, dropped: v.dropped, duration_s, clicks, keys })
}
