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
    edit.range = Range { start: 0, end: 3 * SECOND };
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
fn track_volumes_change_only_their_tracks_and_keep_the_video_copied() {
    let src = fixture("volume", "640x360", "30", &["-g", "30"]);
    let out = src.with_file_name(format!("glint-clip-volume-out-{}.mp4", std::process::id()));
    let mut edit = Edit::new(3 * SECOND, 2);
    assert!(edit.set_volume(0, 50) && edit.set_volume(1, 150));
    let (job, _) = export_to(&src, &edit, &export::Settings::default(), &out);
    let before = [max_volume(&src, 0), max_volume(&src, 1)];
    let after = [max_volume(&out, 0), max_volume(&out, 1)];
    let streams = probe(&out, "stream=codec_type,codec_name");
    let names = probe(&out, "stream_tags=handler_name");
    let same_start = first_packet_size(&out, 0.0) == first_packet_size(&src, 0.0);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    let Job::Video(v) = &job else { panic!("{job:?}") };
    assert!(v.video == Video::Copy && same_start, "the picture is copied: {job:?}");
    assert!(v.tracks.iter().all(|t| !v.copies(t)));
    assert_eq!(streams, ["h264,video", "aac,audio", "aac,audio"]);
    assert_eq!(names, ["VideoHandler", "desktop", "mic"]);
    let gained = [after[0] - before[0], after[1] - before[1]];
    eprintln!("max volume {before:?} dB → {after:?} dB, {gained:?}");
    assert!((gained[0] + 6.02).abs() < 0.5, "50 % is −6 dB: {gained:?}");
    assert!((gained[1] - 3.52).abs() < 0.5, "150 % is +3.5 dB: {gained:?}");
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

// ---- smart cut: need ffmpeg and ffprobe on PATH ---------------------------------------------------

/// Where the smart cut tests keep their (large) files: `GLINT_TEST_DIR`, or the temp folder.
fn test_dir() -> PathBuf {
    std::env::var_os("GLINT_TEST_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

/// `secs` of 60 fps 1080p with sound, from ffmpeg's `encoder` with a keyframe every 2 s and no b-frames.
fn long_fixture(name: &str, encoder: &str, secs: u32) -> PathBuf {
    let path = test_dir().join(format!("glint-smart-{name}-{}.mp4", std::process::id()));
    let p = path.to_string_lossy().into_owned();
    let video = format!("testsrc2=size=1920x1080:rate=60:duration={secs}");
    let sound = format!("sine=frequency=440:sample_rate=48000:duration={secs}");
    let args = [
        "-y", "-v", "error", "-f", "lavfi", "-i", &video, "-f", "lavfi", "-i", &sound, "-map", "0", "-map", "1", "-c:v", encoder,
        "-g", "120", "-bf", "0", "-b:v", "12M", "-pix_fmt", "yuv420p", "-tag:v", if encoder.starts_with("hevc") { "hvc1" } else { "avc1" },
        "-c:a", "aac", "-b:a", "192k", &p,
    ];
    tool("ffmpeg", &args);
    path
}

/// Each frame ffmpeg decodes from `path`: its time in seconds and the md5 of its picture.
fn frame_md5s(path: &Path) -> Vec<(f64, String)> {
    let p = path.to_string_lossy();
    let args = ["-v", "error", "-i", &p, "-map", "0:v:0", "-fps_mode", "passthrough", "-enc_time_base", "demux", "-f", "framemd5", "-"];
    let out = tool("ffmpeg", &args);
    let mut tb = 1.0;
    let mut frames = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("#tb 0: ") {
            let (n, d) = rest.split_once('/').unwrap();
            tb = n.trim().parse::<f64>().unwrap() / d.trim().parse::<f64>().unwrap();
        } else if !line.starts_with('#') {
            let cols: Vec<&str> = line.split(',').map(str::trim).collect();
            frames.push((cols[2].parse::<f64>().unwrap() * tb, cols[5].to_string()));
        }
    }
    frames
}

const THUMB: usize = 128 * 72;

/// Each frame of `path` as a small grey picture.
fn thumbs(path: &Path) -> Vec<Vec<u8>> {
    let p = path.to_string_lossy();
    let args = ["-v", "error", "-i", &p, "-map", "0:v:0", "-fps_mode", "passthrough", "-vf", "scale=128:72,format=gray", "-f", "rawvideo", "-"];
    let out = std::process::Command::new("ffmpeg").args(args).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout.chunks_exact(THUMB).map(<[u8]>::to_vec).collect()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
}

/// The times of `path`'s video keyframes, in seconds.
fn key_times(path: &Path) -> Vec<f64> {
    let p = path.to_string_lossy();
    let out = tool("ffprobe", &["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0", &p]);
    let mut keys: Vec<f64> = out.lines().filter(|l| l.contains(",K")).filter_map(|l| l.split(',').next()?.parse().ok()).collect();
    keys.sort_by(f64::total_cmp);
    keys
}

/// Checks a smart cut of `src` from `start` to `end` (seconds) written to `out`: it starts on the frame
/// shown at `start`, decodes cleanly in ffmpeg and media foundation, its frames from the first copied
/// keyframe on are the recording's own, those before it look like the ones they stand for (not their
/// neighbours), and the sound runs as long as the picture. Returns how many frames were encoded anew.
fn assert_smart_cut(src: &Path, out: &Path, start: f64, end: f64) -> usize {
    let p = out.to_string_lossy();
    let args = ["-v", "error", "-i", &p, "-fps_mode", "passthrough", "-enc_time_base", "demux", "-f", "null", "-"];
    let errors = std::process::Command::new("ffmpeg").args(args).output().unwrap();
    assert!(errors.status.success() && errors.stderr.is_empty(), "ffmpeg: {}", String::from_utf8_lossy(&errors.stderr));

    let (src_frames, out_frames) = (frame_md5s(src), frame_md5s(out));
    let s0 = src_frames.iter().rposition(|f| f.0 <= start + 1e-6).unwrap();
    let e0 = src_frames.iter().position(|f| f.0 >= end - 1e-6).unwrap_or(src_frames.len());
    assert_eq!(out_frames.len(), e0 - s0, "one frame for each the cut shows");
    let key = key_times(src).into_iter().find(|&k| k >= src_frames[s0].0 - 1e-6).filter(|&k| k < end);
    let k0 = key.map_or(e0, |k| src_frames.iter().position(|f| (f.0 - k).abs() < 1e-6).unwrap());
    for (i, (o, s)) in out_frames.iter().zip(&src_frames[s0..e0]).enumerate() {
        let at = (s.0 - start).max(0.0);
        assert!((o.0 - at).abs() < 0.002, "frame {i} at {} for {at}", o.0);
        if s0 + i >= k0 {
            assert_eq!(o.1, s.1, "frame {i} (source {}) is copied as it was", s0 + i);
        }
    }
    let (src_thumbs, out_thumbs) = (thumbs(src), thumbs(out));
    for i in 0..k0 - s0 {
        let own = psnr(&out_thumbs[i], &src_thumbs[s0 + i]);
        assert!(own > 30.0, "re-encoded frame {i}: {own:.1} dB");
        for n in [(s0 + i).wrapping_sub(1), s0 + i + 1].into_iter().filter(|&n| n < src_thumbs.len()) {
            let other = psnr(&out_thumbs[i], &src_thumbs[n]);
            assert!(own >= other - 0.5, "re-encoded frame {i} looks like source {n} ({other:.1} dB) more than its own ({own:.1} dB)");
        }
    }

    let _com = Com::init();
    let info = media::probe(out).unwrap();
    if info.snap != Some(glint_core::clip::info::Snap::Codec) {
        let reader = media::open(out, None).unwrap();
        media::video_only(&reader, windows::Win32::Media::MediaFoundation::MFVideoFormat_NV12, info.size).unwrap();
        let mut decoded = 0;
        while media::next_video(&reader).unwrap().is_some() {
            decoded += 1;
        }
        assert_eq!(decoded, e0 - s0, "media foundation decodes every frame");
    }

    let lengths: Vec<f64> = probe(out, "stream=duration").iter().map(|d| d.parse().unwrap()).collect();
    for d in &lengths[1..] {
        assert!((d - lengths[0]).abs() < 0.03, "the sound runs as long as the picture: {lengths:?}");
    }
    k0 - s0
}

fn smart_cut(name: &str, encoder: &str) {
    glint_core::log::set_sink(|m| eprintln!("log: {m}"));
    let src = long_fixture(name, encoder, 12);
    let out = |what: &str| src.with_file_name(format!("glint-smart-{name}-{what}-{}.mp4", std::process::id()));
    let cut = |start: f64, end: f64, settings: &export::Settings, out: &Path| {
        let mut edit = Edit::new(12 * SECOND, 1);
        let at = |s: f64| (s * SECOND as f64).round() as i64;
        edit.range = Range { start: at(start), end: at(end) };
        let t = std::time::Instant::now();
        let (job, _) = export_to(&src, &edit, settings, out);
        (job, t.elapsed())
    };
    let plain = export::Settings::default();

    let (snap, reordered) = {
        let _com = Com::init();
        (media::probe(&src).unwrap().snap, index::read(&src).unwrap().video.reordered)
    };
    if snap == Some(glint_core::clip::info::Snap::Codec) {
        // windows can't decode or encode it here: the copy starts on the keyframe before, as recorded
        let (job, _) = cut(1.3, 11.2, &plain, &out("snap"));
        let Job::Video(v) = &job else { panic!("{job:?}") };
        assert!(v.video == Video::Copy && v.cut_anew() == 0 && v.pieces[0].lead == 0, "{job:?}");
        assert_eq!(assert_smart_cut(&src, &out("snap"), 0.0, 11.2), 0);
        eprintln!("{name}: windows has no codec for it here, so only the copy from the keyframe before was checked");
        let _ = std::fs::remove_file(out("snap"));
        let _ = std::fs::remove_file(&src);
        return;
    }
    // off a keyframe: encoded up to the one at 2 s, then copied
    let (job, smart_time) = cut(1.3, 11.2, &plain, &out("mid"));
    let Job::Video(v) = &job else { panic!("{job:?}") };
    assert!(v.video == Video::Copy && v.cut_anew() > 0, "{name}: {snap:?} {reordered} {job:?}");
    let anew = assert_smart_cut(&src, &out("mid"), 1.3, 11.2);
    assert_eq!(anew, 42, "1.3 s to 2 s at 60 fps");
    let (job, full_time) = cut(1.3, 11.2, &export::Settings { kbps: Some(12_000), ..plain }, &out("full"));
    assert!(matches!(&job, Job::Video(VideoJob { video: Video::Encode { .. }, .. })));
    eprintln!("{name}: smart cut {smart_time:?}, full re-encode {full_time:?}");
    assert!(smart_time < full_time / 2, "{name}: smart {smart_time:?} vs full {full_time:?}");

    // on a keyframe: a plain copy, packets and sound as recorded
    let (job, _) = cut(4.0, 9.0, &plain, &out("key"));
    let Job::Video(v) = &job else { panic!("{job:?}") };
    assert!(v.video == Video::Copy && v.cut_anew() == 0 && v.copies(&v.tracks[0]), "{job:?}");
    assert_eq!(first_packet_size(&out("key"), 0.0), first_packet_size(&src, 4.0));
    assert_eq!(assert_smart_cut(&src, &out("key"), 4.0, 9.0), 0);

    // inside one keyframe's stretch: all of it encoded
    cut(4.5, 5.5, &plain, &out("gop"));
    assert_eq!(assert_smart_cut(&src, &out("gop"), 4.5, 5.5), 60);
    // from the very start, to the very end
    cut(0.0, 3.0, &plain, &out("start"));
    assert_eq!(assert_smart_cut(&src, &out("start"), 0.0, 3.0), 0);
    cut(10.5, 12.0, &plain, &out("end"));
    assert_eq!(assert_smart_cut(&src, &out("end"), 10.5, 12.0), 90);

    for what in ["mid", "full", "key", "gop", "start", "end"] {
        let _ = std::fs::remove_file(out(what));
    }
    let _ = std::fs::remove_file(&src);
}

#[test]
#[ignore = "needs ffmpeg with nvenc and ffprobe on PATH; writes and deletes big files (GLINT_TEST_DIR)"]
fn an_h264_smart_cut_starts_on_its_frame_and_copies_the_rest() {
    smart_cut("h264", "h264_nvenc");
}

#[test]
#[ignore = "needs ffmpeg with nvenc, ffprobe and windows' hevc codecs; writes and deletes big files (GLINT_TEST_DIR)"]
fn an_hevc_smart_cut_starts_on_its_frame_and_copies_the_rest() {
    smart_cut("hevc", "hevc_nvenc");
}

#[test]
#[ignore = "needs ffmpeg and ffprobe on PATH and a recording in GLINT_SMART_SRC; writes next to GLINT_TEST_DIR"]
fn a_real_recording_smart_cuts() {
    let src = PathBuf::from(std::env::var_os("GLINT_SMART_SRC").expect("set GLINT_SMART_SRC to a glint or afterglow recording"));
    let out = test_dir().join(format!("glint-smart-real-{}.mp4", std::process::id()));
    let info = {
        let _com = Com::init();
        media::probe(&src).unwrap()
    };
    assert_eq!(info.snap, None, "the recording can be smart-cut");
    let (start, end) = (10.3, 30.7);
    let mut edit = Edit::new(info.duration, info.tracks.len());
    edit.range = Range { start: (start * SECOND as f64) as i64, end: (end * SECOND as f64) as i64 };
    let t = std::time::Instant::now();
    let (job, _) = export_to(&src, &edit, &export::Settings::default(), &out);
    let smart_time = t.elapsed();
    let anew = assert_smart_cut(&src, &out, start, end);
    let full = out.with_extension("full.mp4");
    let t = std::time::Instant::now();
    export_to(&src, &edit, &export::Settings { kbps: Some(20_000), ..export::Settings::default() }, &full);
    let full_time = t.elapsed();
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&full);
    let Job::Video(v) = &job else { panic!("{job:?}") };
    eprintln!("{anew} frames ({:.2} s) encoded anew; smart cut {smart_time:?}, full re-encode {full_time:?}", v.cut_anew() as f64 / SECOND as f64);
    assert!(anew > 0 && smart_time < full_time / 2);
}

#[test]
fn windows_always_decodes_h264() {
    let _com = Com::init();
    assert!(media::has_decoder(windows::Win32::Media::MediaFoundation::MFVideoFormat_H264));
}

// ---- self-made files: only media foundation ---------------------------------------------------------

const UNEVEN_SIZE: u32 = 128;

/// A small h.264 file that only gets a frame when "the screen changed", on a 144 fps clock like a game
/// recording: frame `i` comes `GAPS[i % 12]` ticks after the one before and shows its number as the grey of its
/// two halves (ones on the left, twelves on the right). With `gop`, a keyframe comes every that many frames.
fn uneven_file(path: &Path, frames: usize, gop: Option<u32>) {
    use windows::Win32::Media::MediaFoundation::*;
    const GAPS: [i64; 12] = [1, 2, 1, 3, 6, 1, 4, 2, 5, 1, 1, 7];
    let n = UNEVEN_SIZE;
    let video_type = |subtype: &windows::core::GUID| unsafe {
        let t = MFCreateMediaType().unwrap();
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).unwrap();
        t.SetGUID(&MF_MT_SUBTYPE, subtype).unwrap();
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).unwrap();
        t.SetUINT64(&MF_MT_FRAME_SIZE, media::pack(n, n)).unwrap();
        t.SetUINT64(&MF_MT_FRAME_RATE, media::pack(144, 1)).unwrap();
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, media::pack(1, 1)).unwrap();
        t
    };
    unsafe {
        let writer = MFCreateSinkWriterFromURL(&windows::core::HSTRING::from(path.as_os_str()), None, None).unwrap();
        let out = video_type(&MFVideoFormat_H264);
        out.SetUINT32(&MF_MT_AVG_BITRATE, 2_000_000).unwrap();
        let stream = writer.AddStream(&out).unwrap();
        let enc = gop.map(|g| {
            let a = media::attributes(2).unwrap();
            a.SetUINT32(&CODECAPI_AVEncMPVGOPSize, g).unwrap();
            a.SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0).unwrap();
            a
        });
        writer.SetInputMediaType(stream, &video_type(&MFVideoFormat_NV12), enc.as_ref()).unwrap();
        writer.BeginWriting().unwrap();
        let at = |k: i64| k * SECOND / 144;
        let mut k = 0;
        for i in 0..frames {
            let next = k + GAPS[i % 12];
            let len = n * n * 3 / 2;
            let buf = MFCreateMemoryBuffer(len).unwrap();
            let mut p = std::ptr::null_mut();
            buf.Lock(&mut p, None, None).unwrap();
            let pixels = std::slice::from_raw_parts_mut(p, len as usize);
            let (luma, chroma) = pixels.split_at_mut((n * n) as usize);
            for row in luma.chunks_mut(n as usize) {
                let (left, right) = row.split_at_mut(n as usize / 2);
                left.fill(16 + 18 * (i % 12) as u8);
                right.fill(16 + 18 * (i / 12) as u8);
            }
            chroma.fill(128);
            buf.Unlock().unwrap();
            buf.SetCurrentLength(len).unwrap();
            let sample = MFCreateSample().unwrap();
            sample.AddBuffer(&buf).unwrap();
            sample.SetSampleTime(at(k)).unwrap();
            sample.SetSampleDuration(at(next) - at(k)).unwrap();
            writer.WriteSample(stream, &sample).unwrap();
            k = next;
        }
        writer.Finalize().unwrap();
    }
}

