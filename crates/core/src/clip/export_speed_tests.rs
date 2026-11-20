use super::tests::{edit, index, info, src};
use super::{
    AAC_BYTES_PER_SECOND, Compress, Format, Job, MB, Piece, Reencode, SILENCE_BYTES_PER_SECOND,
    Settings, Source, Video, VideoJob, bitrate_for, estimate, muxed, plan, plan_why,
};
use crate::clip::clicks::Clicks;
use crate::clip::edit::{Edit, Speed, TimeMap, TimedRange, Undo};
use crate::clip::index::{Index, Samples};
use crate::clip::keys::Keys;
use crate::clip::math::{Range, SECOND as S, gif_estimate, gif_frames};
use crate::record::{clicks, keys};

const RETIMED_LEN: i64 = 9 * S + 4 * S / 3;

fn r(start: i64, end: i64) -> Range {
    Range { start, end }
}

fn timed(start: i64, end: i64, speed: Speed) -> TimedRange {
    TimedRange { range: r(start, end), speed }
}

fn video_rate(job: Job) -> (u32, Option<u32>) {
    let Job::Video(VideoJob { video: Video::Encode { bitrate, qp, .. }, .. }) = job else { panic!("{job:?}") };

    (bitrate, qp)
}

fn mixed_edit() -> Edit {
    let mut edit = edit(1);
    for at in [5, 7, 11, 13, 17] {
        assert!(edit.split(at * S, 1));
    }
    assert!(edit.set_speed(0, Speed::from_milli(500)));
    assert!(edit.set_speed(1, Speed::from_milli(500)));
    assert!(edit.toggle_removed(2));
    assert!(edit.set_speed(3, Speed::from_milli(2_000)));
    assert!(edit.set_speed(4, Speed::from_milli(1_500)));
    assert!(edit.set_speed(5, Speed::from_milli(500)));

    edit
}

#[test]
fn conversions_floor_signed_deltas_and_keep_short_pieces_nonempty() {
    let rates = [500, 671, 1_000, 1_500, 2_000].map(Speed::from_milli);
    assert_eq!(rates.map(Speed::label), ["0.5×", "0.671×", "1×", "1.5×", "2×"]);
    assert_eq!(rates.map(|s| s.source_delta(3)), [1, 2, 3, 4, 6]);
    assert_eq!(rates.map(|s| s.source_delta(-1)), [-1, -1, -1, -2, -2]);
    assert_eq!(rates.map(|s| s.output_delta(3)), [6, 4, 3, 2, 1]);
    assert_eq!(rates.map(|s| s.output_delta(-1)), [-2, -2, -1, -1, -1]);
    assert_eq!(timed(1, 2, Speed::from_milli(2_000)).len(), 1);
    assert_eq!(timed(1, 2, Speed::from_milli(1_500)).len(), 1);
    assert_eq!(timed(1, 1, Speed::from_milli(2_000)).len(), 0);

    let large = i64::MAX - i64::MAX % 3;
    assert_eq!(Speed::from_milli(1_500).output_delta(large), large / 3 * 2);
    assert_eq!(Speed::from_milli(2_000).source_delta(i64::MAX), i64::MAX);
    assert_eq!(Speed::from_milli(50).output_delta(i64::MIN), i64::MIN);
}

#[test]
fn rates_from_lengths_round_to_thousandths_and_clamp() {
    let rate = Speed::from_lengths(6_715, 10_000);

    assert_eq!(rate.milli(), 672);
    assert_eq!(Speed::from_lengths(6_714, 10_000).milli(), 671);
    assert_eq!(Speed::from_milli(1_010).label(), "1.01×");
    assert_eq!(Speed::from_milli(1_001).label(), "1.001×");
    assert_eq!(Speed::default(), Speed::NORMAL);
    assert_eq!(Speed::from_milli(0).milli(), 50);
    assert_eq!(Speed::from_milli(u32::MAX).milli(), 16_000);
    assert_eq!(Speed::from_lengths(1, i64::MAX).milli(), 50);
    assert_eq!(Speed::from_lengths(i64::MAX, 1).milli(), 16_000);
    assert_eq!(Speed::from_lengths(1, 0).milli(), 16_000);
    assert_eq!(Speed::from_lengths(0, 1), Speed::NORMAL);
}

