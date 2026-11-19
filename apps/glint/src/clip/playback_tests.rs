use glint_core::clip::edit::{Edit, Speed, TimeMap};
use glint_core::clip::math::SECOND;
use glint_core::config::AfterTrim;

use super::clicklane::Axis;
use super::player::{FrameAction, frame_action};
use super::timeline::View;
use super::{ClipApp, Play, Playback, Saved};

#[test]
fn output_clock_crosses_rate_changes_and_removed_ranges() {
    let mut edit = Edit::new(15 * SECOND, 0);
    for split in [3, 7, 10, 13] {
        assert!(edit.split(split * SECOND, 1));
    }
    assert!(edit.set_speed(1, Speed::from_milli(500)));
    assert!(edit.toggle_removed(2));
    assert!(edit.set_speed(3, Speed::from_milli(2_000)));
    edit.range.start = SECOND;
    edit.range.end = 14 * SECOND;
    let mut playback = Playback::new(Playback::from_source(edit.timed(), 0)).unwrap();

    let start = playback.advance(0);
    let slow = playback.advance(2 * SECOND);
    let slow_middle = playback.advance(4 * SECOND);
    let fast = playback.advance(10 * SECOND);
    let fast_middle = playback.advance(11 * SECOND);
    let normal = playback.advance(11 * SECOND + SECOND / 2);
    let end = playback.advance(20 * SECOND);

    assert_eq!(start, (SECOND, false, false));
    assert_eq!(slow, (3 * SECOND, false, false));
    assert_eq!(slow_middle, (4 * SECOND, false, false));
    assert_eq!(fast, (10 * SECOND, true, false));
    assert_eq!(fast_middle, (12 * SECOND, false, false));
    assert_eq!(normal, (13 * SECOND, false, false));
    assert_eq!(end, (14 * SECOND, false, true));
}

#[test]
fn exact_or_covered_seek_needs_no_lookahead() {
    let exact = frame_action(100, 100, 0, None, false);
    let covered = frame_action(105, 100, 10, None, false);

    assert_eq!(exact, FrameAction::Show);
    assert_eq!(covered, FrameAction::Show);
}

#[test]
fn catch_up_only_converts_the_unsuperseded_frame() {
    let no_duration = frame_action(110, 100, 0, None, false);
    let next_is_due = frame_action(110, 100, 10, Some(110), false);
    let gap = frame_action(110, 100, 5, Some(120), false);
    let last = frame_action(110, 100, 5, None, true);

    assert_eq!(no_duration, FrameAction::ReadAhead);
    assert_eq!(next_is_due, FrameAction::Advance);
    assert_eq!(gap, FrameAction::Show);
    assert_eq!(last, FrameAction::Show);
}

#[test]
fn video_only_reposition_keeps_the_audio_epoch_and_playback_clock() {
    let mut app = ClipApp::new(None, 1, AfterTrim::KeepBoth, String::new(), Saved::default(), [0; 3]);
    app.edit = Edit::new(10 * SECOND, 0);
    assert!(app.edit.set_speed(0, Speed::from_milli(2_000)));
    app.seek(SECOND, true);
    let play = Play::Running { at: std::time::Instant::now(), from: SECOND };
    app.play = play;
    let epoch = app.epoch;
    let (source, _, _) = app.playback.as_mut().unwrap().advance(SECOND);

    app.seek_video(source, true, source);

    assert_ne!(app.epoch, epoch);
    assert_eq!(app.playback.as_ref().unwrap().audio_epoch, epoch);
    assert!(app.play == play);
    assert_eq!(app.playback.as_mut().unwrap().advance(2 * SECOND).0, 5 * SECOND);
}

