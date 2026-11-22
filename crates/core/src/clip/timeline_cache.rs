use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use super::math::SECOND;
use crate::gallery;

pub const BUCKET: i64 = SECOND / 100;
pub const THUMB_HEIGHT: u32 = 64;
pub const MAX_THUMBS: usize = 240;
const MAX_CUTS: usize = 100_000;
const MAX_TRACKS: usize = 64;
const MAX_PEAKS: usize = 8 * 60 * 60 * 100;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const DISK_BUDGET: u64 = 512 * 1024 * 1024;
const VERSION: &[u8; 4] = b"gtp1";

#[derive(Clone, Debug, PartialEq)]
pub struct Thumb {
    pub at: i64,
    pub size: (u32, u32),
    pub rgb: Arc<Vec<u8>>,
}

#[derive(Default)]
pub struct Filmstrip {
    pub thumbs: Vec<Thumb>,
    /// Present only after the editor's detailed scan has finished.
    pub cuts: Option<Vec<i64>>,
}

pub struct Cache {
    source: PathBuf,
    stamp: (u64, u64),
    dir: PathBuf,
    key: u64,
}

fn stamp(path: &Path) -> io::Result<(u64, u64)> {
    let meta = fs::metadata(path)?;
    let time = meta.modified()?.duration_since(UNIX_EPOCH).map_err(io::Error::other)?;

    Ok((meta.len(), time.as_nanos() as u64))
}

impl Cache {
    pub fn for_file(path: &Path) -> Option<Self> {
        let dir = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(crate::paths::appdata)
            .join("glint").join("timeline-cache");
        Self::in_dir(path, dir)
    }

    fn in_dir(path: &Path, dir: PathBuf) -> Option<Self> {
        let source = fs::canonicalize(path).ok()?;
        let stamp = stamp(&source).ok()?;
        let key = gallery::path_id(&source);

        Some(Self { source, stamp, dir, key })
    }

    fn file(&self, kind: &str) -> PathBuf {
        self.dir.join(format!("{:016x}.{kind}", self.key))
    }

    fn read<T>(&self, kind: &str, body: impl FnOnce(&mut BufReader<File>) -> io::Result<T>) -> Option<T> {
        let file = File::open(self.file(kind)).ok()?;
        if file.metadata().ok()?.len() > MAX_BYTES {
            return None;
        }

        let mut input = BufReader::new(file);
        if bytes::<4>(&mut input).ok()?.as_slice() != VERSION
            || (u64::from_le_bytes(bytes(&mut input).ok()?), u64::from_le_bytes(bytes(&mut input).ok()?)) != self.stamp
        {
            return None;
        }

        body(&mut input).ok()
    }

    fn write(&self, kind: &str, body: impl FnOnce(&mut BufWriter<File>) -> io::Result<()>) -> io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(&self.dir)?;
        let target = self.file(kind);
        let temp = target.with_extension(format!("{kind}.{}.{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let result = (|| {
            let mut out = BufWriter::new(OpenOptions::new().create_new(true).write(true).open(&temp)?);
            out.write_all(VERSION)?;
            out.write_all(&self.stamp.0.to_le_bytes())?;
            out.write_all(&self.stamp.1.to_le_bytes())?;
            body(&mut out)?;
            out.flush()?;
            let len = out.get_ref().metadata()?.len();
            drop(out);
            if len > MAX_BYTES || stamp(&self.source)? != self.stamp {
                return Err(invalid());
            }

            fs::rename(&temp, &target)?;
            self.sweep(&target);

            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }

        result
    }

