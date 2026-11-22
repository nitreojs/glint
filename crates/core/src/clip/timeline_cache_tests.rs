use std::sync::Arc;

use super::*;
use crate::record::preview::CapturePreview;

#[test]
fn timeline_cache_reuses_both_tracks_until_the_source_changes() {
    let dir = std::env::temp_dir().join(format!("glint-timeline-cache-{}", std::process::id()));
    let path = dir.join("capture.mp4");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&path, b"first video").unwrap();
    let cache = Cache::in_dir(&path, dir.clone()).unwrap();
    let mut preview = CapturePreview::default();
    preview.add_track();
    preview.add_track();
    preview.audio(0, 480, &[i16::MIN; 960]);
    preview.audio(1, 960, &[8_192; 960]);
    preview.thumbnail(Thumb { at: 2 * BUCKET, size: (1, 1), rgb: Arc::new(vec![30, 90, 200]) });
    preview.trim(BUCKET);
    let snapshot = preview.snapshot(BUCKET, 4 * BUCKET, &[(0, BUCKET), (1, 2 * BUCKET)]);
    let (thumbs, waves) = snapshot.parts();
    let waves = waves.unwrap();
    assert_eq!(waves, [vec![255, 1, 1], vec![1, 128, 1]]);
    assert_eq!(thumbs[0].at, BUCKET);

    cache.save_waves(&waves).unwrap();
    cache.save_filmstrip(&Filmstrip { thumbs: thumbs.clone(), cuts: Some(vec![BUCKET]) }).unwrap();
    let strip = cache.filmstrip().unwrap();
    assert_eq!(cache.waves(2).unwrap(), waves);
    assert!(cache.waves(1).is_none());
    assert_eq!(strip.thumbs, thumbs);
    assert_eq!(strip.cuts, Some(vec![BUCKET]));

    fs::write(&path, b"a replacement video").unwrap();
    let changed = Cache::in_dir(&path, dir.clone()).unwrap();
    assert!(changed.waves(2).is_none());
    assert!(changed.filmstrip().is_none());
    assert!(cache.save_waves(&waves).is_err());
    fs::remove_dir_all(dir).unwrap();
}
