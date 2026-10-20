use std::path::{Path, PathBuf};

pub(super) const SECOND: i64 = 10_000_000;

pub(super) const GIF_FPS: [u32; 4] = [10, 15, 24, 30];
pub(super) const GIF_WIDTHS: [u32; 4] = [1280, 960, 640, 480];
pub(super) const GIF_WARN_BYTES: u64 = 50 * 1024 * 1024;
// measured with this exporter at 15 fps: 0.031 bytes per pixel per frame on a glint screen recording, 0.063 on ffmpeg's testsrc.
const GIF_BYTES_PER_PIXEL: f64 = 0.06;

pub(super) fn frame_len(fps: f64) -> i64 {
    if fps > 0.0 { ((SECOND as f64 / fps).round() as i64).max(1) } else { SECOND / 30 }
}

pub(super) fn frame_time(i: i64, fps: f64) -> i64 {
    (i.max(0) as f64 * SECOND as f64 / fps).round() as i64
}

pub(super) fn snap(t: i64, fps: f64) -> i64 {
    frame_time((t.max(0) as f64 * fps / SECOND as f64).round() as i64, fps)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Range {
    pub start: i64,
    pub end: i64,
}

impl Range {
    pub(super) fn full(duration: i64) -> Self {
        Range { start: 0, end: duration.max(0) }
    }

    pub(super) fn len(self) -> i64 {
        self.end - self.start
    }

    pub(super) fn is_full(self, duration: i64) -> bool {
        self.start <= 0 && self.end >= duration
    }

    pub(super) fn set_start(&mut self, t: i64, min_len: i64) {
        self.start = t.clamp(0, (self.end - min_len).max(0));
    }

    pub(super) fn set_end(&mut self, t: i64, duration: i64, min_len: i64) {
        self.end = t.clamp((self.start + min_len).min(duration), duration);
    }
}

pub(super) fn fmt_time(hns: i64) -> String {
    let ms = hns.max(0) / 10_000;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{ms:03}") } else { format!("{m}:{s:02}.{ms:03}") }
}

pub(super) fn unique_sibling(src: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    let dir = src.parent().unwrap_or(Path::new("."));
    let mut path = dir.join(format!("{stem}{suffix}.{ext}"));
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{stem}{suffix}_{n}.{ext}"));
    }
    path
}

pub(super) fn gif_size(src: (u32, u32), width: Option<u32>) -> (u32, u32) {
    match width {
        Some(w) if w < src.0 && src.0 > 0 => (w, ((src.1 as f64 * w as f64 / src.0 as f64).round() as u32).max(1)),
        _ => src,
    }
}

pub(super) fn gif_frames(len: i64, fps: u32) -> u64 {
    if len <= 0 || fps == 0 { 0 } else { ((len as i128 * fps as i128 + SECOND as i128 - 1) / SECOND as i128) as u64 }
}

pub(super) fn gif_tick(start: i64, k: u64, fps: u32) -> i64 {
    start + (k as i128 * SECOND as i128 / fps as i128) as i64
}

pub(super) fn gif_delay(k: u64, fps: u32) -> u16 {
    let cs = |k: u64| (k as f64 * 100.0 / fps as f64).round() as u64;
    (cs(k + 1) - cs(k)) as u16
}

