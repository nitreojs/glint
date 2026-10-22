use super::*;

const RED: [u8; 3] = [255, 0, 0];

fn click(button: Button, down: i64, up: Option<i64>) -> Click {
    Click { button, down, up, x: 10.5, y: 20.5 }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn ring(shapes: &[Shape]) -> Option<Shape> {
    shapes.iter().copied().find(|s| s.radius > 0.0 || s.width < 8.0)
}

fn dot(shapes: &[Shape]) -> Option<Shape> {
    shapes.iter().copied().find(|s| s.radius == 0.0 && s.width == 8.0)
}

#[test]
fn the_ring_eases_out_to_its_radius_while_fading() {
    let style = Style::new(RED, 1.0);
    let clicks = [click(Button::Left, 0, Some(100 * MS))];
    assert!(state_at(&clicks, -1, &style).is_empty());

    let start = state_at(&clicks, 0, &style);
    let r = ring(&start).unwrap();
    assert_eq!((r.x, r.y, r.radius, r.width, r.color), (10.5, 20.5, 0.0, 3.0, [1.0, 0.0, 0.0, 1.0]));

    let r = ring(&state_at(&clicks, 100 * MS, &style)).unwrap();
    let p = 100.0f32 / 450.0;
    assert!(close(r.radius, 28.0 * (1.0 - (1.0 - p).powi(3))), "{}", r.radius);
    assert!(close(r.color[3], 1.0 - p));

    let r = ring(&state_at(&clicks, 225 * MS, &style)).unwrap();
    assert!(close(r.radius, 24.5) && close(r.color[3], 0.5), "{r:?}");
    let late = ring(&state_at(&clicks, 449 * MS, &style)).unwrap();
    assert!(late.radius > 27.99 && late.color[3] < 0.003, "{late:?}");
    assert!(state_at(&clicks, 450 * MS, &style).is_empty());
}

#[test]
fn the_dot_stays_while_held_and_fades_after_the_release() {
    let style = Style::new(RED, 1.0);
    let clicks = [click(Button::Left, 0, Some(2000 * MS))];
    let held = state_at(&clicks, 1000 * MS, &style);
    assert_eq!(held, [Shape { x: 10.5, y: 20.5, radius: 0.0, width: 8.0, color: [1.0, 0.0, 0.0, 1.0] }]);
    assert_eq!(dot(&state_at(&clicks, 2000 * MS, &style)).unwrap().color[3], 1.0);
    assert!(close(dot(&state_at(&clicks, 2060 * MS, &style)).unwrap().color[3], 0.5));
    assert!(state_at(&clicks, 2120 * MS, &style).is_empty());

    let quick = [click(Button::Left, 0, Some(50 * MS))];
    let s = state_at(&quick, 80 * MS, &style);
    assert!(ring(&s).is_some() && close(dot(&s).unwrap().color[3], 0.75), "{s:?}");
    assert!(dot(&state_at(&quick, 200 * MS, &style)).is_none());

    let never = [click(Button::Left, 0, None)];
    assert_eq!(dot(&state_at(&never, 3_600_000 * MS, &style)).unwrap().color[3], 1.0);
}

#[test]
fn each_button_has_its_colour() {
    let style = Style::new([0x2F, 0x7B, 0xE5], 1.0);
    let accent = [0x2F as f32 / 255.0, 0x7B as f32 / 255.0, 0xE5 as f32 / 255.0];
    let at = |b: Button| ring(&state_at(&[click(b, 0, None)], 0, &style)).unwrap().color;
    assert_eq!(at(Button::Left), [accent[0], accent[1], accent[2], 1.0]);
    assert_eq!(at(Button::Right), [0.88, 0.88, 0.88, 1.0]);
    assert_eq!(at(Button::Middle), [accent[0], accent[1], accent[2], 0.55]);
    let fading = ring(&state_at(&[click(Button::Middle, 0, None)], 225 * MS, &style)).unwrap();
    assert!(close(fading.color[3], 0.275));
}

#[test]
fn sizes_follow_the_monitor_dpi() {
    let style = Style::new(RED, 1.5);
    assert_eq!((style.radius, style.stroke, style.dot), (42.0, 4.5, 12.0));
    let held = state_at(&[click(Button::Left, 0, None)], 900 * MS, &style);
    assert_eq!(held.len(), 1);
    assert_eq!((held[0].radius, held[0].width), (0.0, 12.0));
}

#[test]
fn overlapping_clicks_each_draw() {
    let style = Style::new(RED, 1.0);
    let clicks = [click(Button::Left, 0, Some(30 * MS)), click(Button::Right, 100 * MS, None)];
    let s = state_at(&clicks, 120 * MS, &style);
    assert_eq!(s.len(), 4);
    assert_eq!(s.iter().filter(|s| s.color[..3] == [0.88; 3]).count(), 2);
}

#[test]
fn frames_are_needed_only_while_something_animates() {
    let style = Style::new(RED, 1.0);
    let clicks = [click(Button::Left, 1000 * MS, Some(3000 * MS))];
    let moves = |t: i64| moving(&clicks, t * MS, &style);
    assert!(!moves(999));
    assert!(moves(1000) && moves(1449));
    assert!(!moves(1450) && !moves(2999));
    assert!(moves(3000) && moves(3119));
    assert!(!moves(3120));
    assert!(!moving(&[], 0, &style));
    assert!(!moving(&[click(Button::Left, 0, None)], 500 * MS, &style));
}

#[test]
fn desktop_positions_map_into_the_region() {
    let across = Rect { x: -1000, y: 400, w: 2000, h: 1300 };
    assert_eq!(to_region((-1000, 400), across), Some((0.5, 0.5)));
    assert_eq!(to_region((0, 1000), across), Some((1000.5, 600.5)));
    assert_eq!(to_region((999, 1699), across), Some((1999.5, 1299.5)));
    for outside in [(1000, 400), (-1001, 500), (0, 399), (0, 1700), (-5000, -5000)] {
        assert_eq!(to_region(outside, across), None, "{outside:?}");
    }
    let portrait = Rect { x: 3440, y: -200, w: 1440, h: 2560 };
    let region = Rect { x: 3500, y: 0, w: 800, h: 1600 };
    assert!(region.intersect(&portrait) == Some(region));
    assert_eq!(to_region((3500, 1599), region), Some((0.5, 1599.5)));
    assert_eq!(to_region((3499, 10), region), None);
}

#[test]
fn the_log_pairs_presses_with_releases() {
    let mut log = Log::default();
    log.release(Button::Left, 5);
    log.press(Button::Left, 10, 1.5, 2.5);
    log.press(Button::Right, 12, 3.5, 4.5);
    log.release(Button::Left, 20);
    log.press(Button::Middle, 30, 5.5, 6.5);
    log.press(Button::Middle, 40, 7.5, 8.5);
    log.release(Button::Right, 8);
    assert_eq!(
        log.clicks(),
        [
            Click { button: Button::Left, down: 10, up: Some(20), x: 1.5, y: 2.5 },
            Click { button: Button::Right, down: 12, up: Some(12), x: 3.5, y: 4.5 },
            Click { button: Button::Middle, down: 30, up: Some(40), x: 5.5, y: 6.5 },
            Click { button: Button::Middle, down: 40, up: None, x: 7.5, y: 8.5 },
        ]
    );
    let video = log.finish(11);
    assert_eq!(video.iter().map(|c| (c.down, c.up)).collect::<Vec<_>>(), [(1, Some(1)), (19, Some(29)), (29, None)]);
}

fn write_mp4(path: &Path) {
    let atom = |kind: &[u8], body: &[u8]| [&((body.len() + 8) as u32).to_be_bytes()[..], kind, body].concat();
    let moov = atom(b"moov", &atom(b"mvhd", &[0; 100]));
    std::fs::write(path, [atom(b"ftyp", b"isom"), atom(b"mdat", &[7; 64]), moov].concat()).unwrap();
}

#[test]
fn clicks_survive_a_trip_through_the_file() {
    let path = std::env::temp_dir().join(format!("glint_clicks_{}.mp4", std::process::id()));
    write_mp4(&path);
    assert_eq!(load(&path), Ok(None));
    let payload = Payload {
        version: VERSION,
        scale: 1.25,
        clicks: vec![click(Button::Left, 0, Some(1_000_000)), click(Button::Middle, 2_500_000, None)],
    };
    save(&path, &payload).unwrap();
    let back = load(&path);
    let raw = mp4::movie_box(&path, &BOX).unwrap().unwrap();
    let json = String::from_utf8(raw).unwrap();
    assert_eq!(
        json,
        r#"{"version":1,"scale":1.25,"clicks":[{"button":"left","down":0,"up":1000000,"x":10.5,"y":20.5},{"button":"middle","down":2500000,"up":null,"x":10.5,"y":20.5}]}"#
    );

    let newer = json.replace("\"version\":1", "\"version\":2");
    mp4::set_movie_box(&path, &BOX, newer.as_bytes()).unwrap();
    let refused = load(&path);
    mp4::set_movie_box(&path, &BOX, b"{\"version\":1,\"scale\":1,\"clicks\":[],\"later\":true}").unwrap();
    let extended = load(&path);
    std::fs::remove_file(&path).unwrap();

    assert_eq!(back, Ok(Some(payload)));
    assert_eq!(refused, Err("clicks were saved by a newer glint (format 2)".into()));
    assert_eq!(extended, Ok(Some(Payload { version: 1, scale: 1.0, clicks: Vec::new() })));
}
