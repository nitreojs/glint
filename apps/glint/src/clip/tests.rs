use eframe::App;
use eframe::egui::{Event, PointerButton, Pos2};
use glint_core::clip::export::Format;
use glint_core::clip::info::Track;
use glint_core::clip::index::Samples;
use glint_core::clip::math::SECOND;
use glint_core::clip::keys::KeySize;
use glint_core::record::clicks::{Button, Click, Payload};
use glint_core::record::keys;

use super::*;

const SCREEN: egui::Vec2 = vec2(WINDOW[0], WINDOW[1]);

fn info(tracks: usize, clicks: Option<Payload>, keys: Option<keys::Payload>) -> Info {
    let names = ["desktop", "spotifast.exe", "mic: HyperX QuadCast"];
    Info {
        duration: 42 * SECOND,
        size: (1920, 1080),
        fps: 60.0,
        codec: None,
        tracks: (0..tracks).map(|k| Track { stream: k as u32 + 1, name: names[k % 3].into() }).collect(),
        clicks,
        keys,
    }
}

/// 60 fps over 42 s, a keyframe every 2 s; sound in 1/4 s packets.
fn index(tracks: usize) -> Index {
    let video = Samples::new((0..42 * 60).map(|i| (i * SECOND / 60, 40_000, i % 120 == 0)).collect());
    let audio = (0..tracks).map(|_| Samples::new((0..42 * 4).map(|i| (i * SECOND / 4, 5_000, true)).collect())).collect();
    Index { video, audio }
}

struct Harness {
    ctx: egui::Context,
    app: ClipApp,
    frame: eframe::Frame,
    time: f64,
    texts: Vec<(String, Rect)>,
    rects: Vec<(Rect, Color32)>,
    /// Files held over the window, every frame, as winit reports a drag.
    hovered: Vec<PathBuf>,
    /// Files dropped with the next frame.
    dropped: Vec<PathBuf>,
}

impl Harness {
    fn new(tracks: usize) -> Harness {
        Harness::build(tracks, None, None)
    }

    fn with_clicks(tracks: usize, clicks: Option<Payload>) -> Harness {
        Harness::build(tracks, clicks, None)
    }

    fn with_keys(tracks: usize, keys: Option<keys::Payload>) -> Harness {
        Harness::build(tracks, None, keys)
    }

    fn build(tracks: usize, clicks: Option<Payload>, keys: Option<keys::Payload>) -> Harness {
        Harness::opened(info(tracks, clicks, keys), None)
    }

    fn opened(info: Info, index: Option<Index>) -> Harness {
        let path = PathBuf::from("C:/nowhere/glint_2026-10-21_04-36.mp4");
        Harness::start(Some(Opened { path, info: Ok(info), index }))
    }

    /// The editor on `file`, or on its start screen without one.
    fn start(file: Option<Opened>) -> Harness {
        let ctx = egui::Context::default();
        glint_ui::theme::fonts(&ctx);
        let cfg = Config::default();
        let app = ClipApp::new(file, cfg.effort, cfg.upload_destination.trim().to_string(), Saved::default(), [0xE5, 0x40, 0x2F]);
        let mut h = Harness {
            ctx,
            app,
            frame: eframe::Frame::_new_kittest(),
            time: 0.0,
            texts: Vec::new(),
            rects: Vec::new(),
            hovered: Vec::new(),
            dropped: Vec::new(),
        };
        h.app.hevc = true;
        h.settle();
        h
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.016;
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), SCREEN)),
            time: Some(self.time),
            events,
            hovered_files: self.hovered.iter().map(|p| egui::HoveredFile { path: Some(p.clone()), ..Default::default() }).collect(),
            dropped_files: self.dropped.drain(..).map(glint_ui::window::dropped_file).collect(),
            ..Default::default()
        };
        let (app, frame) = (&mut self.app, &mut self.frame);
        let out = self.ctx.run_ui(input, |ui| app.ui(ui, frame));
        self.texts = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::epaint::Shape::Text(t) => Some((t.galley.text().to_string(), Rect::from_min_size(t.pos, t.galley.size()))),
                _ => None,
            })
            .collect();
        self.rects = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::epaint::Shape::Rect(r) => Some((r.rect.intersect(c.clip_rect), r.fill)),
                _ => None,
            })
            .collect();
    }

    fn settle(&mut self) {
        for _ in 0..30 {
            self.frame(Vec::new());
        }
    }

    fn has(&self, text: &str) -> bool {
        self.texts.iter().any(|(t, _)| t == text)
    }

    fn starts(&self, prefix: &str) -> Option<String> {
        self.texts.iter().find(|(t, _)| t.starts_with(prefix)).map(|(t, _)| t.clone())
    }

    fn at(&self, text: &str) -> Pos2 {
        self.texts.iter().find(|(t, _)| t == text).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("no {text:?}"))
    }

    fn click(&mut self, at: Pos2) {
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![Event::PointerMoved(at), button(true)]);
        self.frame(vec![button(false)]);
        self.settle();
    }

    fn key(&mut self, key: Key, pressed: bool) {
        self.chord(key, Modifiers::NONE, pressed);
    }

    fn chord(&mut self, key: Key, modifiers: Modifiers, pressed: bool) {
        self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers }]);
    }

    fn right_click(&mut self, at: Pos2) {
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed, modifiers: Default::default() };
        self.frame(vec![Event::PointerMoved(at), button(true)]);
        self.frame(vec![button(false)]);
        self.settle();
    }

    fn drag(&mut self, from: Pos2, to: Pos2) {
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![Event::PointerMoved(from), button(from, true)]);
        for k in 1..=8 {
            self.frame(vec![Event::PointerMoved(from + (to - from) * k as f32 / 8.0)]);
        }
        self.frame(vec![button(to, false)]);
        self.settle();
    }

    fn muted(&self) -> Vec<bool> {
        self.app.edit.lanes.iter().map(|l| l.muted).collect()
    }

    fn lanes(&self) -> Vec<String> {
        let tracks = &self.app.info().unwrap().tracks;
        self.app.edit.lanes.iter().map(|l| l.name(tracks).to_string()).collect()
    }

    fn rect_of(&self, text: &str) -> Rect {
        self.texts.iter().find(|(t, _)| t == text).map(|(_, r)| *r).unwrap_or_else(|| panic!("no {text:?}"))
    }
}

