use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

use super::{atom, atoms, child, is_sound, read_moov};

const BLOCK: usize = 32 * 1024;
const WINDOWS: usize = 4;
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

/// A virtual audio-only MP4. Its header is ready before any sample data is read.
/// Track order, codec configuration, sample timestamps and edits are unchanged.
pub struct AudioView {
    header: Vec<u8>,
    len: u64,
    spans: Vec<Span>,
    input: Mutex<Input>,
}

struct Span {
    at: u64,
    source: u64,
    end: u64,
}

struct Window {
    at: u64,
    data: Vec<u8>,
}

struct Input {
    file: File,
    windows: Vec<Window>,
}

impl AudioView {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
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
            .and_then(|n| u32::try_from(n.checked_add(8)?).ok())
            .ok_or("audio-only MP4 is too large")?;
        let header_size = FTYP.len() + moov.len() + 16;
        chunks.sort_unstable_by_key(|c| c.source);
        let mut spans: Vec<Span> = Vec::new();
        let mut at = header_size as u64;
        for chunk in chunks {
            chunk.source.checked_add(chunk.size as u64).filter(|&end| end <= len)
                .ok_or("audio chunk is outside the file")?;
            if chunk.wide {
                moov[chunk.patch..chunk.patch + 8].copy_from_slice(&at.to_be_bytes());
            } else {
                let offset = u32::try_from(at).map_err(|_| "audio chunk offset is too large")?;
                moov[chunk.patch..chunk.patch + 4].copy_from_slice(&offset.to_be_bytes());
            }
            if chunk.size != 0 {
                if let Some(last) = spans.last_mut().filter(|last| last.source + (last.end - last.at) == chunk.source) {
                    last.end += chunk.size as u64;
                } else {
                    spans.push(Span { at, source: chunk.source, end: at + chunk.size as u64 });
                }
            }
            at += chunk.size as u64;
        }

        let mut header = Vec::with_capacity(header_size);
        header.extend_from_slice(FTYP);
        header.extend_from_slice(&((moov.len() + 8) as u32).to_be_bytes());
        header.extend_from_slice(b"moov");
        header.extend_from_slice(&moov);
        header.extend_from_slice(&bytes.to_be_bytes());
        header.extend_from_slice(b"mdat");

        Ok(Self { header, len: at, spans, input: Mutex::new(Input { file, windows: Vec::with_capacity(WINDOWS) }) })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Reads batches of compressed audio, caching recent blocks for interleaved track reads.
    pub fn read_at(&self, mut at: u64, dst: &mut [u8], cancelled: impl Fn() -> bool) -> io::Result<usize> {
        let len = dst.len().min(self.len.saturating_sub(at) as usize);
        let mut dst = &mut dst[..len];
        if at < self.header.len() as u64 {
            let header = &self.header[at as usize..];
            let n = header.len().min(dst.len());
            dst[..n].copy_from_slice(&header[..n]);
            at += n as u64;
            dst = &mut dst[n..];
        }
        if dst.is_empty() {
            return Ok(len);
        }

        let mut input = self.input.lock().map_err(|_| io::Error::other("audio preview read failed"))?;
        while !dst.is_empty() {
            let first = self.header.len() as u64;
            let block = first + (at - first) / BLOCK as u64 * BLOCK as u64;
            let index = input.windows.iter().position(|window| window.at == block);
            let window = match index {
                Some(index) => input.windows.remove(index),
                None => {
                    let mut data = if input.windows.len() == WINDOWS {
                        input.windows.remove(0).data
                    } else {
                        Vec::with_capacity(BLOCK)
                    };
                    data.resize((self.len - block).min(BLOCK as u64) as usize, 0);
                    self.fill(&mut input.file, block, &mut data, &cancelled)?;

                    Window { at: block, data }
                }
            };
            let data = &window.data[(at - block) as usize..];
            let n = data.len().min(dst.len());
            dst[..n].copy_from_slice(&data[..n]);
            input.windows.push(window);
            at += n as u64;
            dst = &mut dst[n..];
        }

        Ok(len)
    }

    fn fill(&self, file: &mut File, mut at: u64, mut dst: &mut [u8], cancelled: &impl Fn() -> bool) -> io::Result<()> {
        let start = self.spans.partition_point(|span| span.end <= at);
        for span in &self.spans[start..] {
            if cancelled() {
                return Err(io::Error::other("audio preview closed"));
            }

            let n = dst.len().min((span.end - at) as usize);
            file.seek(SeekFrom::Start(span.source + at - span.at))?;
            file.read_exact(&mut dst[..n])?;
            at += n as u64;
            dst = &mut dst[n..];
            if dst.is_empty() {
                return Ok(());
            }
        }

        Err(io::ErrorKind::UnexpectedEof.into())
    }
}

#[cfg(test)]
#[path = "audio_view_tests.rs"]
mod tests;
