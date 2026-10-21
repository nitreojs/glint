use eframe::App;
use eframe::egui::{Event, PointerButton, Pos2};

use super::export::{Format, Video, VideoJob};
use super::math::{Range, SECOND};
use super::media::Track;
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
        crate::settings::fonts(&ctx);
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
    assert!(!h.has("frame rate") && !h.has("reset"));
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

// ---- headless exports: need ffmpeg and ffprobe on PATH -------------------------------------------

fn tool(name: &str, args: &[&str]) -> String {
    let out = std::process::Command::new(name).args(args).output().unwrap_or_else(|e| panic!("{name}: {e}"));
    assert!(out.status.success(), "{name} {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn probe(path: &Path, entries: &str) -> Vec<String> {
    let p = path.to_string_lossy();
    tool("ffprobe", &["-v", "error", "-show_entries", entries, "-of", "csv=p=0", &p]).lines().map(str::to_owned).collect()
}

fn first_packet_size(path: &Path, at: f64) -> u64 {
    let p = path.to_string_lossy();
    let out = tool("ffprobe", &["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,size", "-of", "csv=p=0", &p]);
    out.lines()
        .filter_map(|l| l.split_once(','))
        .find(|(t, _)| (t.parse::<f64>().unwrap_or(-1.0) - at).abs() < 0.001)
        .and_then(|(_, s)| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("no packet at {at} in {p}"))
}

fn fixture(name: &str, size: &str, rate: &str, extra: &[&str]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("glint-clip-{name}-{}.mp4", std::process::id()));
    let p = path.to_string_lossy().into_owned();
    let video = format!("testsrc2=size={size}:rate={rate}:duration=3");
    let mut args = vec![
        "-y", "-v", "error", "-f", "lavfi", "-i", &video, "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=3",
        "-f", "lavfi", "-i", "sine=frequency=880:sample_rate=48000:duration=3", "-map", "0", "-map", "1", "-map", "2",
        "-c:v", "libx264", "-bf", "0", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "128k",
        "-metadata:s:a:0", "title=desktop", "-metadata:s:a:1", "title=mic",
    ];
    args.extend_from_slice(extra);
    args.push(&p);
    tool("ffmpeg", &args);
    path
}

