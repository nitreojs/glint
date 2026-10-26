//! The replay buffer: the last stretch of a recording kept as encoded packets, and the clips cut
//! from it. Cutting never re-encodes: a clip starts on a video keyframe and its timestamps are
//! moved to start at zero, ready for any [`PacketSink`] (an MP4 file).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use super::pipeline::{Packet, PacketSink, Stream};

/// What the buffer keeps: `keep` (100 ns units) of the newest video, from a keyframe, and never
/// more than `max_bytes` of packets (then it keeps less time).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub keep: i64,
    pub max_bytes: usize,
}

/// Packets of every stream in arrival order, the oldest dropped as newer ones come in.
pub struct Replay {
    limits: Limits,
    streams: Vec<Stream>,
    /// The video stream's index, once added.
    video: Option<usize>,
    /// Shared with the clips cut from it, so cutting under the lock copies no packet data.
    packets: VecDeque<(usize, Arc<Packet>)>,
    /// Presentation times of the video keyframes in `packets`, oldest first.
    keys: VecDeque<i64>,
    bytes: usize,
    /// End of the newest video packet.
    end: Option<i64>,
    /// The recording that filled the buffer has ended.
    finished: bool,
    /// The byte limit, not the window, dropped the oldest video last: it holds less than `keep`.
    capped: bool,
    /// Where the recording clock sits on the wall clock.
    clock: Clock,
}

/// The wall-clock time of the recording clock's zero, from when video packets arrive: a packet
/// can't arrive before the frame after it was captured, so the earliest arrival seen relative to
/// its end (encoding latency only) places the clock best. Only the last [`Clock::WINDOW`] of
/// arrivals counts, so a wall clock set back or forward is followed.
#[derive(Clone, Debug, Default)]
pub struct Clock {
    /// (video time, unix time of video time zero by that arrival), increasing in both: a
    /// sliding-window minimum.
    samples: VecDeque<(i64, i64)>,
}

impl Clock {
    /// How far back arrivals count (100 ns units).
    pub const WINDOW: i64 = 10 * 10_000_000;

    /// A video packet ending at `end` (recording clock) arrived at `arrived` (unix time, 100 ns
    /// units).
    pub fn arrived(&mut self, end: i64, arrived: i64) {
        let zero = arrived - end;
        while self.samples.back().is_some_and(|&(_, z)| z >= zero) {
            self.samples.pop_back();
        }
        self.samples.push_back((end, zero));
        while self.samples.front().is_some_and(|&(t, _)| t < end - Self::WINDOW) {
            self.samples.pop_front();
        }
    }

    /// The unix time (100 ns units) of recording time zero; `None` before any video arrived.
    pub fn zero(&self) -> Option<i64> {
        self.samples.front().map(|&(_, z)| z)
    }
}

/// Now as unix time in 100 ns units.
pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| (d.as_nanos() / 100) as i64)
}

/// A stretch of the buffer in presentation order. Its packets keep the recording's timestamps;
/// [`Clip::write`] moves them to start at zero.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub streams: Vec<Stream>,
    pub packets: Vec<(usize, Arc<Packet>)>,
    /// The keyframe it starts on, on the recording's clock (100 ns units).
    pub start: i64,
    /// How long it plays (100 ns units).
    pub duration: i64,
    /// It's shorter than asked for because the byte limit dropped older video.
    pub capped: bool,
    /// The unix time (ms) of its first frame, from the buffer's [`Clock`]; `None` when unknown.
    pub wall: Option<i64>,
}

impl Replay {
    pub fn new(limits: Limits) -> Replay {
        Replay {
            limits,
            streams: Vec::new(),
            video: None,
            packets: VecDeque::new(),
            keys: VecDeque::new(),
            bytes: 0,
            end: None,
            finished: false,
            capped: false,
            clock: Clock::default(),
        }
    }