#[test]
fn the_export_panel_shows_only_the_rows_that_apply() {
    let mut h = Harness::new(3);
    for row in ["export", "as", "size", "resolution", "original", "≤ 10 MB", "≤ 25 MB", "≤ 50 MB", "1080p (original)", "save"] {
        assert!(h.has(row), "{row}");
    }
    assert!(h.has("combine sound into one track"));
    assert!(h.has("frame rate") && h.has("60 fps (original)") && !h.has("reset"));
    assert!(h.has("more options") && !h.has("bitrate") && !h.has("codec"));
    assert!(h.has("1920×1080 · 60 fps · 0:42 · 3 sound tracks"));
    assert!(h.has("drag the red edges to trim · ") && h.has("all keys"));
    assert!(h.starts("≈ ").is_some());
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · 3 sound tracks"));
    assert!(h.texts.iter().all(|(t, _)| !t.contains("instantly")));
    assert!(h.has("re-encodes ·") && h.has("can't copy this file"), "no sample index to copy by");

    h.app.set_muted(1, true);
    h.settle();
    assert!(h.has("· muted, not exported"));
    assert!(h.has("combine sound into one track"));
    h.app.set_muted(2, true);
    h.settle();
    assert!(!h.has("combine sound into one track"));
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · 1 sound track"));
    assert_eq!(h.muted(), [false, true, true]);
    h.click(h.at("mute all"));
    assert_eq!(h.muted(), [true, true, true]);
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · no sound"));
    h.click(h.at("unmute all"));
    assert_eq!(h.muted(), [false, false, false]);

    h.click(h.at("gif"));
    assert_eq!(h.app.saved.export.format, Format::Gif);
    assert!(h.has("frame rate") && h.has("15 fps") && h.has("save gif"));
    assert!(!h.has("more options") && !h.has("60 fps (original)"));
    assert!(!h.has("combine sound into one track"));
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · no sound (gif)"));

    h.app.edit.crop = Some(Crop { x: 0, y: 0, w: 1280, h: 720 });
    h.settle();
    assert!(h.has("1280×720") && h.has("reset") && h.has("1280×720 (original)"));
    h.click(h.at("reset"));
    assert_eq!(h.app.edit.crop, None);
    assert!(!h.has("reset") && h.has("1080p (original)"));

    h.click(h.at("≤ 25 MB"));
    assert_eq!(h.app.saved.export.target_mb, Some(25));
}