#[test]
fn fractional_stretch_keeps_source_bounds_and_ripples_later_coordinates() {
    let source_len = 671 * S / 100;
    let piece_end = 5 * S + source_len;
    let mut edit = Edit::new(20 * S, 1);
    assert!(edit.split(5 * S, 1));
    assert!(edit.split(piece_end, 1));
    let before = TimeMap::new(&edit);
    let speed = Speed::from_lengths(source_len, 10 * S);
    assert!(edit.set_speed(1, speed));
    let after = TimeMap::new(&edit);
    let shift = 10 * S - source_len;

    assert_eq!(speed.milli(), 671);
    assert_eq!(edit.splits().collect::<Vec<_>>(), [5 * S, piece_end]);
    assert_eq!(after.to_edit(5 * S), before.to_edit(5 * S));
    assert_eq!(after.to_edit(piece_end), 15 * S);
    assert_eq!(after.to_edit(17 * S), before.to_edit(17 * S) + shift);
    assert_eq!(after.duration(), 20 * S + shift);
    assert_eq!(after.to_source(15 * S), piece_end);
    assert_eq!(after.to_source(10 * S), 5 * S + source_len / 2);
    assert_eq!(after.to_source(after.duration() + S), 21 * S);
    assert_eq!(after.to_edit(21 * S), after.duration() + S);
    assert_eq!(after.to_edit(-S), -S);
    assert_eq!(after.to_source(-S), -S);

    edit.range = r(5 * S, piece_end);
    let info = info(1);
    let source = src(&info, None);
    let video = plan(&edit, &Settings::default(), &source);
    let gif = plan(&edit, &Settings { format: Format::Gif, ..Settings::default() }, &source);
    let Job::Gif(gif) = gif else { panic!() };

    assert_eq!(video.len(), 10 * S);
    assert_eq!(gif.len(), 10 * S);
    assert_eq!(gif.source_time(5 * S), Some((0, 5 * S + source_len / 2)));
}

#[test]
fn full_axis_preserves_trimmed_and_removed_footage_at_its_stored_rate() {
    let mut edit = mixed_edit();
    assert!(edit.toggle_removed(2));
    assert!(edit.set_speed(2, Speed::from_milli(2_000)));
    assert!(edit.toggle_removed(2));
    let shaded = TimeMap::new(&edit);
    assert!(edit.toggle_removed(2));
    edit.range = r(0, 20 * S);
    let restored = TimeMap::new(&edit);

    for (source, edited) in [
        (0, 0),
        (3 * S, 6 * S),
        (7 * S, 14 * S),
        (9 * S, 15 * S),
        (11 * S, 16 * S),
        (13 * S, 17 * S),
        (17 * S, 17 * S + 8 * S / 3),
        (20 * S, 23 * S + 8 * S / 3),
    ] {
        assert_eq!(shaded.to_edit(source), edited);
        assert_eq!(shaded.to_source(edited), source);
        assert_eq!(restored.to_edit(source), edited);
    }
    assert_eq!(shaded.duration(), restored.duration());
}

#[test]
fn timeline_round_trips_monotonically_with_at_most_one_conversion_step_of_error() {
    let mut edit = Edit::new(1_200, 0);
    for at in [1, 11, 403, 901, 1_101] {
        assert!(edit.split(at, 1));
    }
    for (i, milli) in [16_000, 50, 671, 1_125, 9_368].into_iter().enumerate() {
        assert!(edit.set_speed(i, Speed::from_milli(milli)));
    }
    let map = TimeMap::new(&edit);

    for source in -20..=1_220 {
        let edited = map.to_edit(source);

        assert!(map.to_edit(source - 1) <= edited);
        assert!(source.abs_diff(map.to_source(edited)) <= 16);
    }
    for edited in -20..=map.duration() + 20 {
        let source = map.to_source(edited);

        assert!(map.to_source(edited - 1) <= source);
        assert!((0..=20).contains(&(edited - map.to_edit(source))));
    }
}

