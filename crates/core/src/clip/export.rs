use serde::{Deserialize, Serialize};
use super::clicks::ClickLook;
use super::crop::Crop;
use super::edit::{Edit, FULL_VOLUME};
use super::index::Index;
use super::info::Info;
use super::keys::KeyLook;
use super::math::{Range, SECOND, gif_estimate, gif_frames};
use crate::record::Codec;
use crate::record::clicks::{Click, Style};
use crate::record::keys;

pub const TARGETS_MB: [u32; 3] = [10, 25, 50];
pub(super) const HEIGHTS: [u32; 3] = [720, 540, 360];
pub(super) const VIDEO_FPS: [u32; 3] = [60, 30, 24];
/// A custom bitrate, in kbit/s: 0.2 to 200 Mbps.
pub(super) const KBPS: std::ops::RangeInclusive<u32> = 200..=200_000;
/// Decimal megabytes, so a "≤ 10 MB" file also fits limits counted in MiB.
pub const MB: u64 = 1_000_000;
pub const AAC_BYTES_PER_SECOND: u64 = 24_000;
// measured: media foundation's aac encoder spends about 1.5 kbit/s on digital silence.
pub const SILENCE_BYTES_PER_SECOND: u64 = 200;
/// Aim this far under a size target.
const UNDER: f64 = 0.96;
const MUXING: f64 = 1.004;
const MUXING_FIXED: u64 = 8 * 1024;
const MIN_BPS: f64 = 150_000.0;
const MIN_GIF: u32 = 64;
/// hevc keeps the look of h.264 at about this share of its bitrate.
const HEVC_SHARE: f64 = 0.7;
/// The most an automatic h.264 bitrate spends on 1080p60. Fast game footage already looks clean
/// there at ~12 Mbps, so carrying a near-lossless recording's rate any higher only grows the file.
/// Other sizes and frame rates scale like [`bitrate_scaled`].
const CEILING_BPS: f64 = 19_000_000.0;
/// The qp a re-encode holds when nothing asks for a size or a bitrate and the source's rate is under
/// [`CEILING_BPS`]: a notch above "compress · light", so it looks like the source. Carrying a low
/// average bitrate over instead starves the frames after a keyframe (a still screen recording at
/// 1.3 Mbps opened blocky for 0.1 s: SSIM 0.94, at this qp 0.999, and the file came out smaller).
const KEEP_QP: u32 = 18;

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

/// How hard "compress" squeezes: the video is re-encoded at one steady quality (a constant qp) instead of a
/// bitrate, so every scene keeps the same look and the size is whatever that takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compress {
    Light,
    #[default]
    Medium,
    Strong,
}

impl Compress {
    /// The encoder's qp, h.264 and hevc alike (hevc just lands smaller). On a 117 Mbps 3440×1440 game
    /// recording nvenc's h.264 measured −32 %, −45 % and −56 % at ssim 0.991, 0.989 and 0.986.
    pub fn qp(self) -> u32 {
        match self {
            Compress::Light => 20,
            Compress::Medium => 22,
            Compress::Strong => 24,
        }
    }

    /// About the share of a near-lossless source's bitrate it keeps (the measurements above); only the
    /// estimate uses it, a compact source shrinks less.
    fn share(self) -> f64 {
        match self {
            Compress::Light => 0.68,
            Compress::Medium => 0.55,
            Compress::Strong => 0.44,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub format: Format,
    /// A size limit; never set together with `kbps` or `compress`.
    pub target_mb: Option<u32>,
    /// Output height after the crop; None keeps it.
    pub height: Option<u32>,
    pub gif_fps: u32,
    /// Video frame rate; None keeps the source's.
    pub fps: Option<u32>,
    /// A custom video bitrate; never set together with `target_mb` or `compress`.
    pub kbps: Option<u32>,
    /// Re-encode at a steady quality; never set together with `target_mb` or `kbps`.
    pub compress: Option<Compress>,
    pub codec: CodecChoice,
    pub combine: bool,
    pub clicks: ClickLook,
    pub keys: KeyLook,
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
            compress: None,
            codec: CodecChoice::Same,
            combine: false,
            clicks: ClickLook::default(),
            keys: KeyLook::default(),
        }
    }
}