#[test]
fn more_options_hold_bitrate_and_codec_and_a_bitrate_replaces_the_size_limit() {
    let mut h = Harness::new(2);
    h.click(h.at("more options"));
    assert!(h.app.saved.more);
    assert!(h.has("bitrate") && h.has("auto") && h.has("codec") && h.has("same as source (h.264)"));

    h.click(h.at("≤ 10 MB"));
    assert_eq!(h.app.saved.export.target_mb, Some(10));
    assert!(h.starts("· ").is_some_and(|t| t.ends_with(" Mbps")), "the size limit's bitrate shows next to auto");

    h.click(h.at("… Mbps"));
    h.frame(vec![Event::Text("3".into())]);
    h.settle();
    assert_eq!((h.app.saved.export.kbps, h.app.saved.export.target_mb), (Some(3000), None));
    assert!(h.starts("· ").is_none());
    assert_eq!(h.starts("≈ ").as_deref(), Some("≈ 18 MB"));

    h.click(h.at("≤ 25 MB"));
    assert_eq!((h.app.saved.export.kbps, h.app.saved.export.target_mb), (None, Some(25)));
    assert!(h.app.custom_mbps.is_empty());

    h.click(h.at("… Mbps"));
    h.frame(vec![Event::Text("999".into())]);
    h.settle();
    assert_eq!((h.app.saved.export.kbps, h.app.saved.export.target_mb), (None, Some(25)), "out of range stays auto");

    h.click(h.at("same as source (h.264)"));
    h.click(h.at("hevc"));
    assert_eq!(h.app.saved.export.codec, export::CodecChoice::Hevc);

    h.click(h.at("more options"));
    assert!(!h.app.saved.more);
    assert!(!h.has("bitrate"));
}

#[test]
fn a_single_track_and_no_track_read_naturally() {
    let h = Harness::new(1);
    assert!(h.has("1920×1080 · 60 fps · 0:42 · 1 sound track"));
    assert!(!h.has("combine sound into one track"));
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · with sound"));
    let h = Harness::new(0);
    assert!(!h.has("sound · muted tracks won't be in the file"));
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · no sound"));
}

#[test]
fn space_plays_on_release_unless_the_hold_panned() {
    let mut h = Harness::new(2);
    let over = pos2(300.0, 250.0);
    h.frame(vec![Event::PointerMoved(over)]);
    h.key(Key::Space, true);
    assert!(h.app.play == Play::Paused);
    h.key(Key::Space, false);
    assert!(h.app.play != Play::Paused);
    h.key(Key::Space, true);
    h.key(Key::Space, false);
    assert!(h.app.play == Play::Paused);

    h.key(Key::Num1, true);
    h.settle();
    assert!(!h.app.viewer.fitted);
    let before = h.app.viewer.target();
    h.key(Key::Space, true);
    let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() };
    h.frame(vec![button(over, true)]);
    for k in 1..=4 {
        h.frame(vec![Event::PointerMoved(over + vec2(15.0 * k as f32, 10.0 * k as f32))]);
    }
    h.frame(vec![button(over + vec2(60.0, 40.0), false)]);
    h.key(Key::Space, false);
    assert!(h.app.play == Play::Paused, "a pan doesn't toggle playback");
    assert_ne!(h.app.viewer.target().offset, before.offset);
    h.key(Key::Num0, true);
    h.settle();
    assert!(h.app.viewer.fitted);
}

#[test]
fn crop_mode_opens_with_c_and_leaves_with_enter() {
    let mut h = Harness::new(0);
    h.key(Key::C, true);
    h.settle();
    assert!(h.app.cropping.is_some());
    assert!(h.has("16:9") && h.has("done") && h.has("1920×1080"));
    h.click(h.at("1:1"));
    assert_eq!(h.app.edit.crop, Some(Crop { x: 420, y: 0, w: 1080, h: 1080 }));
    h.key(Key::Enter, true);
    h.settle();
    assert!(h.app.cropping.is_none());
    assert!(h.has("1080×1080 (original)") && h.has("reset"));
    assert!(h.app.undo.undo(&mut h.app.edit));
    h.key(Key::C, true);
    h.key(Key::Escape, true);
    h.settle();
    assert!(h.app.cropping.is_none() && h.app.edit.crop.is_none());
}