/// Each decoded frame's time and the number [`uneven_file`] painted on it.
fn numbered_frames(path: &Path) -> Vec<(i64, usize)> {
    let reader = media::open(path, None).unwrap();
    media::video_only(&reader, windows::Win32::Media::MediaFoundation::MFVideoFormat_NV12, (UNEVEN_SIZE, UNEVEN_SIZE))
        .unwrap();
    let digit = |y: u8| ((y as f64 - 16.0) / 18.0).round() as usize;
    let mut frames = Vec::new();
    while let Some(f) = media::next_video(&reader).unwrap() {
        // the luma plane starts with its top row, whatever the pitch
        let row = media::with_pixels(&f.sample, UNEVEN_SIZE / 4, |rows| rows.row(0).to_vec()).unwrap();
        let half = UNEVEN_SIZE as usize / 2;
        frames.push((f.ts, digit(row[half / 2]) + 12 * digit(row[half + half / 2])));
    }
    frames
}

/// The times media foundation reads straight from the file, before decoding.
fn packet_times(path: &Path) -> Vec<i64> {
    let reader = media::open(path, None).unwrap();
    unsafe {
        reader.SetStreamSelection(media::ALL, false).unwrap();
        reader.SetStreamSelection(media::VIDEO, true).unwrap();
    }
    let mut times = Vec::new();
    while let media::Got::Sample { ts, .. } = media::read(&reader, media::VIDEO).unwrap() {
        times.push(ts);
    }
    times.sort_unstable();
    times
}