    fn sweep(&self, keep: &Path) {
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        let mut total = 0u64;
        let mut old = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !matches!(path.extension().and_then(|s| s.to_str()), Some("waves" | "filmstrip"))
                || !entry.file_type().is_ok_and(|t| t.is_file())
            {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            total = total.saturating_add(meta.len());
            if path != keep {
                old.push((meta.modified().unwrap_or(UNIX_EPOCH), meta.len(), path));
            }
        }
        if total <= DISK_BUDGET {
            return;
        }

        old.sort_unstable();
        for (_, len, path) in old {
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(len);
            }
            if total <= DISK_BUDGET {
                break;
            }
        }
    }

    pub fn waves(&self, tracks: usize) -> Option<Vec<Vec<u8>>> {
        self.read("waves", |input| {
            if count(input, MAX_TRACKS)? != tracks {
                return Err(invalid());
            }

            (0..tracks).map(|_| {
                let n = count(input, MAX_PEAKS)?;
                let mut data = vec![0; n];
                input.read_exact(&mut data)?;

                Ok(data)
            }).collect()
        })
    }

    pub fn save_waves(&self, tracks: &[Vec<u8>]) -> io::Result<()> {
        if tracks.len() > MAX_TRACKS || tracks.iter().any(|t| t.len() > MAX_PEAKS) {
            return Err(invalid());
        }

        self.write("waves", |out| {
            number(out, tracks.len())?;
            for track in tracks {
                number(out, track.len())?;
                out.write_all(track)?;
            }

            Ok(())
        })
    }

    pub fn filmstrip(&self) -> Option<Filmstrip> {
        self.read("filmstrip", |input| {
            let cuts = match bytes::<1>(input)?[0] {
                0 => None,
                1 => Some((0..count(input, MAX_CUTS)?).map(|_| read_time(input)).collect::<io::Result<Vec<_>>>()?),
                _ => return Err(invalid()),
            };
            let n = count(input, MAX_THUMBS + 1)?;
            let mut thumbs = Vec::with_capacity(n);
            for _ in 0..n {
                let at = read_time(input)?;
                let size = (u32::from_le_bytes(bytes(input)?), u32::from_le_bytes(bytes(input)?));
                let mut rgb = vec![0; pixels(size)?];
                input.read_exact(&mut rgb)?;
                if thumbs.last().is_some_and(|prev: &Thumb| prev.at >= at || prev.size != size) {
                    return Err(invalid());
                }
                thumbs.push(Thumb { at, size, rgb: Arc::new(rgb) });
            }

            Ok(Filmstrip { thumbs, cuts })
        })
    }

    pub fn save_filmstrip(&self, strip: &Filmstrip) -> io::Result<()> {
        if strip.thumbs.len() > MAX_THUMBS + 1 || strip.cuts.as_ref().is_some_and(|c| c.len() > MAX_CUTS) {
            return Err(invalid());
        }

        self.write("filmstrip", |out| {
            out.write_all(&[u8::from(strip.cuts.is_some())])?;
            if let Some(cuts) = &strip.cuts {
                number(out, cuts.len())?;
                for &at in cuts {
                    out.write_all(&at.to_le_bytes())?;
                }
            }
            number(out, strip.thumbs.len())?;
            for thumb in &strip.thumbs {
                if thumb.rgb.len() != pixels(thumb.size)? {
                    return Err(invalid());
                }
                out.write_all(&thumb.at.to_le_bytes())?;
                out.write_all(&thumb.size.0.to_le_bytes())?;
                out.write_all(&thumb.size.1.to_le_bytes())?;
                out.write_all(&thumb.rgb)?;
            }

            Ok(())
        })
    }
}

fn pixels((w, h): (u32, u32)) -> io::Result<usize> {
    if !(1..=512).contains(&w) || !(1..=THUMB_HEIGHT).contains(&h) {
        return Err(invalid());
    }

    Ok((w * h * 3) as usize)
}

fn bytes<const N: usize>(input: &mut impl Read) -> io::Result<[u8; N]> {
    let mut data = [0; N];
    input.read_exact(&mut data)?;

    Ok(data)
}

fn read_time(input: &mut impl Read) -> io::Result<i64> {
    let at = i64::from_le_bytes(bytes(input)?);
    if at < 0 {
        return Err(invalid());
    }

    Ok(at)
}

fn count(input: &mut impl Read, max: usize) -> io::Result<usize> {
    let n = u32::from_le_bytes(bytes(input)?) as usize;
    if n > max {
        return Err(invalid());
    }

    Ok(n)
}

fn number(out: &mut impl Write, n: usize) -> io::Result<()> {
    out.write_all(&(n as u32).to_le_bytes())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid timeline cache")
}

#[cfg(test)]
#[path = "timeline_cache_tests.rs"]
mod tests;