#[test]
fn trim_handles_and_their_shade_stay_off_the_sound_header() {
    let mut h = Harness::new(3);
    let header = h.rect_of("sound · muted tracks won't be in the file");
    let link = h.rect_of("mute all");
    let (left, right) = (header.left() + 6.0, link.right() - 6.0);
    let x_of = |t: i64| left + (right - left) * t as f32 / (42 * SECOND) as f32;
    h.app.edit.range.end = ((link.center().x - left) / (right - left) * (42 * SECOND) as f32) as i64;
    h.settle();
    let p = widgets::palette(false, [0xE5, 0x40, 0x2F]);
    let shade = Color32::from_black_alpha(158);
    let over = |r: &Rect| r.intersects(header.shrink(1.0)) || r.intersects(link.shrink(1.0));
    let covering: Vec<_> = h.rects.iter().filter(|(r, fill)| (*fill == shade || *fill == p.acc) && over(r)).collect();
    assert!(covering.is_empty(), "{covering:?}");
    assert!(h.rects.iter().any(|(r, fill)| *fill == p.acc && (r.center().x - link.center().x).abs() < 1.0), "the handle is under the link");

    h.click(link.center());
    assert_eq!(h.muted(), [true, true, true], "the link is clickable above the handle");
    let end = h.app.edit.range.end;
    let x = x_of(end);
    h.drag(pos2(x, header.center().y), pos2(x - 150.0, header.center().y));
    assert_eq!(h.app.edit.range.end, end, "the header doesn't grab the handle");
    h.drag(pos2(x, h.at("desktop").y), pos2(x - 150.0, h.at("desktop").y));
    let moved = h.app.edit.range.end;
    assert!(moved < end - 5 * SECOND, "a sound lane grabs it: {end} -> {moved}");
    let x = x_of(moved);
    h.drag(pos2(x, header.center().y - 41.0), pos2(x + 100.0, header.center().y - 41.0));
    assert!(h.app.edit.range.end > moved + 3 * SECOND, "the video lane grabs it");
}

#[test]
fn tracks_get_added_moved_deleted_renamed_and_undone_from_the_lanes() {
    let mut h = Harness::new(2);
    h.click(h.at("+ add empty track"));
    assert_eq!(h.lanes(), ["desktop", "spotifast.exe", "empty"]);
    h.right_click(h.at("empty"));
    assert!(h.has("rename") && h.has("move up") && h.has("move down") && h.has("delete track"));
    h.click(h.at("move up"));
    assert_eq!(h.lanes(), ["desktop", "empty", "spotifast.exe"]);
    assert!(!h.has("delete track"), "picking closes the menu");
    h.right_click(h.at("desktop"));
    h.click(h.at("delete track"));
    assert_eq!(h.lanes(), ["empty", "spotifast.exe"]);
    assert!(h.has("sound · muted tracks won't be in the file"));

    h.right_click(h.at("spotifast.exe"));
    h.click(h.at("rename"));
    assert!(h.ctx.text_edit_focused());
    h.chord(Key::A, Modifiers::COMMAND, true);
    h.frame(vec![Event::Text("music".into())]);
    h.key(Key::Enter, true);
    h.settle();
    assert_eq!(h.lanes(), ["empty", "music"]);
    assert_eq!(h.app.edit.lanes[1].name.as_deref(), Some("music"));

    h.drag(h.at("music"), h.at("empty"));
    assert_eq!(h.lanes(), ["music", "empty"]);
    let Some(Job::Video(v)) = h.app.job() else { panic!() };
    assert_eq!(v.tracks.iter().map(|t| (t.source, t.name.as_str())).collect::<Vec<_>>(), [(Some(1), "music"), (None, "empty")]);
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · 2 sound tracks"));

    let back: [&[&str]; 5] = [
        &["empty", "music"],
        &["empty", "spotifast.exe"],
        &["desktop", "empty", "spotifast.exe"],
        &["desktop", "spotifast.exe", "empty"],
        &["desktop", "spotifast.exe"],
    ];
    for want in back {
        h.chord(Key::Z, Modifiers::COMMAND, true);
        h.settle();
        assert_eq!(h.lanes(), want);
    }
}

#[test]
fn the_preview_smooths_below_200_percent_and_decodes_room_to_shrink() {
    use super::preview::crisp;
    assert!(crisp(1.0, true) && crisp(1.0, false), "100 % stays 1:1");
    assert!(!crisp(0.5, true) && !crisp(0.5, false) && !crisp(1.5, false) && !crisp(1.999 - 0.01, false));
    assert!(!crisp(2.6, true), "fitting a small video up stays smooth");
    assert!(crisp(2.0, false) && crisp(8.0, false), "zooming in to 200 % and past shows pixels");
    let info = Info { duration: SECOND, size: (1920, 1080), fps: 60.0, codec: None, tracks: Vec::new(), clicks: None, keys: None };
    assert_eq!(decode_size(&info, vec2(480.0, 270.0)), (960, 540));
    assert_eq!(decode_size(&info, vec2(1200.0, 675.0)), (1920, 1080));
    assert_eq!(decode_size(&info, vec2(3840.0, 2160.0)), (1920, 1080));
}

fn clicks_harness() -> Harness {
    let at = |button, down: i64, up: Option<i64>, x: f32| Click { button, down, up, x, y: 300.5 };
    let clicks = vec![
        at(Button::Left, 5 * SECOND, Some(5 * SECOND + SECOND / 10), 400.5),
        at(Button::Right, 10 * SECOND, Some(12 * SECOND), 800.5),
        at(Button::Middle, 20 * SECOND, None, 1200.5),
    ];
    Harness::with_clicks(1, Some(Payload { version: 1, scale: 1.0, clicks }))
}

