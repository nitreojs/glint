//! Following a text log another program writes: the lines added since the last look, read
//! without holding the file open between looks, so the writer can replace it any time.

use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::PathBuf;

/// The bytes that tell one file from the next one put in its place (a game's log starts with a
/// time stamp).
const HEAD: usize = 64;
/// The most read in one poll, so catching up on a long file is spread over polls.
const MOST: u64 = 1 << 20;
/// A line this long without an end is handed over anyway.
const LONGEST: usize = 64 << 10;

pub struct Tail {
    path: PathBuf,
    /// Bytes read so far.
    pos: u64,
    /// The file's first bytes, as far as they were there.
    head: Vec<u8>,
    /// The end of the last read, not yet ended by a newline.
    partial: Vec<u8>,
    chunk: Vec<u8>,
}

impl Tail {
    /// Follows `path` from its first byte, or from when it appears.
    pub fn new(path: PathBuf) -> Tail {
        Tail { path, pos: 0, head: Vec::new(), partial: Vec::new(), chunk: Vec::new() }
    }

    /// Appends the lines finished since the last poll to `out`: without their line end (`\n` or
    /// `\r\n`) or a byte-order mark, invalid UTF-8 replaced. A file that's replaced (shorter, or
    /// starting differently) is read again from its start, and a missing one is waited for.
    pub fn poll(&mut self, out: &mut Vec<String>) -> Result<(), String> {
        let err = |e: std::io::Error| format!("{}: {e}", self.path.display());
        let mut f = match File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(err(e)),
        };
        let len = f.metadata().map_err(err)?.len();
        let mut head = [0; HEAD];
        let n = fill(&mut f, &mut head[..len.min(HEAD as u64) as usize]).map_err(err)?;
        let common = n.min(self.head.len());
        let reset = len < self.pos || head[..common] != self.head[..common];
        if reset {
            self.pos = 0;
            self.partial.clear();
        }
        if reset || n > self.head.len() {
            self.head.clear();
            self.head.extend_from_slice(&head[..n]);
        }
        if len <= self.pos {
            return Ok(());
        }
        f.seek(SeekFrom::Start(self.pos)).map_err(err)?;
        self.chunk.clear();
        f.take(MOST).read_to_end(&mut self.chunk).map_err(err)?;
        self.pos += self.chunk.len() as u64;
        let mut rest = &self.chunk[..];
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            self.partial.extend_from_slice(&rest[..i]);
            out.push(line(&self.partial));
            self.partial.clear();
            rest = &rest[i + 1..];
        }
        self.partial.extend_from_slice(rest);
        if self.partial.len() > LONGEST {
            out.push(line(&self.partial));
            self.partial.clear();
        }
        Ok(())
    }
}

/// Reads into all of `buf` unless the file ends first; how much it read.
fn fill(f: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

fn line(bytes: &[u8]) -> String {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn polled(t: &mut Tail) -> Vec<String> {
        let mut out = Vec::new();
        t.poll(&mut out).unwrap();
        out
    }

    fn append(path: &PathBuf, bytes: &[u8]) {
        std::fs::OpenOptions::new().append(true).create(true).open(path).unwrap().write_all(bytes).unwrap();
    }

    #[test]
    fn it_follows_appends_partial_lines_and_a_file_put_in_its_place() {
        let path = std::env::temp_dir().join(format!("glint_tail_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut t = Tail::new(path.clone());
        assert!(polled(&mut t).is_empty(), "no file yet");

        append(&path, "\u{feff}10/26 18:53:30 first\r\nsecond\r\nthi".as_bytes());
        assert_eq!(polled(&mut t), ["10/26 18:53:30 first", "second"], "the BOM and line ends go");
        assert!(polled(&mut t).is_empty(), "the partial line waits");
        append(&path, b"rd\nfourth\r");
        assert_eq!(polled(&mut t), ["third"]);
        append(&path, b"\n");
        assert_eq!(polled(&mut t), ["fourth"], "a \\r\\n split between reads");

        // the game starts again: a new, longer file in its place, its partial line dropped
        append(&path, b"half a li");
        polled(&mut t);
        let restarted = format!("\u{feff}10/26 19:10:02 new run\r\n{}\r\n", "x".repeat(400));
        std::fs::write(&path, &restarted).unwrap();
        assert_eq!(polled(&mut t), ["10/26 19:10:02 new run".to_string(), "x".repeat(400)]);

        // and a shorter one
        std::fs::write(&path, "\u{feff}10/26 19:10:02 new run\r\n").unwrap();
        assert_eq!(polled(&mut t), ["10/26 19:10:02 new run"], "same start, but shorter than what was read");
        append(&path, b"bad \xff byte\n");
        assert_eq!(polled(&mut t), ["bad \u{fffd} byte"]);

        std::fs::remove_file(&path).unwrap();
        assert!(polled(&mut t).is_empty(), "gone: nothing, no error");
    }

    #[test]
    fn a_long_file_is_read_over_several_polls() {
        let path = std::env::temp_dir().join(format!("glint_tail_long_{}.log", std::process::id()));
        let row = format!("{}\n", "y".repeat(1023));
        std::fs::write(&path, row.repeat(1536)).unwrap();
        let mut t = Tail::new(path.clone());
        let (first, second) = (polled(&mut t).len(), polled(&mut t).len());
        std::fs::remove_file(&path).unwrap();
        assert_eq!((first, second), (1024, 512));
    }
}
