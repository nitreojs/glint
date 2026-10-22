use super::*;

const ACCENT: [u8; 3] = [255, 0, 0];
/// A deterministic text width: one unit per character.
fn measure(s: &str) -> f32 {
    s.chars().count() as f32
}

fn text(s: &str) -> KeyLabel {
    KeyLabel::Text(s.into())
}

fn key(at: i64, hold: Option<i64>, mods: Mods, main: KeyLabel) -> Key {
    Key { at, hold, mods, main }
}

fn chord(ctrl: bool, alt: bool, shift: bool, win: bool) -> Mods {
    Mods { ctrl, alt, shift, win }
}

fn pills(keys: &[Key], t: i64, style: &Style) -> Vec<Pill> {
    state_at(keys, t, style, (1920.0, 1080.0), &measure)
}

#[test]
fn labels_read_as_chords_specials_and_characters() {
    let ctrl_shift_s = key(0, None, chord(true, false, true, false), text("S"));
    assert_eq!(label(&ctrl_shift_s), "Ctrl+Shift+S");
    assert_eq!(label(&key(0, None, chord(false, true, false, false), KeyLabel::Named(Named::Tab))), "Alt+Tab");
    assert_eq!(label(&key(0, None, chord(false, false, false, true), text("E"))), "Win+E");
    assert_eq!(label(&key(0, None, Mods::default(), KeyLabel::Named(Named::Esc))), "Esc");
    assert_eq!(label(&key(0, None, Mods::default(), text("a"))), "a");
    assert_eq!(label(&key(0, None, Mods::default(), KeyLabel::Named(Named::Left))), "\u{2190}");
    assert_eq!(label(&key(0, None, chord(true, true, false, true), KeyLabel::Func(5))), "Ctrl+Alt+Win+F5");
    assert_eq!(label(&key(0, None, chord(false, false, false, false), KeyLabel::Named(Named::PageUp))), "PgUp");
    assert!(chord(true, false, false, false).chord() && !chord(false, false, true, false).chord());
}

#[test]
fn auto_repeat_collapses_and_releases_pair_by_physical_key() {
    let mut log = Log::default();
    log.up(0x41, 5); // a release with no press is ignored
    log.down(0x41, 10, Mods::default(), text("a"));
    log.down(0x41, 15, Mods::default(), text("a")); // the OS repeating the held key
    log.down(0x41, 20, Mods::default(), text("a"));
    log.down(0x1B, 25, Mods::default(), KeyLabel::Named(Named::Esc));
    log.up(0x41, 40);
    log.up(0x1B, 30);
    assert_eq!(
        log.keys(),
        [
            key(10, Some(40), Mods::default(), text("a")),
            key(25, Some(30), Mods::default(), KeyLabel::Named(Named::Esc)),
        ]
    );
    // A press re-used after release starts a new event, not a repeat.
    log.down(0x41, 50, chord(true, false, false, false), text("a"));
    assert_eq!(log.keys().len(), 3);

    let video = log.finish(12);
    assert_eq!(video.iter().map(|k| (k.at, k.hold)).collect::<Vec<_>>(), [(13, Some(18)), (38, None)]);
}

#[test]
fn a_pill_rises_holds_then_fades() {
    let style = Style::new(ACCENT, 1.0);
    let keys = [key(0, Some(100 * MS), Mods::default(), text("A"))];
    assert!(pills(&keys, -1, &style).is_empty(), "nothing before the key");

    let appearing = &pills(&keys, 0, &style)[0];
    assert_eq!(appearing.alpha, 0.0, "fades in from nothing");
    assert_eq!(appearing.text, "A");

    let risen = &pills(&keys, style.rise, &style)[0];
    assert!((risen.alpha - 1.0).abs() < 1e-5, "{}", risen.alpha);

    // hold counts from the release (100 ms), then a 200 ms fade.
    let fs = 100 * MS + style.hold;
    assert!((pills(&keys, fs, &style)[0].alpha - 1.0).abs() < 1e-5);
    let mid = &pills(&keys, fs + style.fade / 2, &style)[0];
    assert!((mid.alpha - 0.5).abs() < 1e-3, "{}", mid.alpha);
    assert!(pills(&keys, fs + style.fade, &style).is_empty(), "gone after the fade");

    // a key still held at the end holds from its press time.
    let held = [key(0, None, Mods::default(), text("B"))];
    assert_eq!(pills(&held, style.hold, &style).len(), 1);
    assert!(pills(&held, style.hold + style.fade, &style).is_empty());
}

