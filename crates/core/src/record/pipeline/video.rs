use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use super::{Event, Frames, Output, lock};
use crate::record::timeline::{AudioPlacer, Pauses};

pub(super) struct Control<'a> {
    pub abort: &'a AtomicBool,
    pub pauses: &'a Mutex<Pauses>,
    pub now: fn() -> i64,
}

#[derive(Debug, PartialEq)]
pub(super) struct Stats {
    pub frames: u64,
    pub dropped: u64,
    pub duration_s: f64,
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
}

impl<T> Video<T> {
    fn slot_time(&self, slot: i64) -> i64 {
        (slot as f64 * self.grid).round() as i64
    }

    fn slot(&self, t: i64) -> i64 {
        self.start.map_or(0, |s| (((t - s) as f64 / self.grid).round() as i64).max(0))
    }

    fn frame<F, O>(&mut self, frames: &mut F, output: &mut O, t: i64) -> Result<(), String>
    where
        F: Frames<Frame = T>,
        O: Output<Frame = T>,
    {
        self.start.get_or_insert(t);
        let slot = self.slot(t);
        match &self.pending {
            Some((f, ps)) if slot <= *ps => frames.compose(f, self.cursor)?,
            _ => {
                let Some(next) = frames.alloc()? else {
                    self.dropped += 1;
                    return Ok(());
                };
                frames.compose(&next, self.cursor)?;
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
    };
    let mut placer = AudioPlacer::default();
    let mut early_audio: Vec<(i64, Vec<Vec<i16>>)> = Vec::new();
    let mut audio_open = tracks > 0;
    let mut end = None;

    loop {
        let event = match rx.recv_timeout(Duration::from_millis(50)) {
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
        if v.stale && end.is_none() {
            let t = lock(ctl.pauses).active((ctl.now)());
            if let Some(t) = t {
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
        return Ok(Stats { frames: v.frames, dropped: v.dropped, duration_s: 0.0 });
    }
    let mut duration_s = 0.0;
    if let Some((prev, ps)) = v.pending.take() {
        let end = lock(ctl.pauses).active_clamped(end.unwrap_or_else(ctl.now));
        let end_slot = v.slot(end).max(ps + 1);
        output.video(prev, v.slot_time(ps), v.slot_time(end_slot) - v.slot_time(ps))?;
        v.frames += 1;
        duration_s = v.slot_time(end_slot) as f64 / 1e7;
        let at = placer.next;
        let pad = vec![0; 2 * placer.pad(v.slot_time(end_slot))];
        write_audio(output, at, &vec![pad; tracks], tracks)?;
    }
    if v.frames == 0 {
        return Err("no frames were captured".into());
    }
    output.finish()?;
    Ok(Stats { frames: v.frames, dropped: v.dropped, duration_s })
}
