use glint_core::clip::math::SECOND;
use glint_core::clip::timeline_cache::Cache;
use glint_core::record::mix::RATE;

use super::*;
use crate::clip::media::Com;

fn record<O: Output<Frame = IMFSample>>(frames: &mut Composer, output: &mut O, ring: &Ring) -> Result<(), String> {
    ring.publish();
    frames.latch(&0)?;
    for i in 0..30 {
        let frame = loop {
            if let Some(frame) = frames.alloc()? {
                break frame;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        frames.compose(&frame, None, &[], None)?;
        let at = i * SECOND / 30;
        if let Some(thumb) = frames.preview_frame(&frame, at) {
            output.preview_thumb(thumb);
        }
        output.video(frame, at, (i + 1) * SECOND / 30 - at)?;
    }
    if let Some(thumb) = frames.finish_preview() {
        output.preview_thumb(thumb);
    }

    let pcm: Vec<_> = (0..RATE).flat_map(|i| {
        let v = if i < RATE / 2 { 0 } else { 16_384 };

        [v, -v]
    }).collect();
    for (i, chunk) in pcm.chunks(1_920).enumerate() {
        output.audio(0, (i * 960) as i64, chunk)?;
    }
    output.finish()
}

fn assert_preview(path: &Path) {
    let cache = Cache::for_file(path).unwrap();
    let filmstrip = cache.filmstrip().unwrap();
    let waveform = cache.waves(1).unwrap();
    assert!(filmstrip.cuts.is_none());
    assert_eq!(filmstrip.thumbs[0].at, 0);
    let thumb = &filmstrip.thumbs[0];
    let (w, h) = thumb.size;
    for (k, &expected) in PATCHES.iter().enumerate() {
        let x = (k as u32 % 4) * w / 4 + w / 8;
        let y = (k as u32 / 4) * h / 2 + h / 4;
        let at = ((y * w + x) * 3) as usize;
        let rgb = &thumb.rgb[at..at + 3];
        assert!(rgb.iter().zip(expected).all(|(&seen, want)| seen.abs_diff(want) <= 10), "{rgb:?} vs {expected:?}");
    }
    assert_eq!(&waveform[0][10..45], &[1; 35]);
    assert!(waveform[0][55..95].iter().all(|&peak| peak > 165));
}

#[test]
#[ignore = "needs a Windows D3D11 adapter and Media Foundation encoders"]
fn timeline_preview_is_written_by_the_recorder_and_the_replay_packet_path() {
    let _com = Com::init();
    let size = (640, 360);
    let path = temp_mp4("timeline-preview-recording");
    let p = params(Codec::H264, size, &path);
    let gpu = gpu_sized(size);
    let ring = Ring::new(size);
    let (mut out, route, _, _) = unsafe { sink_writer(&p, &gpu, Force::default(), vec!["tone".into()]) }.unwrap();
    let mut frames = unsafe { Composer::new(&p, &[ring.handle], gpu, route) }.unwrap();
    record(&mut frames, &mut out, &ring).unwrap();
    out.save_preview(&path, SECOND);
    assert_preview(&path);
    drop(out);
    let _ = std::fs::remove_file(&path);

    let path = temp_mp4("timeline-preview-replay");
    let gpu = gpu_sized(size);
    let sink = ReplaySink::new(Limits { keep: 2 * SECOND, max_bytes: 64 * 1024 * 1024 });
    let audio = vec![("tone".into(), AacEncoder::new().unwrap())];
    let video = MfVideoEncoder::new(&gpu, Force::default());
    let mut out = Encoded::new(video, &video_config(&p), audio, sink.clone()).unwrap();
    let route = out.video_encoder().route().unwrap();
    let mut frames = unsafe { Composer::new(&p, &[ring.handle], gpu, route) }.unwrap();
    record(&mut frames, &mut out, &ring).unwrap();
    let mut clip = sink.lock().last(SECOND).unwrap();
    let preview = std::mem::take(&mut clip.preview);
    let mut file = Mp4Sink::create(&path).unwrap();
    clip.write(&mut file).unwrap();
    preview.save(&path);
    assert_preview(&path);
    drop(file);
    let _ = std::fs::remove_file(&path);
}