fn export_to(src: &Path, edit: &Edit, settings: &export::Settings, out: &Path) -> (Job, u64) {
    let _com = Com::init();
    let info = media::probe(src).unwrap();
    let index = index::read(src).unwrap();
    let source = Source { info: &info, index: Some(&index), bytes: std::fs::metadata(src).unwrap().len() };
    let job = export::plan(edit, settings, &source);
    let estimate = export::estimate(&job, &source);
    let (pm, c) = (AtomicU32::new(0), AtomicBool::new(false));
    let done = trim::export(src, &info, &job, out, 60, &Progress { permille: &pm, cancel: &c }).unwrap();
    assert!(done);
    (job, estimate)
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn the_fast_path_copies_drops_a_muted_track_and_cuts() {
    let src = fixture("fast", "1280x720", "30", &["-g", "30"]);
    let out = src.with_file_name(format!("glint-clip-fast-out-{}.mp4", std::process::id()));
    let mut edit = Edit::new(3 * SECOND, 2);
    edit.range = Range { start: SECOND / 2, end: 3 * SECOND };
    assert!(edit.split(3 * SECOND / 2, 1) && edit.split(2 * SECOND, 1));
    assert!(edit.toggle_removed(1));
    edit.muted[1] = true;
    let (job, estimate) = export_to(&src, &edit, &export::Settings::default(), &out);
    let Job::Video(VideoJob { video: Video::Copy, pieces, tracks, .. }) = &job else { panic!("{job:?}") };
    assert_eq!(tracks, &[0]);
    assert_eq!(pieces.iter().map(|p| (p.lead, p.end)).collect::<Vec<_>>(), [(0, 3 * SECOND / 2), (2 * SECOND, 3 * SECOND)]);

    let streams = probe(&out, "stream=codec_type,codec_name,width,height");
    let size = std::fs::metadata(&out).unwrap().len();
    let duration: f64 = probe(&out, "format=duration")[0].parse().unwrap();
    let frames = probe(&out, "stream=nb_frames");
    let names = probe(&out, "stream_tags=handler_name");
    let same_start = first_packet_size(&out, 0.0) == first_packet_size(&src, 0.0);
    let same_cut = first_packet_size(&out, 1.5) == first_packet_size(&src, 2.0);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert_eq!(streams, ["h264,video,1280,720", "aac,audio"]);
    assert!((duration - 2.5).abs() < 0.08, "{duration}");
    assert_eq!(frames[0], "75");
    assert_eq!(names, ["VideoHandler", "desktop"]);
    assert!(same_start && same_cut, "the pieces start with the source's own keyframes");
    let off = (estimate as f64 - size as f64).abs() / size as f64;
    assert!(off < 0.05, "estimate {estimate} vs {size}");
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn a_10_mb_target_reencodes_the_crop_under_the_limit() {
    let cbr = [
        "-vf", "noise=alls=30:allf=t+u", "-g", "60", "-b:v", "40M", "-minrate", "40M", "-maxrate", "40M", "-bufsize", "4M",
        "-x264-params", "nal-hrd=cbr",
    ];
    let src = fixture("target", "1920x1080", "60", &cbr);
    let out = src.with_file_name(format!("glint-clip-target-out-{}.mp4", std::process::id()));
    let mut edit = Edit::new(3 * SECOND, 2);
    edit.crop = Some(Crop { x: 101, y: 51, w: 1601, h: 901 });
    let settings = export::Settings { target_mb: Some(10), ..export::Settings::default() };
    let src_size = std::fs::metadata(&src).unwrap().len();
    let (job, estimate) = export_to(&src, &edit, &settings, &out);
    let streams = probe(&out, "stream=codec_type,width,height");
    let size = std::fs::metadata(&out).unwrap().len();
    let duration: f64 = probe(&out, "format=duration")[0].parse().unwrap();

    let soft = src.with_file_name(format!("glint-clip-target-soft-{}.mp4", std::process::id()));
    let (pm, c) = (AtomicU32::new(0), AtomicBool::new(false));
    let info = {
        let _com = Com::init();
        media::probe(&src).unwrap()
    };
    let soft_done = {
        let _com = Com::init();
        trim::export_in_software(&src, &info, &job, &soft, &Progress { permille: &pm, cancel: &c })
    };
    let soft_streams = probe(&soft, "stream=codec_type,width,height");
    let soft_size = std::fs::metadata(&soft).map_or(0, |m| m.len());
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&soft);
    assert_eq!(soft_done, Ok(true));
    assert_eq!(soft_streams, ["video,1600,900", "audio", "audio"]);
    assert!(soft_size <= 10 * export::MB, "a missed target gets a second, smaller pass: {soft_size}");

    assert!(src_size > 12 * export::MB, "the source has to be over the target: {src_size}");
    let Job::Video(VideoJob { video: Video::Encode { size: dims, crop, .. }, .. }) = &job else { panic!("{job:?}") };
    assert_eq!((*dims, *crop), ((1600, 900), Some(Crop { x: 100, y: 50, w: 1600, h: 900 })));
    assert_eq!(streams, ["video,1600,900", "audio", "audio"]);
    assert!((duration - 3.0).abs() < 0.08, "{duration}");
    assert!(size <= 10 * export::MB, "{size}");
    assert!(estimate <= 10 * export::MB);
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn a_cropped_gif_comes_out_at_the_scaled_crop() {
    let src = fixture("gif", "1280x720", "30", &["-g", "30"]);
    let out = src.with_file_name(format!("glint-clip-gif-out-{}.gif", std::process::id()));
    let mut edit = Edit::new(3 * SECOND, 2);
    edit.range = Range { start: SECOND, end: 2 * SECOND };
    edit.crop = Some(Crop { x: 640, y: 0, w: 640, h: 720 });
    let settings = export::Settings { format: Format::Gif, height: Some(360), gif_fps: 10, ..export::Settings::default() };
    let (job, _) = export_to(&src, &edit, &settings, &out);
    let streams = probe(&out, "stream=codec_name,width,height");
    let duration: f64 = probe(&out, "format=duration")[0].parse().unwrap();
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(matches!(&job, Job::Gif(g) if g.size == (320, 360)), "{job:?}");
    assert_eq!(streams, ["gif,320,360"]);
    assert!((duration - 1.0).abs() < 0.05, "{duration}");
}