impl Harness {
    fn clicks(&self) -> Vec<Click> {
        self.app.edit.clicks.as_ref().map_or(Vec::new(), |c| c.list.clone())
    }

    fn timeline_x(&self) -> impl Fn(i64) -> f32 + use<> {
        let (left, right) = (self.rect_of("sound · muted tracks won't be in the file").left() + 6.0, self.rect_of("mute all").right() - 6.0);
        move |t| left + (right - left) * t as f32 / (42 * SECOND) as f32
    }

    fn double_click(&mut self, at: Pos2) {
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        self.frame(vec![Event::PointerMoved(at), button(true)]);
        self.frame(vec![button(false)]);
        self.frame(vec![button(true)]);
        self.frame(vec![button(false)]);
        self.settle();
    }
}

#[test]
fn the_clicks_lane_selects_moves_deletes_relabels_and_hides_with_undo() {
    let mut h = clicks_harness();
    assert!(h.has("3 clicks") && !h.has("· hidden, not exported"));
    let x_of = h.timeline_x();
    let y = h.at("3 clicks").y;
    let downs = |h: &Harness| h.clicks().iter().map(|c| c.down).collect::<Vec<_>>();

    h.click(pos2(x_of(5 * SECOND), y));
    assert_eq!((h.app.click_sel.clone(), h.app.pos, h.app.click_focus), (vec![0], 5 * SECOND, true));
    assert!(!h.app.preview_shapes().is_empty(), "the preview shows the click at the playhead");
    h.key(Key::Delete, true);
    assert_eq!(downs(&h), [10 * SECOND, 20 * SECOND]);
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert_eq!(downs(&h), [5 * SECOND, 10 * SECOND, 20 * SECOND]);

    let px = x_of(SECOND) - x_of(0);
    h.drag(pos2(x_of(10 * SECOND), y), pos2(x_of(10 * SECOND) + 4.0 * px, y));
    let moved = h.clicks()[1];
    assert!((moved.down - 14 * SECOND).abs() < SECOND / 10, "{}", moved.down);
    assert_eq!(math::snap(moved.down, 60.0), moved.down, "snapped to a frame");
    assert_eq!(moved.up, Some(moved.down + 2 * SECOND), "the release moves along");
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert_eq!(h.clicks()[1].down, 10 * SECOND);

    h.drag(pos2(x_of(13 * SECOND), y), pos2(x_of(4 * SECOND + SECOND / 2), y));
    let mut picked = h.app.click_sel.clone();
    picked.sort();
    assert_eq!(picked, [0, 1], "box select");
    h.key(Key::Delete, true);
    assert_eq!(downs(&h), [20 * SECOND]);
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();

    h.right_click(pos2(x_of(20 * SECOND), y));
    assert!(h.has("delete") && h.has("left click") && h.has("right click") && h.has("middle click"));
    h.click(h.at("left click"));
    assert_eq!(h.clicks()[2].button, Button::Left);
    assert!(!h.has("left click"));

    let burning = |h: &Harness| matches!(h.app.job(), Some(Job::Video(v)) if v.burn.is_some());
    assert!(burning(&h));
    h.click(h.at("3 clicks"));
    assert!(h.has("· hidden, not exported") && !burning(&h));
    assert!(h.app.preview_shapes().is_empty(), "hidden clicks aren't drawn");
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert!(burning(&h) && h.app.edit.clicks.as_ref().unwrap().shown);

    assert!(h.has("show clicks") && h.has("large"));
    h.click(h.at("large"));
    let Some(Job::Video(v)) = h.app.job() else { panic!() };
    assert_eq!(v.burn.unwrap().style.radius, 42.0);
    h.click(h.at("show clicks"));
    assert!(!h.app.edit.clicks.as_ref().unwrap().shown && !burning(&h));
}

#[test]
fn clicks_move_on_the_preview_and_a_double_click_adds_one() {
    let mut h = clicks_harness();
    let x_of = h.timeline_x();
    let y = h.at("3 clicks").y;
    h.click(pos2(x_of(10 * SECOND), y));
    let (frame, scale) = (h.app.frame_rect, h.app.frame_rect.width() / 1920.0);
    let handle = frame.min + vec2(800.5, 300.5) * scale;
    h.drag(handle, handle + vec2(60.0, 30.0));
    let c = h.clicks()[1];
    assert!((c.x - (800.5 + 60.0 / scale)).abs() < 0.5 && (c.y - (300.5 + 30.0 / scale)).abs() < 0.5, "{c:?}");
    assert!(h.app.play == Play::Paused);
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert_eq!((h.clicks()[1].x, h.clicks()[1].y), (800.5, 300.5));

    h.click(pos2(x_of(10 * SECOND), y));
    h.double_click(frame.center());
    let list = h.clicks();
    assert_eq!(list.len(), 4);
    assert_eq!((list[3].down, list[3].button), (10 * SECOND, Button::Left));
    assert!((list[3].x - 960.0).abs() < 2.0 && (list[3].y - 540.0).abs() < 2.0, "{:?}", list[3]);
    assert!(h.app.play == Play::Paused, "adding doesn't start playback");
    assert_eq!(h.app.click_sel, [3]);
}