#[test]
fn seeking_inside_a_speed_piece_keeps_source_time_and_rejects_clock_jitter() {
    let mut edit = Edit::new(10 * SECOND, 0);
    assert!(edit.split(2 * SECOND, 1));
    assert!(edit.split(8 * SECOND, 1));
    assert!(edit.set_speed(1, Speed::from_milli(1_500)));
    let from = 5 * SECOND;
    let mut playback = Playback::new(Playback::from_source(edit.timed(), from)).unwrap();

    let start = playback.advance(0);
    let one_second_later = playback.advance(SECOND);
    let stale = playback.advance(SECOND - 1);
    let boundary = playback.advance(2 * SECOND);

    assert_eq!(start, (from, false, false));
    assert_eq!(one_second_later, (6 * SECOND + SECOND / 2, false, false));
    assert_eq!(stale, one_second_later);
    assert_eq!(boundary, (8 * SECOND, false, false));
}

#[test]
fn unrelated_splits_do_not_restart_preview_but_future_speed_edits_do() {
    let mut edit = Edit::new(10 * SECOND, 0);
    let mut playback = Playback::new(Playback::from_source(edit.timed(), 0)).unwrap();
    let (source, _, _) = playback.advance(2 * SECOND);
    assert!(edit.split(5 * SECOND, 1));

    let same = playback.matches(source, &Playback::from_source(edit.timed(), source));
    assert!(edit.set_speed(1, Speed::from_milli(2_000)));
    let different = playback.matches(source, &Playback::from_source(edit.timed(), source));

    assert!(same);
    assert!(!different);
}

#[test]
fn resetting_speeds_refreshes_running_preview_and_undo_restores_removed_piece_speed() {
    let mut app = ClipApp::new(None, 1, AfterTrim::KeepBoth, String::new(), Saved::default(), [0; 3]);
    app.edit = Edit::new(9 * SECOND, 0);
    assert!(app.edit.split(3 * SECOND, 1));
    assert!(app.edit.split(6 * SECOND, 1));
    assert!(app.edit.set_speed(0, Speed::from_milli(500)));
    assert!(app.edit.set_speed(1, Speed::from_milli(2_000)));
    assert!(app.edit.toggle_removed(1));
    app.seek(SECOND, true);
    let old_epoch = app.epoch;

    let changed = app.reset_piece_speeds();

    assert!(changed);
    assert!(app.play == Play::Starting);
    assert_eq!(app.pos, SECOND);
    assert_ne!(app.epoch, old_epoch);
    assert!(app.edit.segments.iter().all(|p| p.speed == Speed::NORMAL));

    let undone = app.undo.undo(&mut app.edit);
    app.refresh_playback();

    assert!(undone);
    assert_eq!(app.playback.as_ref().unwrap().pieces[0].speed, Speed::from_milli(500));
    assert_eq!(app.edit.segments[1].speed, Speed::from_milli(2_000));
    assert!(app.edit.segments[1].removed);
}

#[test]
fn the_retimed_axis_moves_and_reveals_a_source_position() {
    let mut app = ClipApp::new(None, 1, AfterTrim::KeepBoth, String::new(), Saved::default(), [0; 3]);
    let source_dur = 12 * SECOND;
    app.edit = Edit::new(source_dur, 0);
    assert!(app.edit.split(2 * SECOND, 1));
    assert!(app.edit.split(6 * SECOND, 1));
    assert!(app.edit.set_speed(1, Speed::from_milli(500)));
    let map = TimeMap::new(&app.edit);
    let axis = Axis {
        left: 100.0,
        width: 512.0,
        view: View::full(map.duration()),
        map: &map,
        source_dur,
        fps: 60.0,
        snapping: true,
    };

    let seam = axis.x(6 * SECOND);
    let grabbed = axis.x(5 * SECOND) + 8.0;
    let moved = axis.moved(5 * SECOND, grabbed, grabbed + 96.0);
    app.view = View { start: 2 * SECOND, span: 4 * SECOND };
    app.jump(moved);

    assert_eq!(seam, 420.0);
    assert_eq!(axis.t(seam), 6 * SECOND);
    assert_eq!(moved, 7 * SECOND);
    assert_eq!(app.pos, moved);
    assert_eq!(app.view.start, 9 * SECOND);
}