#[test]
fn typing_a_word_batches_into_one_row_that_backspace_edits() {
    let style = Style::new(ACCENT, 1.0);
    let p = |t: i64, c: &str| key(t, Some(t + MS), Mods::default(), text(c));
    // h e l l o typed within a second → one cap "hello".
    let word = [p(0, "h"), p(10 * MS, "e"), p(20 * MS, "l"), p(30 * MS, "l"), p(40 * MS, "o")];
    let shown = pills(&word, 50 * MS, &style);
    assert_eq!(shown.len(), 1, "one row, one strip cap");
    assert_eq!(shown[0].text, "hello");
    // Space joins the run as a literal space; a key typed before `t` isn't shown yet.
    let typing = [p(0, "h"), p(10 * MS, "i"), key(20 * MS, Some(21 * MS), Mods::default(), KeyLabel::Named(Named::Space)), p(30 * MS, "u")];
    assert_eq!(pills(&typing, 25 * MS, &style)[0].text, "hi ");
    assert_eq!(pills(&typing, 35 * MS, &style)[0].text, "hi u");
    // Backspace edits the live row instead of adding a cap.
    let edited = [p(0, "c"), p(10 * MS, "a"), p(20 * MS, "t"), key(30 * MS, Some(31 * MS), Mods::default(), KeyLabel::Named(Named::Backspace)), p(40 * MS, "b")];
    assert_eq!(pills(&edited, 50 * MS, &style)[0].text, "cab");
    // A gap longer than the burst starts a fresh row (but short enough that the first is still up).
    let gap = style.burst + style.burst / 5;
    let spaced = [p(0, "x"), p(gap, "y")];
    let two = pills(&spaced, gap + MS, &style);
    assert_eq!(two.iter().map(|p| p.text.clone()).collect::<Vec<_>>(), ["y", "x"], "two rows, newest first");
}

#[test]
fn a_chord_or_special_is_its_own_row_of_caps_and_breaks_typing() {
    let style = Style::new(ACCENT, 1.0);
    // Ctrl+Shift+S is one row of three caps.
    let chorded = [key(0, Some(MS), chord(true, false, true, false), text("S"))];
    let caps = pills(&chorded, 10 * MS, &style);
    assert_eq!(caps.iter().map(|p| p.text.clone()).collect::<Vec<_>>(), ["Ctrl", "Shift", "S"]);
    assert!(caps[0].rect.right() <= caps[1].rect.left() && caps[1].rect.right() <= caps[2].rect.left(), "laid out left to right");
    assert!(caps.iter().all(|c| (c.alpha - caps[0].alpha).abs() < 1e-6), "the whole row shares one alpha");

    // Typing, then a shortcut: the text row stays, the chord is a separate row below it.
    let p = |t: i64, c: &str| key(t, Some(t + MS), Mods::default(), text(c));
    let mix = [p(0, "h"), p(10 * MS, "i"), key(20 * MS, Some(21 * MS), chord(true, false, false, false), text("S"))];
    let rows = pills(&mix, 30 * MS, &style);
    assert_eq!(rows[0].text, "Ctrl", "the newest row (the chord) is at the bottom");
    assert!(rows.iter().any(|r| r.text == "hi"), "the typed row is kept");

    // An Enter during typing closes the row as a trailing cap.
    let sent = [p(0, "h"), p(10 * MS, "i"), key(20 * MS, Some(21 * MS), Mods::default(), KeyLabel::Named(Named::Enter))];
    let one = pills(&sent, 30 * MS, &style);
    assert_eq!(one.iter().map(|p| p.text.clone()).collect::<Vec<_>>(), ["hi", "Enter"], "same row: the text then the Enter cap");
}

#[test]
fn rows_stack_newest_at_the_bottom_up_to_three() {
    let style = Style::new(ACCENT, 1.0);
    let special = |t: i64, n: Named| key(t, Some(t + MS), Mods::default(), KeyLabel::Named(n));
    let keys = [special(0, Named::Esc), special(10 * MS, Named::Tab), special(20 * MS, Named::Enter), special(30 * MS, Named::Delete)];
    let shown = pills(&keys, 40 * MS, &style);
    assert_eq!(shown.len(), 3, "only three rows stack");
    assert_eq!(shown.iter().map(|p| p.text.clone()).collect::<Vec<_>>(), ["Delete", "Enter", "Tab"], "newest first, oldest dropped");
    assert!(shown[0].rect.bottom() > shown[1].rect.bottom() && shown[1].rect.bottom() > shown[2].rect.bottom());
}

