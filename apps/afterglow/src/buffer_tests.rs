use std::fs;
use std::sync::Arc;

use glint_core::clip::math::SECOND;
use glint_core::clip::timeline_cache::{Cache, Thumb};
use glint_core::record::Codec;
use glint_core::record::negotiate::Matrix;
use glint_core::record::pipeline::{AudioEncoder, Packet, PacketSink, Stream, VideoConfig};
use glint_core::record::replay::{Limits, ReplaySink};
use glint_media::clip::media::{Com, probe};
use glint_media::record::AacEncoder;

use super::{reserve_output, write};

#[test]
fn a_new_save_does_not_reuse_a_partial_clips_path() {
    let dir = std::env::temp_dir().join(format!("afterglow-save-reservation-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();

    let existing = dir.join("clip.mp4.part");
    fs::write(&existing, b"recoverable data").unwrap();
    let paths = [dir.join("clip.mp4"), dir.join("clip_2.mp4")];

    let (final_path, part) = reserve_output(paths.into_iter()).unwrap();

    assert_eq!(fs::read(&existing).unwrap(), b"recoverable data");
    assert_eq!(final_path, dir.join("clip_2.mp4"));
    assert_eq!(part, dir.join("clip_2.mp4.part"));
    assert!(part.is_file());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "needs ffmpeg on PATH and Windows Media Foundation"]
fn saving_a_replay_publishes_both_previews_at_the_final_path() {
    let _com = Com::init();
    let dir = std::env::temp_dir().join(format!("afterglow-timeline-save-{}", std::process::id()));
    let video = std::process::Command::new("ffmpeg").args([
        "-nostdin", "-v", "error", "-f", "lavfi", "-i", "color=size=64x64:rate=1",
        "-frames:v", "1", "-c:v", "libx264", "-threads:v", "1", "-f", "h264", "pipe:1",
    ]).output().unwrap();
    assert!(video.status.success(), "{}", String::from_utf8_lossy(&video.stderr));

    let mut sink = ReplaySink::new(Limits { keep: SECOND, max_bytes: 1_000_000 });
    let config = VideoConfig {
        codec: Codec::H264, width: 64, height: 64, fps: 1.0, qp: 20, effort: 20, keyframe_secs: 1.0, bitrate: None,
    };
    let vid = sink.add_stream(Stream::Video { config, headers: Vec::new(), matrix: Matrix::Bt709 }).unwrap();
    let mut encoder = AacEncoder::new().unwrap();
    let aud = sink.add_stream(encoder.configure("game").unwrap()).unwrap();
    sink.write(vid, Packet { data: video.stdout, pts: 0, dts: 0, duration: SECOND, key: true }).unwrap();
    sink.preview_thumb(Thumb { at: 0, size: (1, 1), rgb: Arc::new(vec![1, 2, 3]) });
    let mut packets = Vec::new();
    let pcm = [16_384i16; 1_920];
    for i in 0..50 {
        let at = i * 960;
        sink.preview_audio(aud, at, &pcm);
        encoder.encode(at, &pcm, &mut packets).unwrap();
    }
    encoder.finish(&mut packets).unwrap();
    for packet in packets {
        sink.write(aud, packet).unwrap();
    }
    let clip = sink.lock().last(SECOND).unwrap();

    let path = write(42, clip, &dir.join("replay")).unwrap();
    let info = probe(&path).unwrap();
    let cache = Cache::for_file(&path).unwrap();
    let strip = cache.filmstrip().unwrap();
    let waves = cache.waves(info.tracks.len()).unwrap();
    assert_eq!(path, dir.join("replay.mp4"));
    assert_eq!(&*strip.thumbs[0].rgb, &[1, 2, 3]);
    assert_eq!(waves.len(), 1);
    assert!(waves[0][10..90].iter().all(|&v| v > 165));
    fs::remove_dir_all(dir).unwrap();
}
