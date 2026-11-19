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
