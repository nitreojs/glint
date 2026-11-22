use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::{atom, atoms, child, is_sound, read_moov};

const LIMIT: usize = 32 * 1024 * 1024;
const FTYP: &[u8] = b"\0\0\0\x18ftypisom\0\0\x02\0isommp42";

struct Chunk {
    source: u64,
    size: usize,
    patch: usize,
    wide: bool,
}

fn word(data: &[u8], at: usize) -> Result<u32, String> {
    let bytes = data.get(at..at + 4).ok_or("truncated MP4 audio table")?;

    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

fn audio_chunks(stbl: &[u8], base: usize, chunks: &mut Vec<Chunk>) -> Result<(), String> {
    let stsc = child(stbl, b"stsc").ok_or("no audio sample-to-chunk table")?;
    let stsz = child(stbl, b"stsz").ok_or("no audio sample sizes")?;
    let offsets = atoms(stbl)?.into_iter().find(|a| matches!(&a.kind, b"stco" | b"co64"))
        .ok_or("no audio chunk offsets")?;
    let wide = &offsets.kind == b"co64";
    let stride = if wide { 8 } else { 4 };
    let data = &stbl[offsets.body..offsets.end];
    let count = word(data, 4)? as usize;
    let samples = word(stsz, 8)? as usize;
    let fixed = word(stsz, 4)? as usize;
    let runs = word(stsc, 4)? as usize;
    let mut run = 0;
    let mut per_chunk = 0;
    let mut sample = 0usize;
    for i in 0..count {
        while run < runs && word(stsc, 8 + run * 12)? as usize == i + 1 {
            per_chunk = word(stsc, 12 + run * 12)? as usize;
            run += 1;
        }
        let Some(end) = sample.checked_add(per_chunk).filter(|&end| per_chunk != 0 && end <= samples) else {
            return Err("invalid MP4 audio chunk table".into());
        };
        let size = if fixed == 0 {
            let mut size = 0u64;
            for s in sample..end {
                size += u64::from(word(stsz, 12 + s * 4)?);
            }
            usize::try_from(size).map_err(|_| "audio chunk is too large")?
        } else {
            fixed.checked_mul(per_chunk).ok_or("audio chunk is too large")?
        };
        sample = end;
        let at = 8 + i * stride;
        let high = u64::from(word(data, at)?);
        let source = if wide { (high << 32) | u64::from(word(data, at + 4)?) } else { high };
        chunks.push(Chunk { source, size, patch: base + offsets.body + at, wide });
    }
    if run != runs || sample != samples {
        return Err("incomplete MP4 audio chunk table".into());
    }

    Ok(())
}

fn table_range(trak: &[u8]) -> Result<std::ops::Range<usize>, String> {
    let mut range = 0..trak.len();
    for kind in [b"mdia", b"minf", b"stbl"] {
        if kind == b"stbl" {
            let minf = &trak[range.clone()];
            let dref = child(minf, b"dinf").and_then(|d| child(d, b"dref"))
                .and_then(|d| d.get(8..)).ok_or("missing MP4 audio data references")?;
            for (kind, data) in super::boxes(dref)? {
                if &kind != b"url " || word(data, 0)? & 1 == 0 {
                    return Err("MP4 audio uses external data references".into());
                }
            }
        }
        let a = atoms(&trak[range.clone()])?.into_iter().find(|a| &a.kind == kind)
            .ok_or("missing MP4 audio sample table")?;
        range = range.start + a.body..range.start + a.end;
    }

    Ok(range)
}

/// Repackages the unchanged sound tracks into a bounded, audio-only MP4 in memory.
/// Track order, codec configuration, sample timestamps, and edits are preserved.
pub fn audio_view(path: &Path, cancelled: impl Fn() -> bool) -> Result<Vec<u8>, String> {
    let io = |e: std::io::Error| e.to_string();
    let mut file = File::open(path).map_err(io)?;
    let (_, len, original) = read_moov(&mut file)?;
    let original = child(&original, b"moov").ok_or("missing MP4 movie header")?;
    if child(original, b"mvex").is_some() {
        return Err("fragmented MP4 has no complete sample table".into());
    }

    let movie = child(original, b"mvhd").ok_or("missing MP4 movie header")?;
    let mut moov = atom(b"mvhd", movie);
    let mut chunks = Vec::new();
    for a in atoms(original)?.into_iter().filter(|a| &a.kind == b"trak") {
        let track = &original[a.body..a.end];
        if !is_sound(track) {
            continue;
        }

        let range = table_range(track)?;
        let base = moov.len() + (a.body - a.start) + range.start;
        audio_chunks(&track[range], base, &mut chunks)?;
        moov.extend_from_slice(&original[a.start..a.end]);
    }
    if chunks.is_empty() {
        return Err("no indexed audio samples".into());
    }

    let bytes = chunks.iter().try_fold(0usize, |total, c| total.checked_add(c.size))
        .filter(|&bytes| bytes <= LIMIT)
        .ok_or("audio data exceeds the preview buffer")?;
    let payload = FTYP.len() + 8;
    let mut view = Vec::with_capacity(payload + bytes + 8 + moov.len());
    view.extend_from_slice(FTYP);
    view.extend_from_slice(&((bytes + 8) as u32).to_be_bytes());
    view.extend_from_slice(b"mdat");
    chunks.sort_unstable_by_key(|c| c.source);
    let mut source = u64::MAX;
    for chunk in chunks {
        if cancelled() {
            return Err("audio preview closed".into());
        }

        let end = chunk.source.checked_add(chunk.size as u64).filter(|&end| end <= len)
            .ok_or("audio chunk is outside the file")?;
        if source != chunk.source {
            file.seek(SeekFrom::Start(chunk.source)).map_err(io)?;
        }

        let at = view.len();
        view.resize(at + chunk.size, 0);
        file.read_exact(&mut view[at..]).map_err(io)?;
        source = end;
        if chunk.wide {
            moov[chunk.patch..chunk.patch + 8].copy_from_slice(&(at as u64).to_be_bytes());
        } else {
            moov[chunk.patch..chunk.patch + 4].copy_from_slice(&(at as u32).to_be_bytes());
        }
    }
    view.extend_from_slice(&((moov.len() + 8) as u32).to_be_bytes());
    view.extend_from_slice(b"moov");
    view.extend_from_slice(&moov);

    Ok(view)
}
