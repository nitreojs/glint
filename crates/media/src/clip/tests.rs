use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32};

use glint_core::clip::crop::Crop;
use glint_core::clip::edit::Edit;
use glint_core::clip::export::{self, Format, Job, Source, Video, VideoJob};
use glint_core::clip::index;
use glint_core::clip::math::{Range, SECOND};

use super::media::{self, Com};
use super::trim::{self, Progress};

const ACCENT: [u8; 3] = [0xE5, 0x40, 0x2F];

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
    let source = Source { info: &info, index: Some(&index), bytes: std::fs::metadata(src).unwrap().len(), accent: ACCENT };
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
    edit.lanes[1].muted = true;
    let (job, estimate) = export_to(&src, &edit, &export::Settings::default(), &out);
    let Job::Video(VideoJob { video: Video::Copy, pieces, tracks, .. }) = &job else { panic!("{job:?}") };
    assert_eq!(tracks.iter().map(|t| t.source).collect::<Vec<_>>(), [Some(0)]);
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

fn pixel(path: &Path, at: f64, x: u32, y: u32) -> [u8; 3] {
    let (p, t, crop) = (path.to_string_lossy(), format!("{at}"), format!("crop=2:2:{x}:{y}"));
    let args = ["-v", "error", "-ss", &t, "-i", &p, "-frames:v", "1", "-vf", &crop, "-f", "rawvideo", "-pix_fmt", "rgb24", "-"];
    let out = std::process::Command::new("ffmpeg").args(args).output().unwrap_or_else(|e| panic!("ffmpeg: {e}"));
    assert!(out.status.success() && out.stdout.len() == 12, "{}", String::from_utf8_lossy(&out.stderr));
    [out.stdout[0], out.stdout[1], out.stdout[2]]
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn shown_clicks_burn_into_the_reencode_and_hidden_ones_leave_the_file_clean() {
    use glint_core::clip::clicks::{ClickColor, ClickLook, ClickSize, Clicks};
    use glint_core::record::clicks::{self, Button, Click, Payload};
    let src = std::env::temp_dir().join(format!("glint-clip-clicks-{}.mp4", std::process::id()));
    let p = src.to_string_lossy().into_owned();
    let args = [
        "-y", "-v", "error", "-f", "lavfi", "-i", "color=c=0x202020:size=640x360:rate=30:duration=3", "-f", "lavfi", "-i",
        "sine=frequency=440:sample_rate=48000:duration=3", "-map", "0", "-map", "1", "-c:v", "libx264", "-bf", "0", "-g", "30",
        "-pix_fmt", "yuv420p", "-c:a", "aac", &p,
    ];
    tool("ffmpeg", &args);
    let held = Click { button: Button::Left, down: SECOND, up: Some(2 * SECOND), x: 400.5, y: 200.5 };
    let early = Click { button: Button::Right, down: SECOND / 50, up: Some(SECOND / 25), x: 100.5, y: 100.5 };
    clicks::save(&src, &Payload { version: 1, scale: 1.0, clicks: vec![early, held] }).unwrap();
    let info = {
        let _com = Com::init();
        media::probe(&src).unwrap()
    };
    assert_eq!(info.clicks.as_ref().map(|c| c.clicks.len()), Some(2));

    let mut edit = Edit::new(3 * SECOND, 1);
    edit.range = Range { start: SECOND / 2, end: 3 * SECOND };
    edit.clicks = info.clicks.as_ref().map(Clicks::new);
    let look = ClickLook { size: ClickSize::Large, color: ClickColor::White };
    let settings = export::Settings { clicks: look, ..export::Settings::default() };
    let mut seen = Vec::new();
    for gpu in [true, false] {
        let out = src.with_file_name(format!("glint-clip-clicks-out-{gpu}-{}.mp4", std::process::id()));
        let job = if gpu {
            export_to(&src, &edit, &settings, &out).0
        } else {
            let index = index::read(&src).unwrap();
            let source = Source { info: &info, index: Some(&index), bytes: 0, accent: ACCENT };
            let job = export::plan(&edit, &settings, &source);
            let (pm, c) = (AtomicU32::new(0), AtomicBool::new(false));
            let _com = Com::init();
            assert_eq!(trim::export_in_software(&src, &info, &job, &out, &Progress { permille: &pm, cancel: &c }), Ok(true));
            job
        };
        let facts = (pixel(&out, 1.0, 400, 200), pixel(&out, 1.0, 300, 200), pixel(&out, 2.0, 400, 200), clicks::load(&out).unwrap().is_none());
        let _ = std::fs::remove_file(&out);
        seen.push((gpu, job, facts));
    }
    edit.clicks.as_mut().unwrap().shown = false;
    let out = src.with_file_name(format!("glint-clip-clicks-hidden-{}.mp4", std::process::id()));
    let (hidden, _) = export_to(&src, &edit, &settings, &out);
    let (hidden_px, hidden_box) = (pixel(&out, 1.0, 400, 200), clicks::load(&out).unwrap());
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);

    for (gpu, job, (dot, beside, after, no_box)) in seen {
        let Job::Video(v) = &job else { panic!("{job:?}") };
        assert!(matches!(v.video, Video::Encode { .. }), "burning in re-encodes");
        assert_eq!(v.burn.as_ref().map(|b| b.clicks.clone()), Some(vec![held]), "the click before the trim start is left out");
        assert!(dot.iter().all(|&c| c > 200), "gpu {gpu}: the held dot is drawn: {dot:?}");
        assert!(beside.iter().all(|&c| c < 60), "gpu {gpu}: and nothing around it: {beside:?}");
        assert!(after.iter().all(|&c| c < 60), "gpu {gpu}: gone after the release: {after:?}");
        assert!(no_box, "gpu {gpu}: burnt-in clicks aren't saved again");
    }
    assert!(matches!(&hidden, Job::Video(VideoJob { video: Video::Copy, burn: None, .. })), "{hidden:?}");
    assert!(hidden_px.iter().all(|&c| c < 60), "{hidden_px:?}");
    assert_eq!(hidden_box, None, "hidden clicks leave the file with none");
}

