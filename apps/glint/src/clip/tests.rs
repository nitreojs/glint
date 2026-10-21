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
        let mut h = Harness { ctx, app, frame: eframe::Frame::_new_kittest(), time: 0.0, texts: Vec::new() };
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
        self.frame(vec![Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }]);
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
    assert_eq!(h.app.edit.muted, [false, true, true]);
    h.click(h.at("mute all"));
    assert_eq!(h.app.edit.muted, [true, true, true]);
    assert_eq!(h.starts("0:42 of").as_deref(), Some("0:42 of 0:42 · no sound"));
    h.click(h.at("unmute all"));
    assert_eq!(h.app.edit.muted, [false, false, false]);

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