#[test]
fn timeline_keeps_wide_internal_boundaries_when_the_duration_saturates() {
    let mut edit = Edit::new(i64::MAX, 0);
    assert!(edit.split(i64::MAX / 2, 1));
    assert!(edit.set_speed(0, Speed::from_milli(50)));
    assert!(edit.set_speed(1, Speed::from_milli(16_000)));
    let map = TimeMap::new(&edit);

    assert_eq!(map.duration(), i64::MAX);
    assert_eq!(map.to_edit(i64::MAX / 2), i64::MAX);
    assert_eq!(map.to_source(i64::MAX), i64::MAX / 20);
}

#[test]
fn splitting_inherits_speed_and_merging_requires_kept_pieces_at_the_same_speed() {
    let mut edit = Edit::new(10 * S, 0);
    let half = Speed::from_milli(500);
    let double = Speed::from_milli(2_000);
    assert!(!edit.set_speed(1, half));
    assert!(!edit.set_speed(0, Speed::NORMAL));
    assert!(edit.set_speed(0, half));
    assert!(edit.split(5 * S, 1));
    assert_eq!(edit.segments.iter().map(|s| s.speed).collect::<Vec<_>>(), [half, half]);

    let mut undo = Undo::default();
    let before = edit.clone();
    assert!(edit.set_speed(1, double));
    undo.record(before, &edit);
    assert!(!edit.merge_kept(1));
    assert!(undo.undo(&mut edit));
    assert_eq!(edit.segments[1].speed, half);
    assert!(undo.redo(&mut edit));
    assert_eq!(edit.segments[1].speed, double);

    assert!(edit.toggle_removed(1));
    assert!(!edit.set_speed(1, half));
    assert!(!edit.merge_kept(1));
    assert!(edit.toggle_removed(1));
    assert_eq!(edit.segments[1].speed, double);
    assert!(edit.set_speed(1, half));
    assert!(edit.merge_kept(1));
    assert_eq!(edit.timed(), [timed(0, 10 * S, half)]);
}

#[test]
fn playback_clips_to_trims_and_coalesces_equal_adjacent_rates_only() {
    let edit = mixed_edit();
    let slow = timed(3 * S, 7 * S, Speed::from_milli(500));
    let fast = timed(11 * S, 13 * S, Speed::from_milli(2_000));
    let tail = timed(13 * S, 15 * S, Speed::from_milli(1_500));
    let pieces = edit.timed();

    assert_eq!(pieces, [slow, fast, tail]);
    assert_eq!(edit.kept(), [r(3 * S, 7 * S), r(11 * S, 15 * S)]);
    assert_eq!(edit.playback_piece(0), Some(slow));
    assert_eq!(edit.playback_piece(5 * S), Some(slow));
    assert_eq!(edit.playback_piece(7 * S - 1), Some(slow));
    assert_eq!(edit.playback_piece(7 * S), Some(fast));
    assert_eq!(edit.playback_piece(9 * S), Some(fast));
    assert_eq!(edit.playback_piece(13 * S), Some(tail));
    assert_eq!(edit.playback_piece(15 * S), None);
}