fn max_volume(path: &Path, track: usize) -> f64 {
    let p = path.to_string_lossy();
    let map = format!("0:a:{track}");
    let args = ["-v", "info", "-nostats", "-i", &p, "-map", &map, "-af", "volumedetect", "-f", "null", "-"];
    let out = std::process::Command::new("ffmpeg").args(args).output().unwrap_or_else(|e| panic!("ffmpeg: {e}"));
    let log = String::from_utf8_lossy(&out.stderr);
    let line = log.lines().find_map(|l| l.split_once("max_volume: ")).unwrap_or_else(|| panic!("no volume for {map}: {log}")).1;
    line.trim_end_matches(" dB").trim().parse().unwrap_or(f64::NEG_INFINITY)
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn deleted_reordered_renamed_and_empty_tracks_come_out_as_shown() {
    let src = fixture("lanes", "640x360", "30", &["-g", "30", "-filter:a:1", "volume=0.1"]);
    let info = {
        let _com = Com::init();
        media::probe(&src).unwrap()
    };
    let mut edit = Edit::new(3 * SECOND, 2);
    edit.range = Range { start: SECOND, end: 3 * SECOND };
    assert!(edit.delete_lane(0));
    edit.add_empty();
    assert!(edit.move_lane(1, 0));
    assert!(edit.rename(1, "voice", &info.tracks));
    let mut facts = Vec::new();
    for (name, settings) in [("copy", export::Settings::default()), ("encode", export::Settings { height: Some(180), ..export::Settings::default() })] {
        let out = src.with_file_name(format!("glint-clip-lanes-{name}-{}.mp4", std::process::id()));
        let (job, estimate) = export_to(&src, &edit, &settings, &out);
        let size = std::fs::metadata(&out).unwrap().len();
        let streams = probe(&out, "stream=codec_type,codec_name,sample_rate,channels");
        let names = probe(&out, "stream_tags=handler_name");
        let lengths: Vec<f64> = probe(&out, "stream=duration").iter().map(|d| d.parse().unwrap()).collect();
        let volumes = [max_volume(&out, 0), max_volume(&out, 1)];
        let _ = std::fs::remove_file(&out);
        facts.push((name, job, estimate, size, streams, names, lengths, volumes));
    }
    let _ = std::fs::remove_file(&src);

    for (name, job, estimate, size, streams, names, lengths, volumes) in facts {
        let Job::Video(v) = &job else { panic!("{job:?}") };
        assert_eq!(v.video == Video::Copy, name == "copy", "{job:?}");
        assert_eq!(v.tracks.iter().map(|t| (t.source, t.name.as_str())).collect::<Vec<_>>(), [(None, "empty"), (Some(1), "voice")]);
        assert_eq!(streams[0], "h264,video", "{name}");
        assert_eq!(streams[1..], ["aac,audio,48000,1", "aac,audio,48000,1"], "{name}: the empty track matches the kept one");
        assert_eq!(names, ["VideoHandler", "empty", "voice"], "{name}");
        assert!(volumes[0] < -90.0, "{name}: the empty track is silent: {volumes:?}");
        assert!(volumes[1] > -45.0 && volumes[1] < -30.0, "{name}: the second track is the quiet mic: {volumes:?}");
        for d in &lengths[1..] {
            assert!((d - lengths[0]).abs() < 0.1, "{name}: every track runs the kept length: {lengths:?}");
        }
        if name == "copy" {
            let off = (estimate as f64 - size as f64).abs() / size as f64;
            assert!(off < 0.05, "estimate {estimate} vs {size}");
        }
    }
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn an_empty_track_stays_its_own_next_to_a_combined_mix() {
    let src = fixture("mixempty", "640x360", "30", &["-g", "30"]);
    let out = src.with_file_name(format!("glint-clip-mixempty-out-{}.mp4", std::process::id()));
    let mut edit = Edit::new(3 * SECOND, 2);
    edit.add_empty();
    assert!(edit.move_lane(2, 1));
    let (job, _) = export_to(&src, &edit, &export::Settings { combine: true, ..export::Settings::default() }, &out);
    let streams = probe(&out, "stream=codec_type,codec_name,sample_rate,channels");
    let names = probe(&out, "stream_tags=handler_name");
    let volumes = [max_volume(&out, 0), max_volume(&out, 1)];
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(matches!(&job, Job::Video(v) if v.combine && v.video == Video::Copy), "{job:?}");
    assert_eq!(streams, ["h264,video", "aac,audio,48000,2", "aac,audio,48000,2"]);
    assert_eq!(names, ["VideoHandler", "desktop + mic", "empty"]);
    assert!(volumes[0] > -30.0 && volumes[1] < -90.0, "{volumes:?}");
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

fn video_stream_facts(path: &Path) -> Vec<String> {
    let p = path.to_string_lossy();
    let entries = "stream=codec_name,width,height,r_frame_rate,nb_frames,bit_rate";
    tool("ffprobe", &["-v", "error", "-select_streams", "v:0", "-show_entries", entries, "-of", "csv=p=0", &p])
        .trim()
        .split(',')
        .map(str::to_owned)
        .collect()
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn thirty_fps_at_3_mbps_drops_frames_on_the_grid() {
    let noisy = ["-vf", "noise=alls=25:allf=t+u", "-g", "60", "-b:v", "20M"];
    let src = fixture("rate", "1280x720", "60", &noisy);
    let out = src.with_file_name(format!("glint-clip-rate-out-{}.mp4", std::process::id()));
    let edit = Edit::new(3 * SECOND, 2);
    let settings = export::Settings { fps: Some(30), kbps: Some(3000), ..export::Settings::default() };
    let (job, estimate) = export_to(&src, &edit, &settings, &out);
    let facts = video_stream_facts(&out);
    let audio = probe(&out, "stream=codec_type,duration");
    let size = std::fs::metadata(&out).unwrap().len();
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(matches!(&job, Job::Video(VideoJob { video: Video::Encode { fps: Some(30), bitrate: 3_000_000, .. }, .. })));
    assert_eq!(facts[..4], ["h264", "1280", "720", "30/1"], "{facts:?}");
    assert_eq!(facts[5], "90", "{facts:?}");
    let bps: f64 = facts[4].parse().unwrap();
    assert!((bps / 3e6 - 1.0).abs() < 0.15, "{bps}");
    assert_eq!(audio.iter().filter(|a| a.starts_with("audio")).count(), 2, "{audio:?}");
    for a in audio.iter().filter(|a| a.starts_with("audio")) {
        let d: f64 = a.split(',').nth(1).unwrap().parse().unwrap();
        assert!((d - 3.0).abs() < 0.06, "{a}");
    }
    let off = (estimate as f64 - size as f64).abs() / size as f64;
    assert!(off < 0.15, "estimate {estimate} vs {size}");
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH and an hevc encoder; writes and deletes temp files"]
fn switching_to_hevc_reencodes_with_it() {
    let has = {
        let _com = Com::init();
        media::has_encoder(windows::Win32::Media::MediaFoundation::MFVideoFormat_HEVC)
    };
    assert!(has, "this pc has no hevc encoder");
    let src = fixture("hevc", "1280x720", "30", &["-g", "30"]);
    let out = src.with_file_name(format!("glint-clip-hevc-out-{}.mp4", std::process::id()));
    let mut edit = Edit::new(3 * SECOND, 2);
    edit.range = Range { start: SECOND / 2, end: 5 * SECOND / 2 };
    let settings = export::Settings { codec: export::CodecChoice::Hevc, ..export::Settings::default() };
    let (job, _) = export_to(&src, &edit, &settings, &out);
    let facts = video_stream_facts(&out);
    let duration: f64 = probe(&out, "format=duration")[0].parse().unwrap();
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert!(matches!(&job, Job::Video(VideoJob { video: Video::Encode { codec: glint_core::record::Codec::Hevc, fps: None, .. }, .. })));
    assert_eq!(facts[..4], ["hevc", "1280", "720", "30/1"], "{facts:?}");
    assert!((duration - 2.0).abs() < 0.08, "{duration}");
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH; writes and deletes temp files"]
fn an_uneven_recording_scales_down_to_a_steady_lower_rate() {
    // like a game recording: 144 fps with frames missing here and there
    let uneven = ["-vf", "select='not(mod(n\\,3))+not(mod(n\\,7))',setpts=N/144/TB*1.0", "-fps_mode", "vfr"];
    let src = fixture("uneven", "1280x720", "144", &uneven);
    let out = src.with_file_name(format!("glint-clip-uneven-out-{}.mp4", std::process::id()));
    let secs: f64 = probe(&src, "format=duration")[0].parse().unwrap();
    let edit = Edit::new((secs * SECOND as f64) as i64, 2);
    let settings = export::Settings { fps: Some(60), height: Some(360), ..export::Settings::default() };
    export_to(&src, &edit, &settings, &out);
    let facts = video_stream_facts(&out);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
    assert_eq!(facts[..4], ["h264", "640", "360", "60/1"], "{facts:?}");
}
