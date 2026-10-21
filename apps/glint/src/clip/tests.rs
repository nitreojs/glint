use eframe::App;
use eframe::egui::{Event, PointerButton, Pos2};
use glint_core::clip::export::Format;
use glint_core::clip::info::Track;
use glint_core::clip::math::SECOND;

use super::*;

const SCREEN: egui::Vec2 = vec2(WINDOW[0], WINDOW[1]);

struct Harness {
    ctx: egui::Context,
    app: ClipApp,
    frame: eframe::Frame,
    time: f64,
    texts: Vec<(String, Rect)>,
    rects: Vec<(Rect, Color32)>,
}

impl Harness {
    fn new(tracks: usize) -> Harness {
        let ctx = egui::Context::default();
        glint_ui::theme::fonts(&ctx);
        let names = ["desktop", "spotifast.exe", "mic: HyperX QuadCast"];
        let info = Info {
            duration: 42 * SECOND,
            size: (1920, 1080),
            fps: 60.0,
            codec: None,
            tracks: (0..tracks).map(|k| Track { stream: k as u32 + 1, name: names[k % 3].into() }).collect(),
        };
        let path = PathBuf::from("C:/nowhere/glint_2026-10-21_04-36.mp4");
        let app = ClipApp::new(path, Ok(info), None, &Config::default(), Saved::default(), [0xE5, 0x40, 0x2F]);
        let mut h = Harness { ctx, app, frame: eframe::Frame::_new_kittest(), time: 0.0, texts: Vec::new(), rects: Vec::new() };
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
    assert!(h.texts.iter().all(|(t, _)| !t.contains("instantly") && !t.contains("re-encode")));

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
    let info = Info { duration: SECOND, size: (1920, 1080), fps: 60.0, codec: None, tracks: Vec::new() };
    assert_eq!(decode_size(&info, vec2(480.0, 270.0)), (960, 540));
    assert_eq!(decode_size(&info, vec2(1200.0, 675.0)), (1920, 1080));
    assert_eq!(decode_size(&info, vec2(3840.0, 2160.0)), (1920, 1080));
}