#[test]
fn uneven_frames_keep_their_own_times_and_a_lower_rate_picks_them_by_time() {
    let _com = Com::init();
    let dir = std::env::temp_dir();
    let src = dir.join(format!("glint-clip-vfr-{}.mp4", std::process::id()));
    let same_out = dir.join(format!("glint-clip-vfr-same-{}.mp4", std::process::id()));
    let low_out = dir.join(format!("glint-clip-vfr-30-{}.mp4", std::process::id()));
    uneven_file(&src, 96, None);
    let info = media::probe(&src).unwrap();
    let index = index::read(&src).unwrap();
    let source = Source { info: &info, index: Some(&index), bytes: std::fs::metadata(&src).unwrap().len(), accent: ACCENT };
    let edit = Edit::new(info.duration, 0);
    let export = |settings: &export::Settings, out: &Path| {
        let job = export::plan(&edit, settings, &source);
        let (pm, c) = (AtomicU32::new(0), AtomicBool::new(false));
        assert_eq!(trim::export_in_software(&src, &info, &job, out, &Progress { permille: &pm, cancel: &c }), Ok(true));
        job
    };
    let compress = Some(export::Compress::Medium);
    let same = export(&export::Settings { compress, ..export::Settings::default() }, &same_out);
    let low = export(&export::Settings { fps: Some(30), compress, ..export::Settings::default() }, &low_out);
    let (decoded, same_frames, low_frames) = (numbered_frames(&src), numbered_frames(&same_out), numbered_frames(&low_out));
    let packets = packet_times(&src);
    for f in [&src, &same_out, &low_out] {
        let _ = std::fs::remove_file(f);
    }

    assert!(info.fps > 40.0 && info.fps < 60.0, "an average rate: {}", info.fps);
    // the reader's video processor would resample the frames onto a steady clock at that average rate
    assert_eq!(decoded.iter().map(|f| f.0).collect::<Vec<_>>(), packets);
    assert_eq!(decoded.iter().map(|f| f.1).collect::<Vec<_>>(), (0..96).collect::<Vec<_>>());

    assert!(matches!(same, Job::Video(VideoJob { video: Video::Encode { fps: None, .. }, .. })), "{same:?}");
    assert_eq!(same_frames.len(), decoded.len());
    let tick = SECOND as f64 / (info.fps * 1000.0).round();
    for (out, src) in same_frames.iter().zip(&decoded) {
        assert_eq!(out.1, src.1);
        let late = (out.0 - src.0) as f64;
        assert!((0.0..=tick).contains(&late), "frame {} at {} for {}", src.1, out.0, src.0);
    }

    assert!(matches!(low, Job::Video(VideoJob { video: Video::Encode { fps: Some(30), .. }, .. })), "{low:?}");
    assert_eq!(low_frames.len() as i64, (info.duration * 30 + SECOND - 1) / SECOND);
    for (k, &(t, shown)) in low_frames.iter().enumerate() {
        let grid = k as i64 * SECOND / 30;
        assert!((t - grid).abs() <= 1, "frame {k} at {t}");
        let newest = decoded.iter().rev().find(|f| f.0 <= grid).unwrap().1;
        assert_eq!(shown, newest, "at {grid} the frame shown should be the newest one by then");
    }
}

