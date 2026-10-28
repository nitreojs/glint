//! An encoder effort the GPU keeps up with at the region's size and rate.
//!
//! Hardware encoders map the effort onto about three presets, and the slower ones can't take
//! every frame of a big region at a high rate: the frames queue up until the recording drops
//! them. Measured on an RTX 5060 Ti at 3440×1440 (scrolling text, constant QP 20), frames per
//! second alone / with afterglow encoding the same at effort 20 alongside:
//!
//! | effort | H.264             | HEVC              |
//! |--------|-------------------|-------------------|
//! | 0–28   | 320–380 / 220–285 | 315–365 / 240–260 |
//! | 30–58  | 160–200 / 125–145 | 150–155 / 120–140 |
//! | 60–100 | 105–120 / 90–95   | ~66 / ~60         |
//!
//! At the same QP the faster presets mostly cost file size (H.264 at effort 25: ~30% over 100).
//! Changing the effort while encoding does nothing (NVIDIA's encoder takes the value and keeps
//! its preset), so it's chosen up front from the pixel rate.

use super::Codec;

/// `effort`, or the effort of a faster preset (50 or 25) when `w`×`h` at `fps` is more pixels
/// per second than the slower ones take with another encoder alongside, less a margin.
pub fn fit(effort: u32, codec: Codec, w: u32, h: u32, fps: f64) -> u32 {
    let mpx = w as f64 * h as f64 * fps / 1e6;
    // megapixels per second the slowest and the middle presets take with afterglow alongside
    // (H.264: ~450 and ~620; HEVC: ~295 and ~595)
    let (slow, middle) = match codec {
        Codec::H264 => (400.0, 540.0),
        Codec::Hevc => (250.0, 500.0),
    };
    match mpx {
        m if m > middle => effort.min(25),
        m if m > slow => effort.min(50),
        _ => effort,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_or_slow_regions_keep_the_effort() {
        assert_eq!(fit(100, Codec::H264, 1654, 964, 200.0), 100);
        assert_eq!(fit(100, Codec::H264, 1920, 1080, 144.0), 100);
        assert_eq!(fit(100, Codec::H264, 3440, 1440, 60.0), 100);
        assert_eq!(fit(100, Codec::Hevc, 1920, 1080, 120.0), 100);
    }

    #[test]
    fn big_regions_at_high_rates_get_a_faster_preset() {
        assert_eq!(fit(100, Codec::H264, 3440, 1440, 144.0), 25);
        assert_eq!(fit(100, Codec::H264, 2560, 1440, 144.0), 50);
        assert_eq!(fit(100, Codec::Hevc, 1920, 1080, 144.0), 50);
        assert_eq!(fit(100, Codec::Hevc, 2560, 1440, 144.0), 25);
    }

    #[test]
    fn a_lower_effort_is_never_raised() {
        assert_eq!(fit(20, Codec::H264, 3440, 1440, 144.0), 20);
        assert_eq!(fit(40, Codec::H264, 2560, 1440, 144.0), 40);
        assert_eq!(fit(0, Codec::Hevc, 3840, 2160, 240.0), 0);
    }
}
