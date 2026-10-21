use serde::{Deserialize, Serialize};

use super::crop::Crop;
use super::edit::Edit;
use super::index::Index;
use super::info::Info;
use super::math::{Range, SECOND, gif_estimate, gif_frames};
use crate::record::Codec;

pub const TARGETS_MB: [u32; 3] = [10, 25, 50];
pub(super) const HEIGHTS: [u32; 3] = [720, 540, 360];
pub(super) const VIDEO_FPS: [u32; 3] = [60, 30, 24];
/// A custom bitrate, in kbit/s: 0.2 to 200 Mbps.
pub(super) const KBPS: std::ops::RangeInclusive<u32> = 200..=200_000;
/// Decimal megabytes, so a "≤ 10 MB" file also fits limits counted in MiB.
pub const MB: u64 = 1_000_000;
pub const AAC_BYTES_PER_SECOND: u64 = 24_000;
/// Aim this far under a size target.
const UNDER: f64 = 0.96;
const MUXING: f64 = 1.004;
const MUXING_FIXED: u64 = 8 * 1024;
const MIN_BPS: f64 = 150_000.0;
const MIN_GIF: u32 = 64;
/// hevc keeps the look of h.264 at about this share of its bitrate.
const HEVC_SHARE: f64 = 0.7;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Video,
    Gif,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodecChoice {
    #[default]
    Same,
    H264,
    Hevc,
}

impl CodecChoice {
    pub(super) fn resolve(self, source: Option<Codec>) -> Codec {
        match self {
            CodecChoice::Same => source.unwrap_or(Codec::H264),
            CodecChoice::H264 => Codec::H264,
            CodecChoice::Hevc => Codec::Hevc,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub format: Format,
    /// A size limit; never set together with `kbps`.
    pub target_mb: Option<u32>,
    /// Output height after the crop; None keeps it.
    pub height: Option<u32>,
    pub gif_fps: u32,
    /// Video frame rate; None keeps the source's.
    pub fps: Option<u32>,
    /// A custom video bitrate; never set together with `target_mb`.
    pub kbps: Option<u32>,
    pub codec: CodecChoice,
    pub combine: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            format: Format::Video,
            target_mb: None,
            height: None,
            gif_fps: 15,
            fps: None,
            kbps: None,
            codec: CodecChoice::Same,
            combine: false,
        }
    }
}

impl Settings {
    /// A size limit takes over from a custom bitrate.
    pub fn set_target(&mut self, mb: Option<u32>) {
        self.target_mb = mb;
        if mb.is_some() {
            self.kbps = None;
        }
    }

    /// A custom bitrate takes over from a size limit; out-of-range values fall back to auto.
    pub fn set_kbps(&mut self, kbps: Option<u32>) {
        self.kbps = kbps.filter(|k| KBPS.contains(k));
        if self.kbps.is_some() {
            self.target_mb = None;
        }
    }
}

/// `"2.5"` or `"2,5"` Mbps as kbit/s, when it's a usable bitrate.
pub fn parse_mbps(text: &str) -> Option<u32> {
    let mbps: f64 = text.trim().replace(',', ".").parse().ok()?;
    let kbps = (mbps * 1000.0).round();
    (mbps.is_finite() && kbps >= *KBPS.start() as f64 && kbps <= *KBPS.end() as f64).then_some(kbps as u32)
}

/// The frame rates offered below the source's.
pub fn frame_rates(source: f64) -> Vec<u32> {
    VIDEO_FPS.into_iter().filter(|&f| (f as f64) < source - 0.01).collect()
}

/// What the recording offers the planner.
pub struct Source<'a> {
    pub info: &'a Info,
    pub index: Option<&'a Index>,
    pub bytes: u64,
}

impl Source<'_> {
    fn secs(&self) -> f64 {
        (self.info.duration.max(1) as f64 / SECOND as f64).max(0.001)
    }

    fn video_bps(&self) -> f64 {
        let bytes = match self.index {
            Some(i) => i.video.total(),
            None => self.bytes.saturating_sub(AAC_BYTES_PER_SECOND * self.info.tracks.len() as u64 * self.secs() as u64),
        };
        bytes as f64 * 8.0 / self.secs()
    }

    fn audio_bytes(&self, track: usize, from: i64, to: i64) -> u64 {
        match self.index.and_then(|i| i.audio.get(track)) {
            Some(s) => s.bytes(from, to),
            None => AAC_BYTES_PER_SECOND * (to - from).max(0) as u64 / SECOND as u64,
        }
    }

    pub(super) fn keys(&self) -> &[i64] {
        self.index.map_or(&[], |i| &i.video.keys)
    }
}