/// The video samples of `path` as stored, in order, with their times.
fn packets(path: &Path) -> Vec<(i64, Vec<u8>)> {
    let reader = media::open(path, None).unwrap();
    unsafe {
        reader.SetStreamSelection(media::ALL, false).unwrap();
        reader.SetStreamSelection(media::VIDEO, true).unwrap();
    }
    let mut out = Vec::new();
    while let media::Got::Sample { ts, sample, .. } = media::read(&reader, media::VIDEO).unwrap() {
        let bytes = unsafe {
            let buf = sample.ConvertToContiguousBuffer().unwrap();
            let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
            buf.Lock(&mut p, None, Some(&mut len)).unwrap();
            let v = std::slice::from_raw_parts(p, len as usize).to_vec();
            buf.Unlock().unwrap();
            v
        };
        out.push((ts, bytes));
    }
    out
}

#[test]
fn a_smart_cut_starts_on_its_frame_and_copies_the_rest_as_recorded() {
    use glint_core::clip::nal;
    let _com = Com::init();
    let dir = std::env::temp_dir();
    let src = dir.join(format!("glint-clip-smart-{}.mp4", std::process::id()));
    let out = dir.join(format!("glint-clip-smart-out-{}.mp4", std::process::id()));
    uneven_file(&src, 96, Some(32));
    let info = media::probe(&src).unwrap();
    let index = index::read(&src).unwrap();
    let source = Source { info: &info, index: Some(&index), bytes: std::fs::metadata(&src).unwrap().len(), accent: ACCENT };
    let frames = numbered_frames(&src);
    let mut edit = Edit::new(info.duration, 0);
    // just after frame 10 starts, so it's the one shown; copied from the keyframe at 32
    let (start, end) = (frames[10].0 + 1, frames[80].0);
    edit.range = Range { start, end };
    let job = export::plan(&edit, &export::Settings::default(), &source);
    let (pm, c) = (AtomicU32::new(0), AtomicBool::new(false));
    let done = trim::export_in_software(&src, &info, &job, &out, &Progress { permille: &pm, cancel: &c });
    let (cut, src_packets, out_packets) = (numbered_frames(&out), packets(&src), packets(&out));
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);

    assert_eq!(info.snap, None, "media foundation's own h.264 can be cut on any frame");
    assert_eq!(index.video.keys, [frames[0].0, frames[32].0, frames[64].0]);
    let Job::Video(v) = &job else { panic!("{job:?}") };
    assert_eq!((v.video, v.pieces[0].lead, v.pieces[0].key), (Video::Copy, start, frames[32].0), "{job:?}");
    assert_eq!(done, Ok(true));
    assert_eq!(cut.iter().map(|f| f.1).collect::<Vec<_>>(), (10..80).collect::<Vec<_>>(), "it starts on frame 10");
    for (k, (t, _)) in cut.iter().enumerate().skip(1) {
        let want = frames[10 + k].0 - start;
        assert!((t - want).abs() <= SECOND / 1000, "frame {} at {t}, recorded {want} after the cut", 10 + k);
    }
    let copied = &out_packets[22..];
    assert_eq!(copied.len(), 80 - 32);
    // the mp4 sink keeps parameter sets out of the samples, so a keyframe is compared by its other units
    let picture = |p: &[u8]| nal::units(p).into_iter().filter(|u| !matches!(u[0] & 0x1F, 7 | 8)).map(<[u8]>::to_vec).collect::<Vec<_>>();
    for (k, (o, s)) in copied.iter().zip(&src_packets[32..80]).enumerate() {
        assert!(picture(&o.1) == picture(&s.1), "packet {} is copied as recorded", 32 + k);
    }
}