#[test]
fn files_without_clicks_get_no_lane() {
    let h = Harness::new(1);
    assert!(h.app.edit.clicks.is_none() && !h.texts.iter().any(|(t, _)| t.ends_with("clicks") || t == "show clicks"));
}

fn keys_harness() -> Harness {
    let txt = |s: &str| keys::KeyLabel::Text(s.into());
    let key = |at: i64, hold: Option<i64>, mods: keys::Mods, main: keys::KeyLabel| keys::Key { at, hold, mods, main };
    let list = vec![
        key(5 * SECOND, Some(5 * SECOND + SECOND / 10), keys::Mods::default(), txt("a")),
        key(10 * SECOND, Some(12 * SECOND), keys::Mods { ctrl: true, ..Default::default() }, txt("c")),
        key(20 * SECOND, None, keys::Mods::default(), keys::KeyLabel::Named(keys::Named::Enter)),
    ];
    Harness::with_keys(1, Some(keys::Payload { version: 1, scale: 1.0, keys: list }))
}

impl Harness {
    fn keys(&self) -> Vec<keys::Key> {
        self.app.edit.keys.as_ref().map_or(Vec::new(), |k| k.list.clone())
    }
}

#[test]
fn the_keys_lane_selects_moves_deletes_realigns_and_hides_with_undo() {
    let mut h = keys_harness();
    assert!(h.has("3 keys") && !h.has("· hidden, not exported"));
    let x_of = h.timeline_x();
    let y = h.at("3 keys").y;
    let ats = |h: &Harness| h.keys().iter().map(|k| k.at).collect::<Vec<_>>();

    h.click(pos2(x_of(5 * SECOND), y));
    assert_eq!((h.app.key_sel.clone(), h.app.pos, h.app.key_focus), (vec![0], 5 * SECOND, true));
    assert!(!h.app.preview_pills(&h.ctx).is_empty(), "the preview shows the key at the playhead");
    h.key(Key::Delete, true);
    assert_eq!(ats(&h), [10 * SECOND, 20 * SECOND]);
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert_eq!(ats(&h), [5 * SECOND, 10 * SECOND, 20 * SECOND]);

    let px = x_of(SECOND) - x_of(0);
    h.drag(pos2(x_of(10 * SECOND), y), pos2(x_of(10 * SECOND) + 4.0 * px, y));
    let moved = h.keys()[1].clone();
    assert!((moved.at - 14 * SECOND).abs() < SECOND / 10, "{}", moved.at);
    assert_eq!(math::snap(moved.at, 60.0), moved.at, "snapped to a frame");
    assert_eq!(moved.hold, Some(moved.at + 2 * SECOND), "the release moves along");
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert_eq!(h.keys()[1].at, 10 * SECOND);

    h.drag(pos2(x_of(13 * SECOND), y), pos2(x_of(4 * SECOND + SECOND / 2), y));
    let mut picked = h.app.key_sel.clone();
    picked.sort();
    assert_eq!(picked, [0, 1], "box select");
    h.key(Key::Delete, true);
    assert_eq!(ats(&h), [20 * SECOND]);
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();

    h.right_click(pos2(x_of(20 * SECOND), y));
    assert!(h.has("delete") && h.has("align left") && h.has("align centre") && h.has("align right"));
    h.click(h.at("align left"));
    assert_eq!(h.app.saved.export.keys.pos, keys::Position::Left);
    assert!(!h.has("align left"), "the menu closes");

    let burning = |h: &Harness| matches!(h.app.job(), Some(Job::Video(v)) if v.keys.is_some());
    assert!(burning(&h));
    h.click(h.at("3 keys"));
    assert!(h.has("· hidden, not exported") && !burning(&h));
    assert!(h.app.preview_pills(&h.ctx).is_empty(), "hidden keys aren't drawn");
    h.chord(Key::Z, Modifiers::COMMAND, true);
    h.settle();
    assert!(burning(&h) && h.app.edit.keys.as_ref().unwrap().shown);

    assert!(h.has("show keys") && h.has("large"));
    h.click(h.at("large"));
    let Some(Job::Video(v)) = h.app.job() else { panic!() };
    assert!((v.keys.unwrap().style.radius - 6.0 * KeySize::Large.factor()).abs() < 0.001, "the size reaches the burn");
    h.click(h.at("show keys"));
    assert!(!h.app.edit.keys.as_ref().unwrap().shown && !burning(&h));
}

