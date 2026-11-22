use glint_core::clip::edit::Speed;
use glint_core::clip::math::{Range, SECOND};

use super::{AudioSpan, PcmCursor};

#[test]
fn fractional_audio_stays_in_phase_across_decoder_and_output_chunks() {
    const SOURCE_START: i64 = 7_901;
    const OUTPUT_START: i64 = 13_007;
    const SOURCE_FRAMES: i64 = 2_053;

    let source = Range { start: SOURCE_START, end: SOURCE_START + SOURCE_FRAMES };
    let input: Vec<f32> = (0..SOURCE_FRAMES).flat_map(|i| [i as f32, -(i as f32)]).collect();
    for step in [50, 671, 1_000, 1_337, 16_000] {
        let output = Range {
            start: OUTPUT_START,
            end: OUTPUT_START + SOURCE_FRAMES * 1_000 / i64::from(step),
        };
        let span = AudioSpan::new(source, output, Speed::from_milli(step), SECOND as u32);
        let mut cursor = PcmCursor::default();
        cursor.reset(source.start);
        let mut scratch = Vec::new();
        let mut written = output.start;
        let mut reused_pair = false;

        assert_eq!(span.available(source.start), output.start);
        for ready in (source.start + 1..source.end).step_by(17).chain([source.end]) {
            let upto = span.available(ready);
            while written < upto {
                let to = upto.min(written + 1 + (written - output.start) % 11);
                let window = span.window(written, to);
                assert!(window.end <= ready);
                reused_pair |= window.start + 2 == cursor.next;
                let base = cursor.mix(window, &mut scratch, |end, fresh| {
                    let end = (end - source.start) as usize * 2;
                    fresh.copy_from_slice(&input[end - fresh.len()..end]);

                    Ok(())
                }).unwrap();

                for (at, [l, r]) in (written..to).zip(span.samples(&scratch, base, written, to)) {
                    let expected = ((at - output.start) as f64 * f64::from(step) / 1_000.0)
                        .min((SOURCE_FRAMES - 1) as f64) as f32;
                    assert!((l - expected).abs() <= 0.000_2, "{step} at {at}: {l} vs {expected}");
                    assert_eq!(r, -l, "{step} at {at}");
                }
                written = to;
            }
        }

        assert_eq!(written, output.end);
        if step == 671 {
            assert!(reused_pair);
        }
    }
}

#[test]
#[ignore = "needs ffmpeg on PATH and Windows Media Foundation"]
fn preview_audio_seeks_and_keeps_playing_when_one_track_ends() {
    use super::{Com, Gains, Source, media, pcm};

    let path = std::env::temp_dir().join(format!("glint-preview-audio-{}.mp4", std::process::id()));
    let output = std::process::Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-f", "lavfi", "-i", "color=size=64x64:rate=12:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=400:sample_rate=48000:duration=1",
            "-f", "lavfi", "-i", "sine=frequency=800:sample_rate=48000:duration=2",
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c:v", "libx264", "-threads:v", "1",
            "-c:a", "aac",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

    let _com = Com::init();
    let info = media::probe(&path).unwrap();
    let streams: Vec<_> = info.tracks.iter().map(|track| track.stream).collect();
    let gains = Gains::new(streams.len(), 1.0);
    let mut source = Source::open(&path, &streams).unwrap();
    let mut out = vec![0.0; source.rate as usize / 5 * 2];
    let mut mix = |at| {
        source.seek(at, &gains).unwrap();
        source.mix(pcm::frames(at, source.rate) + (out.len() / 2) as i64, &gains, &mut out).unwrap();

        [400.0, 800.0].map(|hz| {
            let (sin, cos) = out.iter().step_by(2).enumerate().fold((0.0, 0.0), |(sin, cos), (i, &s)| {
                let phase = std::f64::consts::TAU * hz * i as f64 / f64::from(source.rate);

                (sin + f64::from(s) * phase.sin(), cos + f64::from(s) * phase.cos())
            });

            sin.hypot(cos) / (out.len() / 2) as f64
        })
    };
    let start = mix(0);
    let late = mix(SECOND * 3 / 2);
    let back = mix(SECOND / 2);
    drop(source);
    let _ = std::fs::remove_file(&path);

    assert!(start[0] > 0.04 && start[1] > 0.04, "{start:?}");
    assert!(late[0] < 0.002 && late[1] > 0.04, "{late:?}");
    assert!(back[0] > 0.04 && back[1] > 0.04, "{back:?}");
}
