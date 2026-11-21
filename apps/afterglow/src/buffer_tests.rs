use std::fs;

use super::reserve_output;

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
