use super::{Category, Event, Level, Row};

#[test]
fn shortened_replay_keeps_both_durations_and_the_output_path() {
    let event = Event::ReplayShortened {
        id: 24,
        duration_ms: 42_000,
        requested_s: 60,
        elapsed_ms: 1_234,
        path: r"E:\replays\game failed.mp4".into(),
    };
    let line = format!("[2026-11-21_07-16-03] {}", event.line("save #24 done in 1.2 s"));
    let row = Row::parse(&line);

    assert_eq!(row.event, Some(event));
    assert_eq!((row.date, row.time, row.message), ("2026-11-21", "07-16-03", "save #24 done in 1.2 s"));
    assert_eq!((row.category, row.level), (Category::Replay, Level::Warning));
}

#[test]
fn legacy_and_future_logs_stay_readable_without_classifying_filenames_as_errors() {
    let success = Row::parse(r"[2026-11-21_07-16-03] save #24 done in 1.2 s: E:\game failed.mp4");
    let error = Row::parse("[2026-11-21_07-16-03] auto-clip: save #24 failed: access denied");
    let future = Row::parse("[2026-11-21_07-16-03] new event\t@diagnostic {\"event\":\"future\"}");

    assert_eq!((success.level, error.level), (Level::Success, Level::Error));
    assert_eq!(future.message, "new event");
    assert!(future.raw.ends_with(r#"{"event":"future"}"#));
}
