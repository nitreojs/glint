use super::*;

#[test]
fn audio_view_reads_only_requested_windows_and_skips_interleaved_bytes() {
    let path = std::env::temp_dir().join(format!("glint-audio-view-{}", std::process::id()));
    let split = BLOCK / 2;
    let skip = 500;
    let payload = [vec![10; split], vec![99; skip], vec![20; BLOCK]].concat();
    std::fs::write(&path, payload).unwrap();
    let header = vec![1, 2, 3];
    let start = header.len() as u64;
    let len = start + (split + BLOCK) as u64;
    let view = AudioView {
        header,
        len,
        spans: vec![
            Span { at: start, source: 0, end: start + split as u64 },
            Span { at: start + split as u64, source: (split + skip) as u64, end: len },
        ],
        input: Mutex::new(Input { file: File::open(&path).unwrap(), windows: Vec::new() }),
    };
    let mut bytes = [0; 5];
    let n = view.read_at(0, &mut bytes, || false).unwrap();
    assert_eq!((n, bytes), (5, [1, 2, 3, 10, 10]));

    let n = view.read_at(start + split as u64 - 2, &mut bytes, || true).unwrap();
    assert_eq!((n, bytes), (5, [10, 10, 20, 20, 20]));
    assert!(view.read_at(len - 2, &mut bytes, || true).is_err());
    let n = view.read_at(len - 2, &mut bytes, || false).unwrap();
    assert_eq!(n, 2);
    assert_eq!(&bytes[..n], &[20, 20]);
    drop(view);
    std::fs::remove_file(path).unwrap();
}