#[test]
fn files_without_keys_get_no_lane() {
    let h = Harness::new(1);
    let chip = h.texts.iter().any(|(t, _)| t.ends_with("keys") && t.chars().next().is_some_and(|c| c.is_ascii_digit()));
    assert!(h.app.edit.keys.is_none() && !h.has("show keys") && !chip);
}

impl Harness {
    fn volumes(&self) -> Vec<u16> {
        self.app.edit.lanes.iter().map(|l| l.volume).collect()
    }

    fn hover(&mut self, at: Pos2) {
        self.frame(vec![Event::PointerMoved(at)]);
        self.settle();
    }

    fn wheel(&mut self, at: Pos2, notches: f32) {
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, notches),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        };
        self.frame(vec![Event::PointerMoved(at), wheel]);
        self.settle();
    }

    fn undo(&mut self) {
        self.chord(Key::Z, Modifiers::COMMAND, true);
        self.settle();
    }
}

#[test]
fn a_lanes_volume_shows_off_100_and_moves_by_wheel_drag_and_slider_one_undo_each() {
    let mut h = Harness::new(2);
    assert!(!h.has("100%"), "at 100 % the volume stays out of sight");
    let name = h.at("spotifast.exe");
    h.hover(name);
    assert!(h.has("100%"), "pointing at the lane's head shows it");

    h.wheel(name, 2.0);
    assert_eq!(h.volumes(), [100, 110]);
    let Some(Job::Video(v)) = h.app.job() else { panic!() };
    assert_eq!(v.tracks.iter().map(|t| v.copies(t)).collect::<Vec<_>>(), [true, false], "only the louder track is encoded again");
    h.hover(pos2(20.0, 20.0));
    assert!(h.has("110%") && !h.has("100%"), "off 100 % it stays shown");
    h.undo();
    assert_eq!(h.volumes(), [100, 100], "two notches, one undo");

    h.hover(name);
    let pill = h.at("100%");
    h.drag(pill, pill + vec2(50.0, 0.0));
    assert!((140..=150).contains(&h.volumes()[1]), "{:?}", h.volumes());
    h.undo();
    assert_eq!(h.volumes(), [100, 100]);

    h.hover(name);
    h.click(h.at("100%"));
    let label = h.texts.iter().rev().find(|(t, _)| t == "100%").map(|(_, r)| *r).unwrap();
    let right = label.right() + 14.0;
    let (left, end) = (right - 240.0 + 16.0, right - 60.0);
    h.click(pos2(left + (end - left) * 0.25, label.center().y));
    assert_eq!(h.volumes(), [100, 50]);
    assert_eq!(h.texts.iter().filter(|(t, _)| t == "50%").count(), 2, "the pill and the slider");
    h.double_click(h.at("50%"));
    assert_eq!(h.volumes(), [100, 100]);
    assert!(h.app.volume_pop.is_none());
    h.undo();
    assert_eq!(h.volumes(), [100, 50]);
}

impl Harness {
    /// The last text drawn as `text`: the export panel's lines come after its rows.
    fn at_last(&self, text: &str) -> Pos2 {
        self.texts.iter().rev().find(|(t, _)| t == text).map(|(_, r)| r.center()).unwrap_or_else(|| panic!("no {text:?}"))
    }