    pub fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
        self.trim();
    }

    /// How much video the buffer holds now (100 ns units), from its oldest keyframe.
    pub fn held(&self) -> i64 {
        match (self.keys.front(), self.end) {
            (Some(&first), Some(end)) => end - first,
            _ => 0,
        }
    }

    /// Bytes of packet data held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    fn add_stream(&mut self, stream: Stream) -> usize {
        if matches!(stream, Stream::Video { .. }) && self.video.is_none() {
            self.video = Some(self.streams.len());
        }
        self.streams.push(stream);
        self.streams.len() - 1
    }

    fn push(&mut self, stream: usize, packet: Packet, arrived: i64) {
        if Some(stream) == self.video {
            self.clock.arrived(packet.pts + packet.duration, arrived);
            if packet.key {
                self.keys.push_back(packet.pts);
            }
            let end = packet.pts + packet.duration;
            self.end = Some(self.end.map_or(end, |e| e.max(end)));
        }
        self.bytes += packet.data.len();
        self.packets.push_back((stream, Arc::new(packet)));
        self.trim();
    }

    /// Drops what's older than the newest keyframe that still leaves `keep` behind it; over the
    /// byte limit, drops whole keyframe intervals until it fits (always keeping the newest one).
    fn trim(&mut self) {
        let Some(end) = self.end else { return };
        while let Some(&next) = self.keys.get(1) {
            let old = end - next >= self.limits.keep;
            if !old && self.bytes <= self.limits.max_bytes {
                break;
            }
            self.capped = !old;
            self.keys.pop_front();
            self.drop_before(next);
        }
    }

    /// Drops packets from the front while they start before `cut`.
    fn drop_before(&mut self, cut: i64) {
        while self.packets.front().is_some_and(|(_, p)| p.pts < cut) {
            let (_, p) = self.packets.pop_front().expect("checked above");
            self.bytes -= p.data.len();
        }
    }

    /// The last `len` (100 ns units) of video and sound: from the newest keyframe at or before
    /// `len` from the end (from the oldest one when the buffer holds less).
    pub fn last(&self, len: i64) -> Option<Clip> {
        let end = self.end?;
        let want = end - len;
        let start = self.keys.iter().rev().find(|&&k| k <= want).or(self.keys.front()).copied()?;
        let mut packets: Vec<(usize, Arc<Packet>)> =
            self.packets.iter().filter(|(_, p)| p.pts >= start && p.pts < end).cloned().collect();
        packets.sort_by_key(|(_, p)| p.pts);
        let capped = self.capped && end - start < len;
        let wall = self.clock.zero().map(|z| (z + start) / 10_000);
        Some(Clip { streams: self.streams.clone(), packets, start, duration: end - start, capped, wall })
    }
}

impl Clip {
    /// Writes the clip into `sink` (an MP4 file), timestamps from zero, and finishes it.
    pub fn write(self, sink: &mut impl PacketSink) -> Result<(), String> {
        let ids = self.streams.into_iter().map(|s| sink.add_stream(s)).collect::<Result<Vec<_>, String>>()?;
        for (stream, p) in self.packets {
            let packet = Packet { pts: p.pts - self.start, dts: p.dts - self.start, ..(*p).clone() };
            sink.write(ids[stream], packet)?;
        }
        sink.finalize()
    }
}

/// The buffer as the recording pipeline's sink: the encoder thread fills it while anyone holding
/// a clone cuts clips from it.
#[derive(Clone)]
pub struct ReplaySink(pub Arc<Mutex<Replay>>);

impl ReplaySink {
    pub fn new(limits: Limits) -> ReplaySink {
        ReplaySink(Arc::new(Mutex::new(Replay::new(limits))))
    }

    pub fn lock(&self) -> MutexGuard<'_, Replay> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl PacketSink for ReplaySink {
    fn add_stream(&mut self, stream: Stream) -> Result<usize, String> {
        Ok(self.lock().add_stream(stream))
    }