impl Settings {
    /// A size limit takes over from a custom bitrate and compressing.
    pub fn set_target(&mut self, mb: Option<u32>) {
        self.target_mb = mb;
        if mb.is_some() {
            self.kbps = None;
            self.compress = None;
        }
    }

    /// A custom bitrate takes over from a size limit and compressing; out-of-range values fall back to auto.
    pub fn set_kbps(&mut self, kbps: Option<u32>) {
        self.kbps = kbps.filter(|k| KBPS.contains(k));
        if self.kbps.is_some() {
            self.target_mb = None;
            self.compress = None;
        }
    }

    /// Compressing takes over from a size limit and a custom bitrate.
    pub fn set_compress(&mut self, c: Option<Compress>) {
        self.compress = c;
        if c.is_some() {
            self.target_mb = None;
            self.kbps = None;
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
    pub accent: [u8; 3],
}

impl Source<'_> {
    fn secs(&self) -> f64 {
        (self.info.duration.max(1) as f64 / SECOND as f64).max(0.001)
    }

    /// The file's bytes in proportion to `len` of it.
    pub fn bytes_for(&self, len: i64) -> u64 {
        let share = (len.max(0) as f64 / self.info.duration.max(1) as f64).min(1.0);
        (self.bytes as f64 * share).round() as u64
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
    /// `fps` None keeps the source's frame timing; otherwise frames land on that grid. With `qp` the encoder
    /// holds that constant quality and `bitrate` only estimates the size.
    Encode { crop: Option<Crop>, size: (u32, u32), bitrate: u32, fps: Option<u32>, codec: Codec, qp: Option<u32> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sound {
    pub source: Option<usize>,
    pub name: String,
    /// Percent, as the lane's [`crate::clip::edit::Lane::volume`].
    pub volume: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Burn {
    pub clicks: Vec<Click>,
    pub style: Style,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyBurn {
    pub keys: Vec<keys::Key>,
    pub style: keys::Style,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VideoJob {
    pub pieces: Vec<Piece>,
    pub tracks: Vec<Sound>,
    pub combine: bool,
    pub video: Video,
    pub target: Option<u64>,
    pub burn: Option<Burn>,
    pub keys: Option<KeyBurn>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GifJob {
    pub kept: Vec<Range>,
    pub crop: Option<Crop>,
    pub size: (u32, u32),
    pub fps: u32,
    pub target: Option<u64>,
    pub burn: Option<Burn>,
    pub keys: Option<KeyBurn>,
}

#[derive(Clone, Debug, PartialEq)]
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
            && info.clicks.is_none()
            && info.keys.is_none()
            && self.tracks.len() == info.tracks.len()
            && (self.tracks.iter().enumerate())
                .all(|(k, t)| t.source == Some(k) && t.name == info.tracks[k].name && self.copies(t))
            && matches!(self.pieces[..], [p] if p.lead <= 0 && p.end >= info.duration)
    }

    /// `t`'s packets go into the file as they are: a recorded track, not mixed with others, at its own
    /// volume. Anything else is decoded and encoded again.
    pub fn copies(&self, t: &Sound) -> bool {
        t.source.is_some() && !self.combine && t.volume == FULL_VOLUME
    }

    /// How much sooner than the trim start a copied file starts: its first piece begins on the
    /// keyframe before. 0 for an encoded one, which starts on the frame.
    pub fn early(&self) -> i64 {
        self.pieces.first().map_or(0, |p| p.start - p.lead)
    }
}

/// The lanes that aren't muted, in order, which are the tracks the file gets.
pub(super) fn sounds(edit: &Edit, info: &Info) -> Vec<Sound> {
    (edit.lanes.iter())
        .filter(|l| !l.muted && l.source.is_none_or(|k| k < info.tracks.len()))
        .map(|l| Sound { source: l.source, name: l.name(&info.tracks).to_string(), volume: l.volume })
        .collect()
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

/// `bps` for `pixels` times as many pixels at `rate` times the frame rate: smaller frames and
/// fewer of them need more bits each.
fn bitrate_scaled(bps: f64, pixels: f64, rate: f64) -> f64 {
    bps * pixels.powf(0.85) * rate.powf(0.6)
}

/// The source's bitrate carried over to the output's pixels, frame rate and codec.
fn carried_bitrate(src: &Source, out: (u32, u32), fps: Option<u32>, codec: Codec) -> f64 {
    let size = src.info.size;
    let pixels = out.0 as f64 * out.1 as f64 / (size.0.max(1) as f64 * size.1.max(1) as f64);
    let rate = fps.map_or(1.0, |f| f as f64 / src.info.fps.max(1.0));
    let share = match (src.info.codec.unwrap_or(Codec::H264), codec) {
        (Codec::H264, Codec::Hevc) => HEVC_SHARE,
        (Codec::Hevc, Codec::H264) => 1.0 / HEVC_SHARE,
        _ => 1.0,
    };
    bitrate_scaled(src.video_bps(), pixels.min(1.0), rate.min(1.0)) * share
}

/// [`CEILING_BPS`] for the output's pixels, frame rate and codec.
fn ceiling(src: &Source, out: (u32, u32), fps: Option<u32>, codec: Codec) -> f64 {
    let out_pixels = out.0 as f64 * out.1 as f64;
    let out_fps = fps.map_or(src.info.fps, f64::from).max(1.0);
    let codec_share = if codec == Codec::Hevc { HEVC_SHARE } else { 1.0 };
    bitrate_scaled(CEILING_BPS, out_pixels / (1920.0 * 1080.0), out_fps / 60.0) * codec_share
}

fn sane_bps(bps: f64) -> u32 {
    bps.clamp(300_000.0, 400e6).round() as u32
}

/// [`carried_bitrate`], never above [`ceiling`].
fn default_bitrate(src: &Source, out: (u32, u32), fps: Option<u32>, codec: Codec) -> u32 {
    sane_bps(carried_bitrate(src, out, fps, codec).min(ceiling(src, out, fps, codec)))
}

fn muxed(payload: u64) -> u64 {
    (payload as f64 * MUXING) as u64 + MUXING_FIXED
}

/// Why an export makes the picture anew instead of copying it as recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reencode {
    /// A gif is always made anew.
    Gif,
    /// Clicks are drawn into the picture.
    Clicks,
    /// Keys are drawn into the picture.
    Keys,
    Crop,
    /// Scaled down to this height.
    Height(u32),
    /// Down to this frame rate.
    Fps(u32),
    Compress,
    /// A custom bitrate.
    Bitrate,
    /// Another codec than the recording's.
    Codec(Codec),
    /// The copy wouldn't fit under this size limit, in MB.
    Size(u32),
    /// No sample index to cut the file's packets by (not an mp4 this reads).
    Unindexed,
}

pub fn plan(edit: &Edit, s: &Settings, src: &Source) -> Job {
    plan_why(edit, s, src).0
}

/// [`plan`], and why its picture is made anew: empty exactly when the video is copied, lossless.
pub fn plan_why(edit: &Edit, s: &Settings, src: &Source) -> (Job, Vec<Reencode>) {
    let size = src.info.size;
    let kept = edit.kept();
    let crop = edit.crop.filter(|c| !c.is_full(size)).map(|c| c.even(size));
    let base = base_size(size, crop);
    let target = s.target_mb.map(|mb| mb as u64 * MB);
    let burn = edit.clicks.as_ref().and_then(|c| {
        let style = s.clicks.style(src.accent, c.scale);
        let clicks = c.burnt(&kept, &style);
        (!clicks.is_empty()).then_some(Burn { clicks, style })
    });
    let keys = edit.keys.as_ref().and_then(|k| {
        let style = s.keys.style(src.accent, k.scale);
        let list = k.burnt(&kept, &style);
        (!list.is_empty()).then_some(KeyBurn { keys: list, style })
    });
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
        return (Job::Gif(GifJob { kept, crop, size: out, fps: s.gif_fps, target, burn, keys }), vec![Reencode::Gif]);
    }
    let tracks = sounds(edit, src.info);
    let combine = s.combine && tracks.iter().filter(|t| t.source.is_some()).count() > 1;
    let height = s.height.filter(|&h| h < base.1);
    let fps = s.fps.filter(|&f| (f as f64) < src.info.fps - 0.01);
    let kbps = s.kbps.filter(|k| KBPS.contains(k));
    let compress = s.compress.filter(|_| kbps.is_none() && target.is_none());
    let codec = s.codec.resolve(src.info.codec);
    let same_codec = src.info.codec.is_none_or(|c| c == codec);
    let why: Vec<Reencode> = [
        burn.as_ref().map(|_| Reencode::Clicks),
        keys.as_ref().map(|_| Reencode::Keys),
        crop.map(|_| Reencode::Crop),
        height.map(Reencode::Height),
        fps.map(Reencode::Fps),
        compress.map(|_| Reencode::Compress),
        kbps.map(|_| Reencode::Bitrate),
        (!same_codec).then_some(Reencode::Codec(codec)),
        src.index.is_none().then_some(Reencode::Unindexed),
    ]
    .into_iter()
    .flatten()
    .collect();
    if why.is_empty() {
        let pieces = kept.iter().map(|r| Piece { lead: lead(src.keys(), r.start), start: r.start, end: r.end }).collect();
        let copy = VideoJob { pieces, tracks: tracks.clone(), combine, video: Video::Copy, target, burn: None, keys: None };
        match s.target_mb {
            Some(mb) if estimate(&Job::Video(copy.clone()), src) > mb as u64 * MB => {}
            _ => return (Job::Video(copy), why),
        }
    }
    let why = if why.is_empty() { vec![Reencode::Size(s.target_mb.unwrap_or_default())] } else { why };
    let out = scaled(base, height);
    let pieces: Vec<Piece> = kept.iter().map(|r| Piece { lead: r.start, start: r.start, end: r.end }).collect();
    let mut job = VideoJob { pieces, tracks, combine, video: Video::Copy, target, burn, keys };
    // with a qp, `bitrate` is only what the estimate expects
    let (bitrate, qp) = match (kbps, target, compress) {
        (Some(k), ..) => (k * 1000, None),
        (None, Some(t), _) => {
            let fit = bitrate_for(t, Job::Video(job.clone()).len(), audio_bytes(&job, src));
            (default_bitrate(src, out, fps, codec).min(fit), None)
        }
        (None, None, Some(c)) => (sane_bps(carried_bitrate(src, out, fps, codec) * c.share()), Some(c.qp())),
        (None, None, None) => {
            let carried = carried_bitrate(src, out, fps, codec);
            let cap = ceiling(src, out, fps, codec);
            if carried > cap { (sane_bps(cap), None) } else { (sane_bps(carried), Some(KEEP_QP)) }
        }
    };
    job.video = Video::Encode { crop, size: out, bitrate, fps, codec, qp };
    (Job::Video(job), why)
}

pub(super) fn audio_bytes(job: &VideoJob, src: &Source) -> u64 {
    let len: i64 = job.pieces.iter().map(|p| p.end - p.lead).sum();
    let per_second = |rate: u64| rate * len as u64 / SECOND as u64;
    let silence = job.tracks.iter().filter(|t| t.source.is_none()).count() as u64 * per_second(SILENCE_BYTES_PER_SECOND);
    if job.combine {
        return per_second(AAC_BYTES_PER_SECOND) + silence;
    }
    let piece = |k: usize| job.pieces.iter().map(|p| src.audio_bytes(k, p.lead, p.end)).sum::<u64>();
    let sound = |t: &Sound| match t.source {
        Some(k) if job.copies(t) => piece(k),
        Some(_) => per_second(AAC_BYTES_PER_SECOND),
        None => 0,
    };
    job.tracks.iter().map(sound).sum::<u64>() + silence
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

/// The done message's comparison of a compressed export (`after`) with what that stretch of the source took
/// (`before`): `412 MB → 190 MB (−54%)`, or plainly that it isn't smaller. None with nothing to compare to.
pub fn saving_label(before: u64, after: u64) -> Option<String> {
    if before == 0 {
        return None;
    }
    let sizes = format!("{} → {}", size_label(before), size_label(after));
    let pct = (before.saturating_sub(after) as f64 * 100.0 / before as f64).round();
    Some(if after >= before {
        format!("{sizes}, not smaller: the source was already compact")
    } else if pct < 1.0 {
        format!("{sizes}, about the same: the source was already compact")
    } else {
        format!("{sizes} (−{pct}%)")
    })
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
            clicks: None,
            keys: None,
        }
    }

    /// 30 fps, a keyframe every 2 s, 50 KB frames; audio 20 KB/s per track in 1/4 s packets.
    fn index(tracks: usize) -> Index {
        let video = Samples::new((0..600).map(|i| (i * S / 30, 50_000, i % 60 == 0)).collect());
        let audio = (0..tracks).map(|_| Samples::new((0..80).map(|i| (i * S / 4, 5_000, true)).collect())).collect();
        Index { video, audio }
    }

    fn src<'a>(info: &'a Info, index: Option<&'a Index>) -> Source<'a> {
        Source { info, index, bytes: 31_000_000, accent: [0xE5, 0x40, 0x2F] }
    }

    fn edit(tracks: usize) -> Edit {
        let mut e = Edit::new(20 * S, tracks);
        e.range = Range { start: 3 * S, end: 15 * S };
        e
    }

    #[test]
    fn muted_lanes_drop_their_tracks_and_combining_needs_two() {
        let (info, idx) = (info(3), index(3));
        let mut e = edit(3);
        e.lanes[1].muted = true;
        e.lanes[2].muted = true;
        let s = Settings { combine: true, ..Settings::default() };
        let Job::Video(v) = plan(&e, &s, &src(&info, Some(&idx))) else { panic!() };
        assert_eq!((sources(&v), v.combine), (vec![Some(0)], false));
        e.lanes[2].muted = false;
        let Job::Video(v) = plan(&e, &s, &src(&info, Some(&idx))) else { panic!() };
        assert_eq!((sources(&v), v.combine), (vec![Some(0), Some(2)], true));
        e.lanes.iter_mut().for_each(|l| l.muted = true);
        assert!(sounds(&e, &info).is_empty());
    }

    fn sources(v: &VideoJob) -> Vec<Option<usize>> {
        v.tracks.iter().map(|t| t.source).collect()
    }

    #[test]
    fn deleted_reordered_renamed_and_empty_lanes_shape_the_tracks_and_still_copy() {
        let (info, idx) = (info(2), index(2));
        let source = src(&info, Some(&idx));
        let mut e = edit(2);
        assert!(e.delete_lane(0));
        e.add_empty();
        assert!(e.move_lane(1, 0));
        assert!(e.rename(1, "voice", &info.tracks));
        let job = plan(&e, &Settings::default(), &source);
        let Job::Video(v) = &job else { panic!() };
        assert_eq!(v.video, Video::Copy);
        let want = [
            Sound { source: None, name: "empty".into(), volume: FULL_VOLUME },
            Sound { source: Some(1), name: "voice".into(), volume: FULL_VOLUME },
        ];
        assert_eq!(v.tracks, want);
        assert!(!v.combine);
        let payload = 13 * 30 * 50_000 + 13 * 4 * 5_000 + 13 * SILENCE_BYTES_PER_SECOND;
        assert_eq!(estimate(&job, &source), muxed(payload));

        let mut whole = Edit::new(20 * S, 2);
        let is_whole = |e: &Edit| matches!(plan(e, &Settings::default(), &source), Job::Video(v) if v.whole(&info));
        assert!(is_whole(&whole));
        assert!(whole.move_lane(1, 0) && !is_whole(&whole));
        assert!(whole.move_lane(1, 0) && is_whole(&whole));
        assert!(whole.rename(0, "x", &info.tracks) && !is_whole(&whole));
        assert!(whole.rename(0, "t0", &info.tracks) && is_whole(&whole));
        whole.add_empty();
        assert!(!is_whole(&whole));
        assert!(whole.delete_lane(2) && whole.delete_lane(1) && !is_whole(&whole));

        let combine = Settings { combine: true, ..Settings::default() };
        let Job::Video(v) = plan(&e, &combine, &source) else { panic!() };
        assert!(!v.combine, "one sound and one empty track don't combine");
        let mut both = edit(2);
        both.add_empty();
        let job = plan(&both, &combine, &source);
        let Job::Video(v) = &job else { panic!() };
        assert!(v.combine && v.video == Video::Copy);
        assert_eq!(sources(v), [Some(0), Some(1), None]);
        let payload = 13 * 30 * 50_000 + 13 * AAC_BYTES_PER_SECOND + 13 * SILENCE_BYTES_PER_SECOND;
        assert_eq!(estimate(&job, &source), muxed(payload));
    }

    #[test]
    fn only_the_tracks_whose_volume_changed_get_encoded_again() {
        let (info, idx) = (info(2), index(2));
        let source = src(&info, Some(&idx));
        let mut e = edit(2);
        assert!(e.set_volume(1, 150));
        let job = plan(&e, &Settings::default(), &source);
        let Job::Video(v) = &job else { panic!() };
        assert_eq!(v.video, Video::Copy, "the picture keeps its fast path");
        assert_eq!(v.tracks.iter().map(|t| (t.source, t.volume)).collect::<Vec<_>>(), [(Some(0), 100), (Some(1), 150)]);
        assert_eq!(v.tracks.iter().map(|t| v.copies(t)).collect::<Vec<_>>(), [true, false]);
        let payload = 13 * 30 * 50_000 + 13 * 4 * 5_000 + 13 * AAC_BYTES_PER_SECOND;
        assert_eq!(estimate(&job, &source), muxed(payload));

        let small = plan(&e, &Settings { height: Some(540), ..Settings::default() }, &source);
        let Job::Video(v) = &small else { panic!() };
        assert!(matches!(v.video, Video::Encode { .. }) && v.copies(&v.tracks[0]) && !v.copies(&v.tracks[1]));

        let combined = plan(&edit(2), &Settings { combine: true, ..Settings::default() }, &source);
        let Job::Video(v) = &combined else { panic!() };
        assert!(v.tracks.iter().all(|t| !v.copies(t)), "a mix is always encoded");
        let mut empty = edit(1);
        empty.add_empty();
        let Job::Video(v) = plan(&empty, &Settings::default(), &source) else { panic!() };
        assert_eq!(v.tracks.iter().map(|t| v.copies(t)).collect::<Vec<_>>(), [true, false], "silence is always encoded");

        let mut whole = Edit::new(20 * S, 2);
        let is_whole = |e: &Edit| matches!(plan(e, &Settings::default(), &source), Job::Video(v) if v.whole(&info));
        assert!(whole.set_volume(0, 0) && !is_whole(&whole));
        assert!(whole.set_volume(0, 100) && is_whole(&whole));
        assert!(whole.set_volume(1, 101) && !is_whole(&whole));
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
        e.lanes[1].muted = true;
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
        let held = Video::Encode { crop: None, size: (1920, 1080), bitrate: 8_400_000, fps: None, codec: Codec::Hevc, qp: Some(KEEP_QP) };
        assert_eq!(v.video, held, "under the ceiling a re-encode holds a quality");
        assert_eq!(v.pieces[0].lead, 3 * S);

        let Job::Video(v) = plan(&e, &Settings { fps: Some(30), ..Settings::default() }, &source) else { panic!() };
        let Video::Encode { bitrate, fps, codec, qp, .. } = v.video else { panic!() };
        assert_eq!((fps, codec, qp), (Some(30), Codec::H264, Some(KEEP_QP)));
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
    fn near_lossless_sources_export_at_a_sane_bitrate() {
        // a 144 fps game recording at qp 16: ~69 Mbps, ~104 fps on average
        let mut info = info(1);
        (info.size, info.fps, info.codec) = ((1842, 1006), 104.0, Some(Codec::H264));
        let frame = 69_000_000 / 8 / 104;
        let video = Samples::new((0..2080).map(|i| (i * S / 104, frame, i % 288 == 0)).collect());
        let idx = Index { video, audio: index(1).audio };
        let source = src(&info, Some(&idx));
        let e = edit(1);
        let bitrate = |choice| {
            let Job::Video(v) = plan(&e, &Settings { fps: Some(60), codec: choice, ..Settings::default() }, &source) else {
                panic!()
            };
            let Video::Encode { bitrate, qp, .. } = v.video else { panic!() };
            assert_eq!(qp, None, "over the ceiling it's a bitrate, so it doesn't grow the file");
            bitrate
        };
        // carrying the source over would keep ~50 Mbps: 33 MB for 5 seconds
        let h264 = bitrate(CodecChoice::H264);
        assert!((12_000_000..19_000_000).contains(&h264), "{h264}");
        let hevc = bitrate(CodecChoice::Hevc);
        assert!((hevc as f64 - h264 as f64 * HEVC_SHARE).abs() <= 1.0, "{hevc} vs {h264}");
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
    fn shown_clicks_in_the_kept_range_burn_in_and_anything_else_keeps_the_fast_path() {
        use crate::clip::clicks::{ClickColor, ClickSize, Clicks};
        use crate::record::clicks::{Button, Payload};
        let (mut info, idx) = (info(1), index(1));
        let at = |down: i64| Click { button: Button::Left, down, up: Some(down + S / 10), x: 10.0, y: 10.0 };
        let payload = Payload { version: 1, scale: 1.25, clicks: vec![at(S), at(5 * S)] };
        info.clicks = Some(payload.clone());
        let source = src(&info, Some(&idx));
        let mut e = edit(1);
        let copies = |e: &Edit, s: &Settings| matches!(plan(e, s, &source), Job::Video(VideoJob { video: Video::Copy, burn: None, .. }));
        assert!(copies(&e, &Settings::default()), "no clicks in the timeline");
        e.clicks = Some(Clicks::new(&payload));
        let look = ClickLook { size: ClickSize::Large, color: ClickColor::White };
        let s = Settings { clicks: look, ..Settings::default() };
        let Job::Video(v) = plan(&e, &s, &source) else { panic!() };
        assert!(matches!(v.video, Video::Encode { crop: None, size: (1920, 1080), .. }));
        let burn = v.burn.clone().unwrap();
        assert_eq!(burn.clicks, [at(5 * S)], "the one before the trim start is left out");
        assert_eq!(burn.style, look.style([0xE5, 0x40, 0x2F], 1.25));
        assert!(!v.whole(&info));

        e.clicks.as_mut().unwrap().shown = false;
        assert!(copies(&e, &s), "hidden clicks: nothing drawn, the fast path stays");
        e.clicks.as_mut().unwrap().shown = true;
        e.range = Range { start: 6 * S, end: 15 * S };
        assert!(copies(&e, &s), "no click shows in what's kept");
        let Job::Video(v) = plan(&Edit::new(20 * S, 1), &Settings::default(), &source) else { panic!() };
        assert!(!v.whole(&info), "a copy of the file would carry its clicks along");
        info.clicks = None;
        assert!(v.whole(&info));

        let mut gif = edit(1);
        gif.clicks = Some(Clicks::new(&payload));
        let Job::Gif(g) = plan(&gif, &Settings { format: Format::Gif, ..s }, &src(&info, Some(&idx))) else { panic!() };
        assert_eq!(g.burn.map(|b| b.clicks.len()), Some(1));
    }

    #[test]
    fn the_plan_says_why_it_reencodes_and_nothing_when_it_copies() {
        use crate::clip::clicks::Clicks;
        use crate::clip::keys::Keys;
        use crate::record::clicks::{Button, Payload};
        let (mut info, idx) = (info(1), index(1));
        let click = Click { button: Button::Left, down: 5 * S, up: Some(5 * S + S / 10), x: 10.0, y: 10.0 };
        let clicks = Payload { version: 1, scale: 1.0, clicks: vec![click] };
        let key = keys::Key { at: 5 * S, hold: Some(6 * S), mods: keys::Mods::default(), main: keys::KeyLabel::Text("s".into()) };
        let pressed = keys::Payload { version: 1, scale: 1.0, keys: vec![key] };
        info.codec = Some(Codec::H264);
        let source = src(&info, Some(&idx));
        let why = |e: &Edit, s: &Settings| plan_why(e, s, &source).1;
        let plain = Settings::default();

        let e = edit(1);
        let (job, none) = plan_why(&e, &plain, &source);
        assert_eq!(none, []);
        let Job::Video(v) = job else { panic!() };
        assert_eq!((v.video, v.early()), (Video::Copy, S), "the copy starts on the keyframe at 2 s");
        assert_eq!(why(&e, &Settings { target_mb: Some(50), ..plain }), [], "it fits as it is");

        let mut drawn = e.clone();
        drawn.clicks = Some(Clicks::new(&clicks));
        drawn.keys = Some(Keys::new(&pressed));
        drawn.crop = Some(Crop { x: 100, y: 0, w: 1000, h: 1080 });
        assert_eq!(why(&drawn, &plain), [Reencode::Clicks, Reencode::Keys, Reencode::Crop]);
        drawn.clicks.as_mut().unwrap().shown = false;
        drawn.keys.as_mut().unwrap().shown = false;
        drawn.crop = None;
        assert_eq!(why(&drawn, &plain), [], "hidden clicks and keys aren't drawn");

        let s = Settings { height: Some(720), fps: Some(24), codec: CodecChoice::Hevc, ..plain };
        assert_eq!(why(&e, &s), [Reencode::Height(720), Reencode::Fps(24), Reencode::Codec(Codec::Hevc)]);
        let s = Settings { height: Some(1080), fps: Some(60), codec: CodecChoice::H264, ..plain };
        assert_eq!(why(&e, &s), [], "a height, a frame rate and a codec the recording already has");
        let mut s = plain;
        s.set_compress(Some(Compress::Light));
        assert_eq!(why(&e, &s), [Reencode::Compress]);
        s.set_kbps(Some(4_000));
        assert_eq!(why(&e, &s), [Reencode::Bitrate]);

        let (job, tight) = plan_why(&e, &Settings { target_mb: Some(10), ..plain }, &source);
        assert_eq!(tight, [Reencode::Size(10)], "the copy is ~20 MB");
        assert!(matches!(&job, Job::Video(v) if v.early() == 0 && matches!(v.video, Video::Encode { .. })));
        assert_eq!(plan_why(&e, &plain, &src(&info, None)).1, [Reencode::Unindexed]);
        assert_eq!(why(&e, &Settings { format: Format::Gif, ..plain }), [Reencode::Gif]);
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

    #[test]
    fn compressing_holds_a_quality_and_never_copies() {
        assert_eq!(Compress::default(), Compress::Medium);
        assert_eq!([Compress::Light, Compress::Medium, Compress::Strong].map(Compress::qp), [20, 22, 24]);
        let (info, idx) = (info(2), index(2));
        let source = src(&info, Some(&idx));
        let e = edit(2);
        let s = Settings { compress: Some(Compress::Medium), ..Settings::default() };
        let job = plan(&e, &s, &source);
        let Job::Video(VideoJob { video: Video::Encode { size, bitrate, fps, qp, .. }, .. }) = job else { panic!("{job:?}") };
        assert_eq!((size, fps, qp), ((1920, 1080), None, Some(22)));
        assert_eq!(bitrate, 6_600_000, "the 12 Mbps source at its measured share");
        let half = plan(&e, &Settings { height: Some(540), ..s }, &source);
        assert!(matches!(half, Job::Video(VideoJob { video: Video::Encode { size: (960, 540), qp: Some(22), .. }, .. })));

        // a size limit or a custom bitrate saved alongside wins, as the setters would have had it
        let limited = plan(&e, &Settings { target_mb: Some(10), ..s }, &source);
        assert!(matches!(limited, Job::Video(VideoJob { video: Video::Encode { qp: None, .. }, .. })));
        let gif = plan(&e, &Settings { format: Format::Gif, ..s }, &source);
        assert!(matches!(gif, Job::Gif(_)));

        let mut s = Settings { kbps: Some(3000), ..Settings::default() };
        s.set_compress(Some(Compress::Strong));
        assert_eq!((s.compress, s.kbps, s.target_mb), (Some(Compress::Strong), None, None));
        s.set_target(Some(10));
        assert_eq!((s.compress, s.target_mb), (None, Some(10)));
        s.set_compress(Some(Compress::Light));
        assert_eq!((s.compress, s.target_mb), (Some(Compress::Light), None));
        s.set_kbps(Some(5000));
        assert_eq!((s.compress, s.kbps), (None, Some(5000)));
        s.set_compress(Some(Compress::Light));
        s.set_target(None);
        s.set_kbps(None);
        assert_eq!(s.compress, Some(Compress::Light), "clearing the others leaves it alone");
    }

    #[test]
    fn the_done_message_compares_with_the_source_stretch() {
        let info = info(1);
        let source = src(&info, None);
        assert_eq!(source.bytes_for(10 * S), 15_500_000);
        assert_eq!((source.bytes_for(40 * S), source.bytes_for(-S)), (31_000_000, 0));

        assert_eq!(saving_label(412_000_000, 190_000_000).unwrap(), "412 MB → 190 MB (−54%)");
        assert_eq!(saving_label(9_640_000, 420_000).unwrap(), "9.6 MB → 420 KB (−96%)");
        let same = saving_label(412_000_000, 410_000_000).unwrap();
        assert_eq!(same, "412 MB → 410 MB, about the same: the source was already compact");
        let not = "not smaller: the source was already compact";
        assert_eq!(saving_label(50_000_000, 50_000_000).unwrap(), format!("50 MB → 50 MB, {not}"));
        assert_eq!(saving_label(50_000_000, 64_000_000).unwrap(), format!("50 MB → 64 MB, {not}"));
        assert_eq!(saving_label(0, 1_000), None);
    }
}
