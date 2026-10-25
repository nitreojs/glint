//! The gallery's thumbnail cache: one small file per capture, named by a key of the capture's
//! path, size and last write, so a changed file gets a new thumbnail and the old one is swept.
//! A file is a header ([`Meta`] of the original) followed by the encoded image.

use std::collections::HashSet;
use std::path::Path;

use super::{fnv, normalized};

/// Bumped when thumbnails are made differently, so old ones are remade.
const VERSION: u8 = 1;
const MAGIC: &[u8; 4] = b"glt1";
const HEADER: usize = 20;
pub const EXT: &str = "thumb";

/// What the thumbnail knows of its original.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// The original's size in pixels.
    pub width: u32,
    pub height: u32,
    /// How long it plays, 0 for a still.
    pub duration_ms: u32,
    /// The thumbnail keeps transparency (stored as png, not jpg).
    pub alpha: bool,
}

pub fn key(path: &Path, size: u64, modified: u64) -> u64 {
    let bytes = normalized(path).into_bytes();
    fnv(bytes.into_iter().chain(size.to_le_bytes()).chain(modified.to_le_bytes()).chain([VERSION]))
}

pub fn file_name(key: u64) -> String {
    format!("{key:016x}.{EXT}")
}

/// The cache file's bytes: the header, then `image`.
pub fn pack(meta: Meta, image: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER + image.len());
    out.extend_from_slice(MAGIC);
    for v in [meta.width, meta.height, meta.duration_ms, u32::from(meta.alpha)] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(image);
    out
}

/// The header and the encoded image of a cache file; `None` if it isn't one.
pub fn unpack(bytes: &[u8]) -> Option<(Meta, &[u8])> {
    let (head, image) = (bytes.get(..HEADER)?, bytes.get(HEADER..)?);
    if &head[..4] != MAGIC || image.is_empty() {
        return None;
    }
    let word = |i: usize| u32::from_le_bytes([head[4 + 4 * i], head[5 + 4 * i], head[6 + 4 * i], head[7 + 4 * i]]);
    Some((Meta { width: word(0), height: word(1), duration_ms: word(2), alpha: word(3) != 0 }, image))
}

/// A thumbnail's size for a `w` x `h` original: its shorter side `short` (never bigger than the
/// original), its longer side at most `long`.
pub fn size_for(w: u32, h: u32, short: u32, long: u32) -> (u32, u32) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let scale = (short as f64 / w.min(h)).min(long as f64 / w.max(h)).min(1.0);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

/// The cache files among `names` that belong to none of the `live` keys.
pub fn stale<'a>(names: impl IntoIterator<Item = &'a str>, live: &HashSet<u64>) -> Vec<&'a str> {
    names
        .into_iter()
        .filter(|name| {
            let Some(hex) = name.strip_suffix(EXT).and_then(|s| s.strip_suffix('.')) else { return false };
            u64::from_str_radix(hex, 16).is_ok_and(|k| hex.len() == 16 && !live.contains(&k))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_changes_with_the_file_and_nothing_else() {
        let p = Path::new(r"C:\caps\a.png");
        let k = key(p, 1000, 55);
        assert_eq!(k, key(Path::new("c:/CAPS/A.png"), 1000, 55));
        assert_ne!(k, key(p, 1001, 55)); // rewritten
        assert_ne!(k, key(p, 1000, 56)); // touched
        assert_ne!(k, key(Path::new(r"C:\caps\b.png"), 1000, 55));
        assert_eq!(file_name(0xab), "00000000000000ab.thumb");
        assert_eq!(file_name(k).len(), 22);
    }

    #[test]
    fn cache_files_round_trip_and_reject_garbage() {
        let meta = Meta { width: 2560, height: 1440, duration_ms: 12_345, alpha: true };
        let bytes = pack(meta, b"jpegdata");
        assert_eq!(unpack(&bytes), Some((meta, &b"jpegdata"[..])));
        assert_eq!(unpack(&bytes[..HEADER]), None);
        assert_eq!(unpack(b"glt1"), None);
        let mut bad = bytes.clone();
        bad[0] = b'x';
        assert_eq!(unpack(&bad), None);
    }

    #[test]
    fn thumbnails_cover_their_short_side_without_growing() {
        assert_eq!(size_for(2560, 1440, 280, 1024), (498, 280));
        assert_eq!(size_for(400, 1200, 280, 1024), (280, 840));
        assert_eq!(size_for(400, 4000, 280, 1024), (102, 1024)); // a very tall one keeps its long side in bounds
        assert_eq!(size_for(200, 100, 280, 1024), (200, 100));
        assert_eq!(size_for(0, 0, 280, 1024), (1, 1));
    }

    #[test]
    fn sweeping_leaves_live_thumbnails_and_other_files_alone() {
        let live: HashSet<u64> = [1, 0xfeed].into_iter().collect();
        let names = [
            file_name(1),
            file_name(2),
            file_name(0xfeed),
            "notes.txt".to_string(),
            "zz.thumb".to_string(),
            "0000000000000003.thumb.tmp".to_string(),
        ];
        let gone = stale(names.iter().map(String::as_str), &live);
        assert_eq!(gone, [file_name(2)]);
    }
}
