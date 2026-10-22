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
fn newest_pills_stack_at_the_bottom_up_to_three() {
    let style = Style::new(ACCENT, 1.0);
    let at = |t: i64, c: &str| key(t, None, Mods::default(), text(c));
    let keys = [at(0, "a"), at(10 * MS, "b"), at(20 * MS, "c"), at(30 * MS, "d")];
    let shown = pills(&keys, 40 * MS, &style);
    assert_eq!(shown.len(), 3, "only three stack");
    assert_eq!(shown.iter().map(|p| p.text.clone()).collect::<Vec<_>>(), ["d", "c", "b"], "newest first, oldest dropped");
    // The bottom pill (newest) sits lower than the ones above it.
    assert!(shown[0].rect.bottom() > shown[1].rect.bottom() && shown[1].rect.bottom() > shown[2].rect.bottom());
}

#[test]
fn a_burst_of_the_same_chord_collapses_with_a_count() {
    let style = Style::new(ACCENT, 1.0);
    let z = |t: i64| key(t, Some(t + MS), chord(true, false, false, false), text("Z"));
    let keys = [z(0), z(200 * MS), z(400 * MS)];
    let shown = pills(&keys, 500 * MS, &style);
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].text, "Ctrl+Z \u{00d7}3");

    // Far apart, they are separate pills again.
    let spaced = [z(0), z(10 * style.hold)];
    assert_eq!(pills(&spaced, 10 * style.hold + MS, &style).len(), 1, "the first has long gone");
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