    /// Frames until the file being read is opened (or isn't).
    fn opened_by_now(&mut self) {
        for _ in 0..500 {
            if self.app.opening.is_none() {
                return self.settle();
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.frame(Vec::new());
        }
        panic!("still opening");
    }
}

fn file(name: &str, tracks: usize) -> Opened {
    Opened { path: PathBuf::from(format!("C:/nowhere/{name}")), info: Ok(info(tracks, None, None)), index: None }
}

#[test]
fn without_a_file_the_editor_offers_a_drop_zone_and_tells_videos_apart() {
    let mut h = Harness::start(None);
    assert!(h.has("drop a video") && h.has("mp4 · mov · m4v · mkv") && h.has("open file…"));
    assert!(!h.has("export") && !h.has("save"), "no panel without a file");

    h.hovered = vec![PathBuf::from("C:/clips/run.mkv")];
    h.settle();
    assert!(h.has("drop to open") && h.has("run.mkv") && !h.has("open file…"));
    h.hovered = vec![PathBuf::from("C:/clips/notes.txt")];
    h.settle();
    assert!(h.has("not a video") && h.has("notes.txt"));
    h.hovered.clear();
    h.dropped = vec![PathBuf::from("C:/clips/notes.txt")];
    h.settle();
    assert!(h.has("opens mp4, mov, m4v and mkv") && h.app.opening.is_none());
    assert!(h.has("drop a video"));
}

#[test]
fn another_file_opens_in_place_and_unsaved_edits_stay_a_click_away() {
    let mut h = Harness::start(None);
    let ctx = h.ctx.clone();
    h.app.swap_in(&ctx, file("a.mp4", 1), None);
    h.settle();
    assert!(h.app.start.is_none() && h.has("a.mp4") && h.has("export"));
    assert!(h.app.toast.is_none() && h.app.previous.is_none(), "the start screen had nothing to keep");

    h.app.change(|e| e.add_marker(SECOND, 1));
    h.app.swap_in(&ctx, file("b.mp4", 2), None);
    h.settle();
    assert!(h.has("b.mp4") && h.has("a.mp4: edits kept · ") && h.has("back"));
    assert_eq!(h.app.edit.lanes.len(), 2, "b.mp4's own timeline");
    let (path, kept) = h.app.previous.clone().unwrap();
    assert_eq!((path, kept.markers.clone()), (PathBuf::from("C:/nowhere/a.mp4"), vec![SECOND]));

    // "back" reads a.mp4 again; one that can't be read leaves what's open alone
    h.click(h.at("back"));
    assert!(h.app.previous.is_none());
    h.opened_by_now();
    assert!(h.starts("can't open a.mp4: ").is_some() && h.has("b.mp4"));

    // read, its edits go back on
    h.app.swap_in(&ctx, file("a.mp4", 1), Some(kept));
    h.settle();
    assert_eq!(h.app.edit.markers, [SECOND]);
    assert!(h.app.previous.is_none(), "b.mp4 had no edits to keep");
}

#[test]
fn a_file_held_over_an_open_clip_shows_where_it_goes() {
    let mut h = Harness::new(1);
    assert!(!h.has("drop to open"));
    h.hovered = vec![PathBuf::from("C:/clips/next.mp4")];
    h.settle();
    assert!(h.has("drop to open") && h.has("next.mp4"));
    h.hovered.clear();
    h.settle();
    assert!(!h.has("drop to open"));
    h.dropped = vec![PathBuf::from("C:/nowhere/glint_2026-10-21_04-36.mp4")];
    h.settle();
    assert!(h.app.opening.is_none(), "the file that's open stays as it is");
}

#[test]
fn the_panel_says_when_the_export_is_lossless_and_why_not() {
    let mut h = Harness::opened(info(1, None, None), Some(index(1)));
    assert!(h.has("lossless") && h.has("· no re-encode"));
    assert!(h.starts("starts ").is_none(), "the whole file starts on its first keyframe");
    let frame = h.app.frame_len();
    h.app.change(|e| {
        e.range.set_start(3 * SECOND, frame);
        true
    });
    h.settle();
    assert!(h.has("starts 1.0 s early, on a keyframe"));

    h.app.saved.export.height = Some(720);
    h.settle();
    assert!(h.has("re-encodes ·") && !h.has("lossless"));
    h.click(h.at_last("720p"));
    assert_eq!(h.app.saved.export.height, None);
    assert!(h.has("lossless"));

    h.app.saved.export.fps = Some(30);
    h.app.change(|e| {
        e.crop = Some(Crop { x: 0, y: 0, w: 960, h: 540 });
        true
    });
    h.settle();
    assert!(h.has("cropped,") && h.has("30 fps"));
    h.click(h.at_last("cropped,"));
    assert!(h.app.edit.crop.is_none() && h.has("30 fps") && !h.has("cropped,"));
    h.click(h.at_last("30 fps"));
    assert!(h.app.saved.export.fps.is_none() && h.has("lossless"));
}

#[test]
fn drawn_in_clicks_are_the_reason_and_a_click_hides_them() {
    let down = 5 * SECOND;
    let clicks = vec![Click { button: Button::Left, down, up: Some(down + SECOND / 10), x: 10.0, y: 10.0 }];
    let mut h = Harness::opened(info(1, Some(Payload { version: 1, scale: 1.0, clicks }), None), Some(index(1)));
    assert!(h.has("re-encodes ·") && h.has("clicks drawn in"));
    h.click(h.at_last("clicks drawn in"));
    assert!(h.app.edit.clicks.as_ref().is_some_and(|c| !c.shown) && h.has("lossless"));
    let without = Harness::opened(info(1, None, None), None);
    assert!(without.has("can't copy this file"), "no sample index, no copy");
}