#[test]
fn video_retimes_picture_and_sound_and_sizes_the_edited_duration() {
    let (info, idx) = (info(1), index(1));
    let source = src(&info, Some(&idx));
    let mut edit = mixed_edit();
    edit.add_empty();
    let settings = Settings::default();
    let (job, why) = plan_why(&edit, &settings, &source);
    let Job::Video(video) = &job else { panic!() };
    let Video::Encode { bitrate, qp, .. } = video.video else { panic!() };

    assert_eq!(why, [Reencode::Speed]);
    assert_eq!(job.len(), RETIMED_LEN);
    assert_eq!((bitrate, qp), (10_558_592, None), "12 MB of retained video over the geometric mean of source and edited duration");
    assert_eq!(video.pieces.iter().map(|p| p.len()).collect::<Vec<_>>(), [8 * S, S, 4 * S / 3]);
    assert_eq!(
        video.pieces.iter().map(|p| (p.lead, p.start, p.end, p.key, p.speed)).collect::<Vec<_>>(),
        [
            (3 * S, 3 * S, 7 * S, 3 * S, Speed::from_milli(500)),
            (11 * S, 11 * S, 13 * S, 11 * S, Speed::from_milli(2_000)),
            (13 * S, 13 * S, 15 * S, 13 * S, Speed::from_milli(1_500)),
        ],
    );
    assert!(video.tracks.iter().all(|t| !video.copies(t)));
    let payload = bitrate as u64 * RETIMED_LEN as u64 / (8 * S as u64);
    let audio = AAC_BYTES_PER_SECOND * RETIMED_LEN as u64 / S as u64
        + SILENCE_BYTES_PER_SECOND * RETIMED_LEN as u64 / S as u64;
    assert_eq!(estimate(&job, &source), muxed(payload + audio));

    let compress = Settings { compress: Some(Compress::Medium), ..settings };
    let compressed = plan(&edit, &compress, &source);
    assert_eq!(video_rate(compressed), (6_600_000, Some(22)));

    let limited = plan(&edit, &Settings { target_mb: Some(10), ..compress }, &source);
    assert_eq!(video_rate(limited), (bitrate_for(10 * MB, RETIMED_LEN, audio), None));

    let custom = plan(
        &edit,
        &Settings { kbps: Some(25_000), target_mb: Some(10), ..compress },
        &source,
    );
    assert_eq!(video_rate(custom), (25_000_000, None));
}

#[test]
fn auto_retime_budgets_only_retained_indexed_video_bytes() {
    let info = info(1);
    let video = Samples::new(
        (0..600)
            .map(|i| {
                let bytes = match i / 30 {
                    3..=6 => 4_000,
                    11..=12 => 16_000,
                    13..=14 => 8_000,
                    _ => 40_000,
                };

                (i * S / 30, bytes, i % 30 == 0)
            })
            .collect(),
    );
    let idx = Index { video, ..index(1) };
    let source = src(&info, Some(&idx));
    let job = plan(&mixed_edit(), &Settings::default(), &source);

    assert_eq!(video_rate(job), (1_689_375, None), "retained video totals 1.92 MB");
}

#[test]
fn auto_retime_counts_a_frame_held_across_cut_points_only_once() {
    let mut info = info(0);
    info.fps = 1.0;
    let video = Samples::new(vec![(3 * S, 100_000, true), (4 * S, 200_000, false), (5 * S, 600_000, false)]);
    let idx = Index { video, audio: Vec::new() };
    let source = src(&info, Some(&idx));
    let mut edit = Edit::new(info.duration, 0);
    edit.range = r(7 * S / 2, 5 * S);
    assert!(edit.split(9 * S / 2, 1));
    assert!(edit.set_speed(0, Speed::from_milli(500)));
    let job = plan(&edit, &Settings::default(), &source);

    assert_eq!(job.len(), 5 * S / 2);
    assert_eq!(video_rate(job), (1_239_355, None), "the trim shows the first two frames, not the one at its end");
}

#[test]
fn auto_retime_without_video_samples_prorates_the_file_by_retained_source_time() {
    let mut info = info(3);
    info.duration += S / 2;
    let empty = Index::default();
    let edit = mixed_edit();
    let settings = Settings::default();
    for idx in [None, Some(&empty)] {
        let audio = 3 * AAC_BYTES_PER_SECOND * 41 / 2;
        let source = Source { bytes: 2_050_000 + audio, ..src(&info, idx) };
        let job = plan(&edit, &settings, &source);
        assert_eq!(video_rate(job), (703_906, None), "8 seconds at 100 KB/s of video");

        let compact = Source { bytes: 1_025_000, ..source };
        let job = plan(&edit, &settings, &compact);
        assert_eq!(video_rate(job), (351_953, None), "the audio estimate exceeds this file's entire bitrate");
    }
}

#[test]
fn auto_retime_respects_the_sane_bitrate_floor_and_ceiling() {
    let mut info = info(0);
    info.fps = 60.0;
    let dense = index(0);
    let sparse = Index { video: Samples::new(vec![(0, 1, true)]), audio: Vec::new() };
    let mut edit = edit(0);
    assert!(edit.set_speed(0, Speed::from_milli(16_000)));
    let settings = Settings::default();
    let capped = plan(&edit, &settings, &src(&info, Some(&dense)));
    let floored = plan(&edit, &settings, &src(&info, Some(&sparse)));

    assert_eq!(video_rate(capped), (19_000_000, None));
    assert_eq!(video_rate(floored), (300_000, None));
}