    fn write(&mut self, stream: usize, packet: Packet) -> Result<(), String> {
        let arrived = unix_now();
        self.lock().push(stream, packet, arrived);
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), String> {
        self.lock().finished = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Codec;
    use crate::record::pipeline::VideoConfig;

    const S: i64 = 10_000_000;
    const FRAME: i64 = S / 10;

    fn video() -> Stream {
        let config = VideoConfig { codec: Codec::H264, width: 64, height: 64, fps: 10.0, qp: 20, effort: 50, bitrate: None, keyframe_secs: 1.0 };
        Stream::Video { config, headers: vec![0x67], matrix: crate::record::negotiate::Matrix::Bt709 }
    }

    fn audio() -> Stream {
        Stream::Audio { name: "desktop".into(), headers: vec![0x12, 0x10] }
    }

    /// `secs` of 10 fps video, a keyframe every second, 100 bytes a frame, with an audio packet of 10
    /// bytes every 1/20 s.
    fn filled(limits: Limits, secs: i64) -> ReplaySink {
        let mut sink = ReplaySink::new(limits);
        sink.add_stream(video()).unwrap();
        sink.add_stream(audio()).unwrap();
        for i in 0..secs * 10 {
            let t = i * FRAME;
            sink.write(0, Packet { data: vec![0; 100], pts: t, dts: t, duration: FRAME, key: i % 10 == 0 }).unwrap();
            for a in [t, t + FRAME / 2] {
                sink.write(1, Packet { data: vec![0; 10], pts: a, dts: a, duration: FRAME / 2, key: true }).unwrap();
            }
        }
        sink
    }

    #[test]
    fn it_keeps_the_window_from_a_keyframe_and_no_more() {
        let sink = filled(Limits { keep: 3 * S, max_bytes: usize::MAX }, 10);
        let r = sink.lock();
        assert_eq!(r.held(), 3 * S, "from the keyframe at 7 s to the end at 10 s");
        assert_eq!(r.keys, [7 * S, 8 * S, 9 * S]);
        assert!(r.packets.iter().all(|(_, p)| p.pts >= 7 * S));
        assert_eq!(r.bytes(), 30 * 100 + 60 * 10);

        let sink = filled(Limits { keep: 3 * S + FRAME, max_bytes: usize::MAX }, 10);
        assert_eq!(sink.lock().held(), 4 * S, "a window between keyframes keeps the one before");
    }

    #[test]
    fn over_the_byte_limit_it_keeps_less_time() {
        let sink = filled(Limits { keep: 60 * S, max_bytes: 2_500 }, 10);
        let r = sink.lock();
        assert_eq!(r.held(), 2 * S);
        assert!(r.bytes() <= 2_500);
        assert!(r.last(60 * S).unwrap().capped, "a minute asked for, 2 s held");
        assert!(!r.last(S).unwrap().capped, "what's asked for fits");
        let tiny = filled(Limits { keep: 60 * S, max_bytes: 1 }, 5);
        assert_eq!(tiny.lock().held(), S, "the newest keyframe interval always stays");
    }

    #[test]
    fn a_buffer_short_of_its_window_only_counts_as_capped_when_bytes_cut_it() {
        let young = filled(Limits { keep: 60 * S, max_bytes: usize::MAX }, 5);
        assert!(!young.lock().last(60 * S).unwrap().capped, "still filling: nothing was dropped");
        let sink = filled(Limits { keep: 60 * S, max_bytes: 2_500 }, 10);
        sink.lock().set_limits(Limits { keep: S, max_bytes: usize::MAX });
        assert!(!sink.lock().last(S).unwrap().capped, "the window drops the old video now");
    }

    #[test]
    fn clips_start_on_the_keyframe_before_the_moment_asked_for() {
        let sink = filled(Limits { keep: 30 * S, max_bytes: usize::MAX }, 10);
        let clip = sink.lock().last(2 * S + FRAME).unwrap();
        assert_eq!(clip.duration, 3 * S, "from the keyframe at 7 s, before 7.9 s");
        assert_eq!(clip.streams, [video(), audio()]);
        assert_eq!(clip.start, 7 * S);
        let video: Vec<&Packet> = clip.packets.iter().filter(|(s, _)| *s == 0).map(|(_, p)| &**p).collect();
        assert_eq!(video.len(), 30);
        assert!(video[0].key && video[0].pts == clip.start, "it opens on the keyframe");
        assert_eq!(clip.packets.iter().filter(|(s, _)| *s == 1).count(), 60, "the sound from the same moment");
        assert!(clip.packets.windows(2).all(|w| w[0].1.pts <= w[1].1.pts));

        let whole = sink.lock().last(60 * S).unwrap();
        assert_eq!(whole.duration, 10 * S, "asking for more than it holds gives everything");
        assert!(ReplaySink::new(Limits { keep: S, max_bytes: 1 }).lock().last(S).is_none(), "nothing yet");
    }

    #[test]
    fn the_clock_takes_the_quickest_arrival_and_forgets_old_ones() {
        let mut c = Clock::default();
        assert_eq!(c.zero(), None);
        let epoch = 1_800_000_000 * S;
        // packets ending at 1 s, 2 s, 3 s arriving 50 ms, 20 ms and 300 ms late
        c.arrived(S, epoch + S + S / 20);
        c.arrived(2 * S, epoch + 2 * S + S / 50);
        c.arrived(3 * S, epoch + 3 * S + 3 * S / 10);
        assert_eq!(c.zero(), Some(epoch + S / 50), "the 20 ms one");
        // the wall clock was set 5 s forward: once the quick arrival is out of the window, it's followed
        c.arrived(14 * S, epoch + 5 * S + 14 * S + S / 10);
        assert_eq!(c.zero(), Some(epoch + 5 * S + S / 10));
        c.arrived(15 * S, epoch - 5 * S + 15 * S + S / 10);
        assert_eq!(c.zero(), Some(epoch - 5 * S + S / 10), "and set back, at once");

        let mut r = Replay::new(Limits { keep: 30 * S, max_bytes: usize::MAX });
        r.add_stream(video());
        r.add_stream(audio());
        for i in 0..30 {
            let t = i * FRAME;
            let packet = Packet { data: vec![0; 10], pts: t, dts: t, duration: FRAME, key: i % 10 == 0 };
            // audio arriving early must not move the clock
            r.push(1, Packet { key: true, ..packet.clone() }, epoch);
            r.push(0, packet, epoch + t + FRAME + S / 100);
        }
        let clip = r.last(S + FRAME).unwrap();
        assert_eq!(clip.start, S);
        assert_eq!(clip.wall, Some((epoch + S / 100 + S) / 10_000), "the first frame, 10 ms of latency included");
    }

    #[derive(Default)]
    struct Written {
        streams: Vec<Stream>,
        packets: Vec<(usize, i64, i64)>,
        done: bool,
    }

    impl PacketSink for Written {
        fn add_stream(&mut self, stream: Stream) -> Result<usize, String> {
            self.streams.push(stream);
            Ok(self.streams.len() - 1)
        }

        fn write(&mut self, stream: usize, packet: Packet) -> Result<(), String> {
            self.packets.push((stream, packet.pts, packet.dts));
            Ok(())
        }

        fn finalize(&mut self) -> Result<(), String> {
            self.done = true;
            Ok(())
        }
    }

    #[test]
    fn a_clip_writes_into_any_sink_from_zero() {
        let clip = filled(Limits { keep: 30 * S, max_bytes: usize::MAX }, 3).lock().last(S).unwrap();
        assert_eq!(clip.start, 2 * S);
        let mut out = Written::default();
        clip.clone().write(&mut out).unwrap();
        assert_eq!(out.streams, clip.streams);
        assert_eq!(out.packets.len(), clip.packets.len());
        assert_eq!(out.packets[0], (0, 0, 0), "the opening keyframe at zero");
        let last = clip.packets.last().unwrap();
        assert_eq!(out.packets.last().unwrap().1, last.1.pts - 2 * S);
        assert!(out.done);
    }
}