/// One kept range; the output starts at `lead`, the keyframe a copied piece has to begin on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub lead: i64,
    pub start: i64,
    pub end: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Video {
    Copy,
    /// `fps` None keeps the source's frame timing; otherwise frames land on that grid.
    Encode { crop: Option<Crop>, size: (u32, u32), bitrate: u32, fps: Option<u32>, codec: Codec },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoJob {
    pub pieces: Vec<Piece>,
    /// Indices into `Info::tracks`.
    pub tracks: Vec<usize>,
    pub combine: bool,
    pub video: Video,
    pub target: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifJob {
    pub kept: Vec<Range>,
    pub crop: Option<Crop>,
    pub size: (u32, u32),
    pub fps: u32,
    pub target: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    Video(VideoJob),
    Gif(GifJob),
}

impl Job {
    /// Length of the output.
    pub fn len(&self) -> i64 {
        match self {
            Job::Video(v) => v.pieces.iter().map(|p| p.end - p.lead).sum(),
            Job::Gif(g) => g.kept.iter().map(|r| r.len()).sum(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl VideoJob {
    /// The output would be the recording itself.
    pub fn whole(&self, info: &Info) -> bool {
        self.video == Video::Copy
            && !self.combine
            && self.tracks.len() == info.tracks.len()
            && matches!(self.pieces[..], [p] if p.lead <= 0 && p.end >= info.duration)
    }
}

/// The audio lanes that aren't muted, which are the tracks the file gets.
pub(super) fn tracks(muted: &[bool]) -> Vec<usize> {
    (0..muted.len()).filter(|&k| !muted[k]).collect()
}

/// The last keyframe at or before `t`, where a copied piece has to start.
pub(super) fn lead(keys: &[i64], t: i64) -> i64 {
    keys[..keys.partition_point(|&k| k <= t)].last().copied().unwrap_or(t)
}

pub fn nearest_key(keys: &[i64], t: i64) -> Option<i64> {
    let i = keys.partition_point(|&k| k < t);
    [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|i| keys.get(i).copied()).min_by_key(|k| (k - t).abs())
}

fn even(v: f64) -> u32 {
    ((v.round() as u32) & !1).max(2)
}

pub fn base_size(src: (u32, u32), crop: Option<Crop>) -> (u32, u32) {
    crop.map_or(src, |c| (c.w, c.h))
}

/// `base` scaled to `height` when that's smaller, even on both sides.
pub fn scaled(base: (u32, u32), height: Option<u32>) -> (u32, u32) {
    match height {
        Some(h) if h < base.1 => (even(base.0 as f64 * h as f64 / base.1 as f64), even(h as f64)),
        _ => (even(base.0 as f64), even(base.1 as f64)),
    }
}

pub fn heights(base: (u32, u32)) -> Vec<u32> {
    HEIGHTS.into_iter().filter(|&h| h < base.1).collect()
}

/// The video bitrate that brings `len` with `audio_bytes` of sound just under `target_bytes`.
pub(super) fn bitrate_for(target_bytes: u64, len: i64, audio_bytes: u64) -> u32 {
    let secs = (len as f64 / SECOND as f64).max(0.1);
    let budget = (target_bytes as f64 * UNDER - MUXING_FIXED as f64) / MUXING - audio_bytes as f64;
    (budget * 8.0 / secs).max(MIN_BPS) as u32
}

/// The source's bitrate carried over to the output's pixels, frame rate and codec.
fn default_bitrate(src: &Source, out: (u32, u32), fps: Option<u32>, codec: Codec) -> u32 {
    let size = src.info.size;
    let pixels = (out.0 as f64 * out.1 as f64) / (size.0.max(1) as f64 * size.1.max(1) as f64);
    let rate = fps.map_or(1.0, |f| f as f64 / src.info.fps.max(1.0));
    let share = match (src.info.codec.unwrap_or(Codec::H264), codec) {
        (Codec::H264, Codec::Hevc) => HEVC_SHARE,
        (Codec::Hevc, Codec::H264) => 1.0 / HEVC_SHARE,
        _ => 1.0,
    };
    (src.video_bps() * pixels.min(1.0).powf(0.85) * rate.min(1.0).powf(0.6) * share).clamp(300_000.0, 400e6).round() as u32
}

fn muxed(payload: u64) -> u64 {
    (payload as f64 * MUXING) as u64 + MUXING_FIXED
}

pub fn plan(edit: &Edit, s: &Settings, src: &Source) -> Job {
    let size = src.info.size;
    let kept = edit.kept();
    let crop = edit.crop.filter(|c| !c.is_full(size)).map(|c| c.even(size));
    let base = base_size(size, crop);
    let target = s.target_mb.map(|mb| mb as u64 * MB);
    if s.format == Format::Gif {
        let len = kept.iter().map(|r| r.len()).sum();
        let mut out = scaled(base, s.height);
        let frames = gif_frames(len, s.gif_fps);
        let est = gif_estimate(out, frames) as f64;
        if let Some(t) = target
            && est > t as f64 * UNDER
        {
            let k = (t as f64 * UNDER / est).sqrt();
            out = (even(out.0 as f64 * k).max(MIN_GIF), even(out.1 as f64 * k).max(MIN_GIF));
        }
        return Job::Gif(GifJob { kept, crop, size: out, fps: s.gif_fps, target });
    }
    let tracks = tracks(&edit.muted);
    let combine = s.combine && tracks.len() > 1;
    let height = s.height.filter(|&h| h < base.1);
    let fps = s.fps.filter(|&f| (f as f64) < src.info.fps - 0.01);
    let kbps = s.kbps.filter(|k| KBPS.contains(k));
    let codec = s.codec.resolve(src.info.codec);
    let same_codec = src.info.codec.is_none_or(|c| c == codec);
    if src.index.is_some() && crop.is_none() && height.is_none() && fps.is_none() && kbps.is_none() && same_codec {
        let pieces = kept.iter().map(|r| Piece { lead: lead(src.keys(), r.start), start: r.start, end: r.end }).collect();
        let copy = VideoJob { pieces, tracks: tracks.clone(), combine, video: Video::Copy, target };
        if target.is_none_or(|t| estimate(&Job::Video(copy.clone()), src) <= t) {
            return Job::Video(copy);
        }
    }
    let out = scaled(base, height);
    let pieces: Vec<Piece> = kept.iter().map(|r| Piece { lead: r.start, start: r.start, end: r.end }).collect();
    let mut job = VideoJob { pieces, tracks, combine, video: Video::Copy, target };
    let bitrate = match (kbps, target) {
        (Some(k), _) => k * 1000,
        (None, Some(t)) => {
            let fit = bitrate_for(t, Job::Video(job.clone()).len(), audio_bytes(&job, src));
            default_bitrate(src, out, fps, codec).min(fit)
        }
        (None, None) => default_bitrate(src, out, fps, codec),
    };
    job.video = Video::Encode { crop, size: out, bitrate, fps, codec };
    Job::Video(job)
}

pub(super) fn audio_bytes(job: &VideoJob, src: &Source) -> u64 {
    let len: i64 = job.pieces.iter().map(|p| p.end - p.lead).sum();
    if job.combine {
        return AAC_BYTES_PER_SECOND * len as u64 / SECOND as u64;
    }
    let piece = |k: usize| job.pieces.iter().map(|p| src.audio_bytes(k, p.lead, p.end)).sum::<u64>();
    job.tracks.iter().map(|&k| piece(k)).sum()
}

pub fn estimate(job: &Job, src: &Source) -> u64 {
    match job {
        Job::Gif(g) => gif_estimate(g.size, gif_frames(job.len(), g.fps)),
        Job::Video(v) => {
            let video = match v.video {
                Video::Copy => {
                    let samples = src.index.map(|i| &i.video);
                    v.pieces.iter().map(|p| samples.map_or(0, |s| s.bytes(p.lead, p.end))).sum()
                }
                Video::Encode { bitrate, .. } => bitrate as u64 * job.len() as u64 / SECOND as u64 / 8,
            };
            muxed(video + audio_bytes(v, src))
        }
    }
}

/// `≈ 18 MB`-style figure in the same megabytes as the targets.
pub fn size_label(bytes: u64) -> String {
    let mb = bytes as f64 / MB as f64;
    if mb < 1.0 {
        format!("{} KB", ((bytes as f64 / 1000.0).round() as u64).max(1))
    } else if mb < 10.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{} MB", mb.round())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::index::Samples;
    use crate::clip::info::Track;

    const S: i64 = SECOND;

    fn info(tracks: usize) -> Info {
        Info {
            duration: 20 * S,
            size: (1920, 1080),
            fps: 30.0,
            codec: None,
            tracks: (0..tracks).map(|k| Track { stream: k as u32 + 1, name: format!("t{k}") }).collect(),
        }
    }

    /// 30 fps, a keyframe every 2 s, 50 KB frames; audio 20 KB/s per track in 1/4 s packets.
    fn index(tracks: usize) -> Index {
        let video = Samples::new((0..600).map(|i| (i * S / 30, 50_000, i % 60 == 0)).collect());
        let audio = (0..tracks).map(|_| Samples::new((0..80).map(|i| (i * S / 4, 5_000, true)).collect())).collect();
        Index { video, audio }
    }

    fn src<'a>(info: &'a Info, index: Option<&'a Index>) -> Source<'a> {
        Source { info, index, bytes: 31_000_000 }
    }

    fn edit(tracks: usize) -> Edit {
        let mut e = Edit::new(20 * S, tracks);
        e.range = Range { start: 3 * S, end: 15 * S };
        e
    }

    #[test]
    fn muted_lanes_drop_their_tracks_and_combining_needs_two() {
        assert_eq!(tracks(&[false, true, false]), [0, 2]);
        assert!(tracks(&[true, true]).is_empty());
        let (info, idx) = (info(3), index(3));
        let mut e = edit(3);
        e.muted = vec![false, true, true];
        let s = Settings { combine: true, ..Settings::default() };
        let Job::Video(v) = plan(&e, &s, &src(&info, Some(&idx))) else { panic!() };
        assert_eq!((v.tracks.clone(), v.combine), (vec![0], false));
        e.muted[2] = false;
        let Job::Video(v) = plan(&e, &s, &src(&info, Some(&idx))) else { panic!() };
        assert_eq!((v.tracks, v.combine), (vec![0, 2], true));
    }

    #[test]
    fn copied_pieces_start_on_the_keyframe_at_or_before() {
        let keys = [0, 2 * S, 4 * S];
        assert_eq!(lead(&keys, 3 * S), 2 * S);
        assert_eq!(lead(&keys, 2 * S), 2 * S);
        assert_eq!(lead(&keys, 2 * S - 1), 0);
        assert_eq!(lead(&keys, 9 * S), 4 * S);
        assert_eq!(lead(&[], 7), 7);
        assert_eq!(lead(&[S], 0), 0);
        assert_eq!(nearest_key(&keys, 3 * S + 1), Some(4 * S));
        assert_eq!(nearest_key(&keys, 3 * S - 1), Some(2 * S));
        assert_eq!(nearest_key(&keys, 99 * S), Some(4 * S));
        assert_eq!(nearest_key(&[], 1), None);
    }

    #[test]
    fn plain_trims_copy_and_anything_else_reencodes() {
        let (info, idx) = (info(2), index(2));
        let source = src(&info, Some(&idx));
        let e = edit(2);
        let plain = Settings::default();
        let Job::Video(v) = plan(&e, &plain, &source) else { panic!() };
        assert_eq!(v.video, Video::Copy);
        assert_eq!(v.pieces, [Piece { lead: 2 * S, start: 3 * S, end: 15 * S }]);
        assert!(!v.whole(&info));
        let Job::Video(v) = plan(&Edit::new(20 * S, 2), &plain, &source) else { panic!() };
        assert!(v.whole(&info));

        let small = Settings { height: Some(720), ..plain };
        let Job::Video(v) = plan(&e, &small, &source) else { panic!() };
        assert!(matches!(v.video, Video::Encode { size: (1280, 720), crop: None, .. }));
        assert_eq!(v.pieces[0].lead, 3 * S);

        let mut cropped = e.clone();
        cropped.crop = Some(Crop { x: 101, y: 0, w: 1001, h: 1080 });
        let Job::Video(v) = plan(&cropped, &plain, &source) else { panic!() };
        assert!(matches!(v.video, Video::Encode { size: (1000, 1080), crop: Some(Crop { x: 100, w: 1000, .. }), .. }));
        let Job::Video(v) = plan(&cropped, &Settings { height: Some(540), ..plain }, &source) else { panic!() };
        assert!(matches!(v.video, Video::Encode { size: (500, 540), .. }));

        let Job::Video(v) = plan(&e, &plain, &src(&info, None)) else { panic!() };
        assert!(matches!(v.video, Video::Encode { size: (1920, 1080), .. }));
        let gif = Settings { format: Format::Gif, height: Some(360), ..plain };
        let Job::Gif(g) = plan(&cropped, &gif, &source) else { panic!() };
        assert_eq!((g.size, g.fps, g.kept.len()), ((332, 360), 15, 1));
    }

    #[test]
    fn estimates_follow_the_samples_or_the_bitrate() {
        let (info, idx) = (info(2), index(2));
        let source = src(&info, Some(&idx));
        let mut e = edit(2);
        e.muted[1] = true;
        let job = plan(&e, &Settings::default(), &source);
        let payload = 13 * 30 * 50_000 + 13 * 4 * 5_000;
        assert_eq!(estimate(&job, &source), muxed(payload));
        assert_eq!(job.len(), 13 * S);
        let combined = plan(&edit(2), &Settings { combine: true, ..Settings::default() }, &source);
        assert_eq!(estimate(&combined, &source), muxed(13 * 30 * 50_000 + 13 * AAC_BYTES_PER_SECOND));
        let small = plan(&e, &Settings { height: Some(540), ..Settings::default() }, &source);
        let Job::Video(VideoJob { video: Video::Encode { bitrate, .. }, .. }) = small else { panic!() };
        assert_eq!(estimate(&small, &source), muxed(bitrate as u64 * 12 / 8 + 12 * 4 * 5_000));
        assert!(bitrate > 12_000_000 / 4 && bitrate < 12_000_000 / 2, "{bitrate}");
    }

    #[test]
    fn size_targets_pick_a_bitrate_just_under() {
        let audio = 20 * 24_000;
        let bps = bitrate_for(10 * MB, 20 * S, audio);
        let total = muxed(bps as u64 * 20 / 8 + audio);
        assert!(total <= 10 * MB && total > 9 * MB, "{total}");
        assert_eq!(bitrate_for(MB, 600 * S, 0) as f64, MIN_BPS);

        let (info, idx) = (info(2), index(2));
        let source = src(&info, Some(&idx));
        let e = edit(2);
        let fits = plan(&e, &Settings { target_mb: Some(50), ..Settings::default() }, &source);
        assert!(matches!(&fits, Job::Video(VideoJob { video: Video::Copy, .. })));
        let tight = plan(&e, &Settings { target_mb: Some(10), ..Settings::default() }, &source);
        assert!(matches!(&tight, Job::Video(VideoJob { video: Video::Encode { size: (1920, 1080), .. }, .. })));
        let est = estimate(&tight, &source);
        assert!(est <= 10 * MB && est > 9 * MB, "{est}");

        let gif = Settings { format: Format::Gif, target_mb: Some(10), ..Settings::default() };
        let g = plan(&e, &gif, &source);
        let Job::Gif(GifJob { size, .. }) = g else { panic!() };
        assert!(size.0 < 1920 && size.0 % 2 == 0 && size.1 % 2 == 0);
        assert!(estimate(&g, &source) <= 10 * MB);
    }

    #[test]
    fn frame_rate_bitrate_and_codec_leave_the_fast_path() {
        let mut info = info(2);
        (info.fps, info.codec) = (60.0, Some(Codec::H264));
        let idx = index(2);
        let source = src(&info, Some(&idx));
        let e = edit(2);
        let copies = |s: Settings| matches!(plan(&e, &s, &source), Job::Video(VideoJob { video: Video::Copy, .. }));
        assert!(copies(Settings::default()));
        assert!(copies(Settings { codec: CodecChoice::H264, ..Settings::default() }));
        assert!(copies(Settings { fps: Some(60), ..Settings::default() }));
        assert!(copies(Settings { kbps: Some(50), ..Settings::default() }));

        let Job::Video(v) = plan(&e, &Settings { codec: CodecChoice::Hevc, ..Settings::default() }, &source) else { panic!() };
        assert_eq!(v.video, Video::Encode { crop: None, size: (1920, 1080), bitrate: 8_400_000, fps: None, codec: Codec::Hevc });
        assert_eq!(v.pieces[0].lead, 3 * S);

        let Job::Video(v) = plan(&e, &Settings { fps: Some(30), ..Settings::default() }, &source) else { panic!() };
        let Video::Encode { bitrate, fps, codec, .. } = v.video else { panic!() };
        assert_eq!((fps, codec), (Some(30), Codec::H264));
        assert!(bitrate < 12_000_000 && bitrate > 6_000_000, "{bitrate}");

        let custom = Settings { kbps: Some(3000), ..Settings::default() };
        let job = plan(&e, &custom, &source);
        assert!(matches!(job, Job::Video(VideoJob { video: Video::Encode { bitrate: 3_000_000, fps: None, .. }, .. })));
        assert_eq!(estimate(&job, &source), muxed(3_000_000 * 12 / 8 + 2 * 48 * 5_000));

        assert_eq!(frame_rates(60.0), [30, 24]);
        assert_eq!(frame_rates(59.94), [30, 24]);
        assert_eq!(frame_rates(144.0), [60, 30, 24]);
        assert!(frame_rates(24.0).is_empty());
    }

    #[test]
    fn a_size_limit_and_a_custom_bitrate_exclude_each_other() {
        let mut s = Settings::default();
        s.set_kbps(Some(3000));
        assert_eq!((s.kbps, s.target_mb), (Some(3000), None));
        s.set_target(Some(10));
        assert_eq!((s.kbps, s.target_mb), (None, Some(10)));
        s.set_kbps(Some(5000));
        assert_eq!((s.kbps, s.target_mb), (Some(5000), None));
        s.set_kbps(Some(10));
        assert_eq!((s.kbps, s.target_mb), (None, None));
        s.set_target(Some(25));
        s.set_kbps(None);
        assert_eq!((s.kbps, s.target_mb), (None, Some(25)));
        s.set_target(None);
        assert_eq!((s.kbps, s.target_mb), (None, None));

        assert_eq!(parse_mbps("2.5"), Some(2500));
        assert_eq!(parse_mbps(" 2,5 "), Some(2500));
        assert_eq!(parse_mbps("0.2"), Some(200));
        assert_eq!(parse_mbps("200"), Some(200_000));
        for bad in ["0.1", "201", "abc", "", "-3", "NaN", "inf"] {
            assert_eq!(parse_mbps(bad), None, "{bad}");
        }

        let (info, idx) = (info(2), index(2));
        let both = Settings { kbps: Some(4000), target_mb: Some(1), ..Settings::default() };
        let job = plan(&edit(2), &both, &src(&info, Some(&idx)));
        assert!(matches!(job, Job::Video(VideoJob { video: Video::Encode { bitrate: 4_000_000, .. }, .. })));
    }

    #[test]
    fn sizes_scale_after_the_crop_and_stay_even() {
        assert_eq!(scaled((1920, 1080), Some(720)), (1280, 720));
        assert_eq!(scaled((1920, 1080), Some(1080)), (1920, 1080));
        assert_eq!(scaled((3440, 1440), Some(540)), (1290, 540));
        assert_eq!(scaled((999, 777), None), (998, 776));
        assert_eq!(heights((1920, 1080)), [720, 540, 360]);
        assert_eq!(heights((1280, 720)), [540, 360]);
        assert_eq!(heights((640, 360)), Vec::<u32>::new());
        assert_eq!(size_label(18_400_000), "18 MB");
        assert_eq!(size_label(9_640_000), "9.6 MB");
        assert_eq!(size_label(420_000), "420 KB");
    }
}
