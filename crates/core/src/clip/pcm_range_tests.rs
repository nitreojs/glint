use super::{Lane, RAMP, frames};
use crate::clip::edit::{Edit, GainRange};
use crate::clip::math::{Range, SECOND};

#[test]
fn audio_range_gain_keeps_time_and_fades_across_buffer_boundaries() {
    let rate = 48_000;
    let mut edit = Edit::new(3 * SECOND, 2);
    let muted = Range { start: SECOND / 2, end: 2 * SECOND };
    let overlap = Range { start: SECOND, end: 3 * SECOND / 2 };
    assert!(edit.set_range_volume(1, muted, 0));
    assert!(edit.set_range_volume(1, overlap, 50));
    let gains = &edit.lanes[1].gains;
    assert_eq!(gains.len(), 3);
    assert!(edit.lanes[0].gains.is_empty());
    assert_eq!(gains[1], GainRange { range: overlap, volume: 50 });

    let count = frames(3 * SECOND, rate) as usize;
    let mix = |batch: usize| {
        let mut lane = Lane::new(0);
        lane.push(0, &vec![0.8; count * 2]);
        let mut out = vec![0.0; count * 2];
        for chunk in out.chunks_mut(batch * 2) {
            lane.take_shaped(chunk, &mut 0.5, 0.5, 0.0, gains, rate);
        }

        out
    };
    let out = mix(233);
    assert_eq!(out, mix(count));
    let sample = |at| out[frames(at, rate) as usize * 2];
    let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
    assert!(close(sample(SECOND / 4), 0.4));
    assert!(close(sample(3 * SECOND / 4), 0.0));
    assert!(close(sample(5 * SECOND / 4), 0.2));
    assert!(close(sample(9 * SECOND / 4), 0.4));
    assert!(close(sample(muted.start + RAMP / 2), 0.2) || (sample(muted.start + RAMP / 2) - 0.2).abs() < 0.002);
    assert!(edit.set_range_volume(1, muted, 100));
    assert!(edit.lanes[1].gains.is_empty());
}