#[test]
fn rates_on_removed_or_fully_trimmed_pieces_leave_normal_copy_intact() {
    let (info, idx) = (info(1), index(1));
    let source = src(&info, Some(&idx));
    let mut edit = edit(1);
    for at in [2, 5, 8, 11] {
        assert!(edit.split(at * S, 1));
    }
    assert!(edit.set_speed(0, Speed::from_milli(500)));
    assert!(edit.set_speed(3, Speed::from_milli(2_000)));
    assert!(edit.toggle_removed(3));

    let (job, why) = plan_why(&edit, &Settings::default(), &source);
    let Job::Video(video) = job else { panic!() };
    assert!(why.is_empty());
    assert_eq!(video.video, Video::Copy);
    assert_eq!(video.pieces, [
        Piece { lead: 2 * S, start: 3 * S, end: 8 * S, key: 2 * S, speed: Speed::NORMAL },
        Piece { lead: 10 * S, start: 11 * S, end: 15 * S, key: 10 * S, speed: Speed::NORMAL },
    ]);
    assert!(video.copies(&video.tracks[0]));
}

#[test]
fn gif_uses_exact_edited_boundaries_but_filters_overlays_in_source_time() {
    let (info, idx) = (info(1), index(1));
    let source = src(&info, Some(&idx));
    let mut edit = mixed_edit();
    let source_times = [S, 6 * S, 8 * S, 12 * S, 14 * S, 16 * S];
    edit.clicks = Some(Clicks::new(&clicks::Payload {
        version: 1,
        scale: 1.0,
        clicks: source_times.into_iter().map(|down| clicks::Click {
            button: clicks::Button::Left,
            down,
            up: Some(down + S / 10),
            x: 10.0,
            y: 10.0,
        }).collect(),
    }));
    edit.keys = Some(Keys::new(&keys::Payload {
        version: 1,
        scale: 1.0,
        keys: source_times.into_iter().map(|at| keys::Key {
            at,
            hold: Some(at + S / 10),
            mods: keys::Mods::default(),
            main: keys::KeyLabel::Text("s".into()),
        }).collect(),
    }));
    let settings = Settings { format: Format::Gif, ..Settings::default() };
    let (job, why) = plan_why(&edit, &settings, &source);
    let Job::Gif(gif) = &job else { panic!() };

    assert_eq!(why, [Reencode::Gif]);
    assert_eq!(gif.len(), RETIMED_LEN);
    assert_eq!(gif_frames(gif.len(), gif.fps), 155);
    assert_eq!(estimate(&job, &source), gif_estimate(gif.size, 155));
    assert_eq!(gif.source_time(0), Some((0, 3 * S)));
    assert_eq!(gif.source_time(1), Some((0, 3 * S)));
    assert_eq!(gif.source_time(8 * S - 1), Some((0, 7 * S - 1)));
    assert_eq!(gif.source_time(8 * S), Some((1, 11 * S)));
    assert_eq!(gif.source_time(8 * S + 1), Some((1, 11 * S + 2)));
    assert_eq!(gif.source_time(9 * S), Some((2, 13 * S)));
    assert_eq!(gif.source_time(RETIMED_LEN - 1), Some((2, 15 * S - 2)));
    assert_eq!(gif.source_time(RETIMED_LEN), None);
    assert_eq!(gif.source_time(RETIMED_LEN + 1), None);
    let clicks = &gif.burn.as_ref().unwrap().clicks;
    let keys = &gif.keys.as_ref().unwrap().keys;
    assert_eq!(clicks.iter().map(|c| c.down).collect::<Vec<_>>(), [6 * S, 12 * S, 14 * S]);
    assert_eq!(keys.iter().map(|k| k.at).collect::<Vec<_>>(), [6 * S, 12 * S, 14 * S]);
}