#[test]
fn a_long_typed_row_keeps_its_tail() {
    let style = Style::new(ACCENT, 1.0);
    let typed: Vec<Key> = (0..50).map(|i| key(i * MS, Some(i * MS + MS), Mods::default(), text(&((b'a' + (i % 26) as u8) as char).to_string()))).collect();
    let text = pills(&typed, 60 * MS, &style).remove(0).text;
    assert_eq!(text.chars().count(), style.keep, "clipped to the last {} characters", style.keep);
    let want: String = (50 - style.keep..50).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    assert_eq!(text, want);
}

#[test]
fn positions_anchor_the_stack_left_centre_and_right() {
    let base = Style::new(ACCENT, 1.0);
    let keys = [key(0, None, Mods::default(), text("W"))];
    let region = (1000.0, 800.0);
    let one = |s: &Style| state_at(&keys, s.rise, s, region, &measure).pop().unwrap().rect;
    let margin = base.margin * region.1;

    let left = one(&base.with(Position::Left));
    assert!((left.left() - margin).abs() < 1e-3);
    let right = one(&base.with(Position::Right));
    assert!((right.right() - (region.0 - margin)).abs() < 1e-3);
    let centre = one(&base.with(Position::Centre));
    assert!((centre.center().x - region.0 / 2.0).abs() < 1e-3);
    assert!((right.bottom() - (region.1 - margin)).abs() < 1e-3, "sits a margin off the bottom");
}

#[test]
fn frames_are_needed_only_while_a_pill_animates() {
    let style = Style::new(ACCENT, 1.0);
    let keys = [key(1000 * MS, Some(3000 * MS), Mods::default(), text("A"))];
    let moves = |t: i64| moving(&keys, t, &style);
    assert!(!moves(999 * MS));
    assert!(moves(1000 * MS) && moves(1000 * MS + style.rise - 1));
    assert!(!moves(1000 * MS + style.rise) && !moves(3000 * MS));
    let fs = 3000 * MS + style.hold;
    assert!(moves(fs) && moves(fs + style.fade - 1));
    assert!(!moves(fs + style.fade));
    assert!(!moving(&[], 0, &style));
}

fn write_mp4(path: &Path) {
    let atom = |kind: &[u8], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
    let moov = atom(b"moov", &atom(b"mvhd", &[0; 100]));
    std::fs::write(path, [atom(b"ftyp", b"isom"), atom(b"mdat", &[7; 64]), moov].concat()).unwrap();
}

#[test]
fn keys_survive_a_trip_through_the_file() {
    let path = std::env::temp_dir().join(format!("glint_keys_{}.mp4", std::process::id()));
    write_mp4(&path);
    assert_eq!(load(&path), Ok(None));
    let payload = Payload {
        version: VERSION,
        scale: 1.25,
        keys: vec![
            key(0, Some(1_000_000), chord(true, false, true, false), text("S")),
            key(2_500_000, None, Mods::default(), KeyLabel::Named(Named::Enter)),
            key(3_000_000, Some(3_100_000), Mods::default(), KeyLabel::Func(2)),
        ],
    };
    save(&path, &payload).unwrap();
    let back = load(&path);
    let raw = mp4::movie_box(&path, &BOX).unwrap().unwrap();
    let json = String::from_utf8(raw).unwrap();
    assert_eq!(
        json,
        r#"{"version":1,"scale":1.25,"keys":[{"at":0,"hold":1000000,"mods":{"ctrl":true,"alt":false,"shift":true,"win":false},"main":{"text":"S"}},{"at":2500000,"hold":null,"mods":{"ctrl":false,"alt":false,"shift":false,"win":false},"main":{"named":"enter"}},{"at":3000000,"hold":3100000,"mods":{"ctrl":false,"alt":false,"shift":false,"win":false},"main":{"func":2}}]}"#
    );

    let newer = json.replace("\"version\":1", "\"version\":2");
    mp4::set_movie_box(&path, &BOX, newer.as_bytes()).unwrap();
    let refused = load(&path);
    std::fs::remove_file(&path).unwrap();

    assert_eq!(back, Ok(Some(payload)));
    assert_eq!(refused, Err("keystrokes were saved by a newer glint (format 2)".into()));
}