pub(super) fn gif_estimate(size: (u32, u32), frames: u64) -> u64 {
    (size.0 as f64 * size.1 as f64 * frames as f64 * GIF_BYTES_PER_PIXEL) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_map_to_times_and_back() {
        for fps in [24.0, 30.0, 60.0, 143.998, 144.0, 200.0] {
            for i in [0, 1, 2, 59, 60, 1000, 123_457] {
                let (t, third) = (frame_time(i, fps), frame_len(fps) / 3);
                assert_eq!(snap(t + third, fps), t, "fps {fps} frame {i}");
                assert_eq!(snap(t - third, fps), t, "fps {fps} frame {i}");
            }
        }
        assert_eq!(frame_time(60, 60.0), SECOND);
        assert_eq!(snap(SECOND - 1, 60.0), SECOND);
        assert_eq!(snap(-5, 60.0), 0);
        assert_eq!(frame_len(144.0), 69_444);
        assert_eq!(frame_len(0.0), SECOND / 30);
    }

    #[test]
    fn trim_range_stays_inside_the_clip_and_at_least_a_frame_long() {
        let (dur, f) = (5 * SECOND, frame_len(60.0));
        let mut r = Range::full(dur);
        r.set_start(-SECOND, f);
        assert_eq!(r.start, 0);
        r.set_start(2 * SECOND, f);
        assert_eq!(r.start, 2 * SECOND);
        r.set_end(SECOND, dur, f);
        assert_eq!(r.end, 2 * SECOND + f);
        r.set_end(9 * SECOND, dur, f);
        assert_eq!(r.end, dur);
        r.set_start(dur, f);
        assert_eq!(r.start, dur - f);
        assert_eq!(r.len(), f);
        assert!(!r.is_full(dur));
        assert!(Range::full(dur).is_full(dur));

        let mut tiny = Range::full(f / 2);
        tiny.set_end(0, f / 2, f);
        assert_eq!(tiny, Range::full(f / 2));
    }

    #[test]
    fn times_format_with_milliseconds() {
        assert_eq!(fmt_time(0), "0:00.000");
        assert_eq!(fmt_time(12_345_678), "0:01.234");
        assert_eq!(fmt_time(61 * SECOND + 50_000), "1:01.005");
        assert_eq!(fmt_time(3_725 * SECOND), "1:02:05.000");
        assert_eq!(fmt_time(-1), "0:00.000");
    }

    #[test]
    fn outputs_sit_next_to_the_source_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("glint-clip-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("glint_2026-10-20_18-45-27.mp4");
        assert_eq!(unique_sibling(&src, "_trim", "mp4"), dir.join("glint_2026-10-20_18-45-27_trim.mp4"));
        std::fs::write(dir.join("glint_2026-10-20_18-45-27_trim.mp4"), b"").unwrap();
        std::fs::write(dir.join("glint_2026-10-20_18-45-27_trim_2.mp4"), b"").unwrap();
        assert_eq!(unique_sibling(&src, "_trim", "mp4"), dir.join("glint_2026-10-20_18-45-27_trim_3.mp4"));
        assert_eq!(unique_sibling(&src, "", "gif"), dir.join("glint_2026-10-20_18-45-27.gif"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn gif_frames_cover_the_range_with_exact_delays() {
        assert_eq!(gif_frames(3 * SECOND, 15), 45);
        assert_eq!(gif_frames(3 * SECOND + 1, 15), 46);
        assert_eq!(gif_frames(0, 15), 0);
        assert_eq!(gif_tick(SECOND, 3, 30), SECOND + SECOND / 10);
        for fps in GIF_FPS {
            let total: u64 = (0..fps as u64 * 4).map(|k| gif_delay(k, fps) as u64).sum();
            assert_eq!(total, 400, "fps {fps}");
        }
        assert_eq!((0..3).map(|k| gif_delay(k, 30)).collect::<Vec<_>>(), [3, 4, 3]);
    }

    #[test]
    fn gif_size_keeps_the_aspect_and_never_upscales() {
        assert_eq!(gif_size((3440, 1440), Some(640)), (640, 268));
        assert_eq!(gif_size((1920, 1080), Some(480)), (480, 270));
        assert_eq!(gif_size((400, 300), Some(640)), (400, 300));
        assert_eq!(gif_size((1920, 1080), None), (1920, 1080));
    }

    #[test]
    fn gif_estimate_grows_with_pixels_and_frames() {
        assert_eq!(gif_estimate((640, 360), 0), 0);
        assert_eq!(gif_estimate((640, 360), 45), 622_080);
        assert!(gif_estimate((1280, 720), 15 * 90) > GIF_WARN_BYTES);
        assert!(gif_estimate((960, 540), 15 * 30) < GIF_WARN_BYTES);
    }
}
