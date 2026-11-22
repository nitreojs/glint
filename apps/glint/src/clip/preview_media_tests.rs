use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use glint_core::clip::{index, math::SECOND};
use glint_media::clip::media::{self, Com};

use super::player::{Cmd, NO_CLOCK, Player};
use super::scan::{Found, Scan};
use super::wave::{BUCKET, Waves};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn wait_for<T>(mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "preview worker timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[ignore = "needs ffmpeg on PATH and Windows Media Foundation"]
fn editor_media_preview_covers_long_clips_and_reuses_exact_frames() {
    let fixture = Fixture(std::env::temp_dir().join(format!("glint-preview-{}.mp4", std::process::id())));
    let output = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=60:duration=40",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=12",
            "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=40",
            "-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000:duration=40",
            "-f", "lavfi", "-i", "sine=frequency=220:sample_rate=48000:duration=40",
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:a", "-map", "4:a",
            "-c:v", "libx264", "-preset", "ultrafast", "-g", "120", "-sc_threshold", "0", "-threads:v", "4", "-c:a", "aac",
        ])
        .arg(&fixture.0)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let _com = Com::init();
    let info = media::probe(&fixture.0).unwrap();
    let index = index::read(&fixture.0).unwrap();
    let ctx = egui::Context::default();
    let player = Player::start(fixture.0.clone(), info.size, ctx.clone());
    let at = 29 * SECOND + SECOND / 3 + 7_000;
    let seek = |t, epoch| {
        let start = Instant::now();
        player.send(Cmd::Seek { t, epoch, play: false, clock: NO_CLOCK });
        let frame = wait_for(|| player.poll()).unwrap();
        assert_eq!(frame.epoch, epoch);
        assert!(frame.ts <= t && t < frame.ts + frame.dur, "frame at {} for {t}", frame.ts);

        (frame, start.elapsed())
    };
    seek(0, 0);
    let (first, cold) = seek(at, 1);
    let (other, _) = seek(8 * SECOND + SECOND / 5, 2);
    let (again, warm) = seek(at, 3);
    assert_ne!(first.image.pixels, other.image.pixels);
    assert!(Arc::ptr_eq(&first.image, &again.image));
    drop(player);

    let start = Instant::now();
    let scan = Scan::start(fixture.0.clone(), info.size, info.duration, &index.video.keys, ctx.clone());
    let mut sparse = 0;
    let overview = wait_for(|| match scan.poll()? {
        Found::Overview(t, _) => {
            sparse += 1;
            (t >= 35 * SECOND).then(|| start.elapsed())
        }
        Found::Thumb(..) => panic!("detailed scan started before the last quarter had a thumbnail"),
        Found::Cut(_) => None,
    });
    assert!(sparse <= 16);
    drop(scan);

    let start = Instant::now();
    let scan = Scan::start(fixture.0.clone(), info.size, 0, &[], ctx.clone());
    let sequential = wait_for(|| match scan.poll()? {
        Found::Thumb(t, _) if t >= 35 * SECOND => Some(start.elapsed()),
        _ => None,
    });
    drop(scan);

    let streams: Vec<_> = info.tracks.iter().map(|track| track.stream).collect();
    assert_eq!(streams.len(), 4);
    let bucket = |seconds| (seconds * SECOND / BUCKET) as usize;
    let measure_waves = |sections| {
        let start = Instant::now();
        let waves = Waves::start_sections(fixture.0.clone(), streams.clone(), info.duration, sections, ctx.clone());
        let mut peaks = vec![Vec::new(); streams.len()];
        let late = wait_for(|| {
            waves.poll(&mut peaks);
            (peaks.iter().filter(|p| p.get(bucket(35)).is_some_and(|&v| v > 50)).count() == 3)
                .then(|| start.elapsed())
        });
        let completed = wait_for(|| {
            waves.poll(&mut peaks);
            (peaks.iter().filter(|p| p.get(bucket(39)).is_some_and(|&v| v > 50)).count() == 3
                && peaks.iter().all(|p| p.get(100..900).is_some_and(|r| r.iter().all(|&v| v > 50)))
                && peaks.iter().filter(|p| p.get(bucket(11)).is_some_and(|&v| v > 50)).count() == 4
                && peaks.iter().filter(|p| p.get(100..bucket(39)).is_some_and(|r| r.iter().all(|&v| v > 50))).count() == 3)
                .then(|| start.elapsed())
        });
        assert_eq!(peaks.iter().filter(|p| p.len() < bucket(13)).count(), 1);

        (late, completed)
    };
    let (wave_sequential, all_sequential) = measure_waves(1);
    let (wave_parallel, all_parallel) = measure_waves(4);
    eprintln!("late waveform across three active tracks: {wave_sequential:?} sequential, {wave_parallel:?} parallel");
    eprintln!("continuous waveform across sections: {all_sequential:?} sequential, {all_parallel:?} parallel");

    eprintln!("late filmstrip: {sequential:?} sequential, {overview:?} overview; exact seek: {cold:?} cold, {warm:?} cached");
}
