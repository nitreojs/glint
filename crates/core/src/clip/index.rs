use std::path::Path;

use super::math::SECOND;
use crate::record::mp4::{boxes, child, moov};

/// One track's samples by presentation time, with running byte totals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Samples {
    times: Vec<i64>,
    before: Vec<u64>,
    total: u64,
    pub keys: Vec<i64>,
}

impl Samples {
    pub(super) fn new(mut list: Vec<(i64, u32, bool)>) -> Samples {
        list.sort_by_key(|s| s.0);
        let mut total = 0;
        let mut before = Vec::with_capacity(list.len());
        for s in &list {
            before.push(total);
            total += s.1 as u64;
        }
        let keys = list.iter().filter(|s| s.2).map(|s| s.0).collect();
        Samples { times: list.iter().map(|s| s.0).collect(), before, total, keys }
    }

    fn at(&self, t: i64) -> u64 {
        let i = self.times.partition_point(|&s| s < t);
        self.before.get(i).copied().unwrap_or(self.total)
    }

    /// Bytes of the samples starting in `from..to`.
    pub(super) fn bytes(&self, from: i64, to: i64) -> u64 {
        if to <= from { 0 } else { self.at(to) - self.at(from) }
    }

    pub(super) fn total(&self) -> u64 {
        self.total
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Index {
    pub video: Samples,
    /// Sound tracks in file order, as `Info::tracks`.
    pub audio: Vec<Samples>,
}

pub fn read(path: &Path) -> Result<Index, String> {
    parse(&moov(path)?)
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let bytes = self.data.get(self.at..self.at + N).ok_or("truncated sample table")?;
        self.at += N;
        Ok(bytes.try_into().unwrap())
    }
    fn u32(&mut self) -> Result<u32, String> {
        self.take::<4>().map(u32::from_be_bytes)
    }
    fn u64(&mut self) -> Result<u64, String> {
        self.take::<8>().map(u64::from_be_bytes)
    }
}

fn reader(data: &[u8]) -> Reader<'_> {
    Reader { data, at: 0 }
}

fn timescale(header: &[u8], ver_skip: (usize, usize)) -> Result<u32, String> {
    let mut r = reader(header);
    let version = r.u32()? >> 24;
    r.at += if version == 1 { ver_skip.1 } else { ver_skip.0 };
    match r.u32()? {
        0 => Err("zero timescale".into()),
        s => Ok(s),
    }
}

fn parse(moov: &[u8]) -> Result<Index, String> {
    let movie_scale = timescale(child(moov, b"mvhd").ok_or("no mvhd")?, (8, 16))?;
    let mut video = None;
    let mut audio = Vec::new();
    for (kind, trak) in boxes(moov)? {
        if &kind != b"trak" {
            continue;
        }
        let mdia = child(trak, b"mdia").ok_or("no mdia")?;
        let handler = child(mdia, b"hdlr").and_then(|h| h.get(8..12)).unwrap_or_default();
        if handler != b"vide" && handler != b"soun" {
            continue;
        }
        let scale = timescale(child(mdia, b"mdhd").ok_or("no mdhd")?, (8, 16))?;
        let stbl = child(mdia, b"minf").and_then(|m| child(m, b"stbl")).ok_or("no stbl")?;
        let (media_time, empty) = edits(child(trak, b"edts").and_then(|e| child(e, b"elst")), movie_scale)?;
        let to_hns = |ticks: i64| (((ticks - media_time) as i128 * SECOND as i128) / scale as i128) as i64 + empty;
        let samples = Samples::new(table(stbl)?.into_iter().map(|(t, size, key)| (to_hns(t), size, key)).collect());
        if handler == b"vide" {
            video.get_or_insert(samples);
        } else {
            audio.push(samples);
        }
    }
    Ok(Index { video: video.ok_or("no video track")?, audio })
}

/// The first edit's media start (media ticks) and any leading empty edit (hns).
fn edits(elst: Option<&[u8]>, movie_scale: u32) -> Result<(i64, i64), String> {
    let Some(elst) = elst else { return Ok((0, 0)) };
    let mut r = reader(elst);
    let v1 = r.u32()? >> 24 == 1;
    let mut empty = 0i64;
    for _ in 0..r.u32()? {
        let (dur, media) = if v1 { (r.u64()? as i64, r.u64()? as i64) } else { (r.u32()? as i64, r.u32()? as i32 as i64) };
        r.u32()?;
        if media == -1 {
            empty += (dur as i128 * SECOND as i128 / movie_scale as i128) as i64;
        } else {
            return Ok((media, empty));
        }
    }
    Ok((0, empty))
}

/// (presentation ticks, size, keyframe) per sample in decode order.
fn table(stbl: &[u8]) -> Result<Vec<(i64, u32, bool)>, String> {
    let mut stsz = reader(child(stbl, b"stsz").ok_or("no stsz")?);
    stsz.u32()?;
    let fixed = stsz.u32()?;
    let count = stsz.u32()? as usize;
    let mut out = Vec::with_capacity(count);
    let mut stts = reader(child(stbl, b"stts").ok_or("no stts")?);
    stts.u32()?;
    let mut dts = 0i64;
    for _ in 0..stts.u32()? {
        let (n, delta) = (stts.u32()?, stts.u32()? as i64);
        for _ in 0..n {
            if out.len() == count {
                break;
            }
            let size = if fixed != 0 { fixed } else { stsz.u32()? };
            out.push((dts, size, true));
            dts += delta;
        }
    }
    if let Some(ctts) = child(stbl, b"ctts") {
        let mut r = reader(ctts);
        r.u32()?;
        let mut i = 0;
        for _ in 0..r.u32()? {
            let (n, off) = (r.u32()?, r.u32()? as i32 as i64);
            for _ in 0..n {
                if let Some(s) = out.get_mut(i) {
                    s.0 += off;
                }
                i += 1;
            }
        }
    }
    if let Some(stss) = child(stbl, b"stss") {
        let mut r = reader(stss);
        r.u32()?;
        out.iter_mut().for_each(|s| s.2 = false);
        for _ in 0..r.u32()? {
            if let Some(s) = (r.u32()? as usize).checked_sub(1).and_then(|i| out.get_mut(i)) {
                s.2 = true;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn words(w: &[u32]) -> Vec<u8> {
        w.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn trak(handler: &[u8; 4], scale: u32, stbl: Vec<u8>, elst: Option<Vec<u8>>) -> Vec<u8> {
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 13]);
        let mut mdia = atom(b"mdhd", &words(&[0, 0, 0, scale, 0, 0]));
        mdia.extend(atom(b"hdlr", &hdlr));
        mdia.extend(atom(b"minf", &atom(b"stbl", &stbl)));
        let mut body = atom(b"tkhd", &[0; 84]);
        if let Some(e) = elst {
            body.extend(atom(b"edts", &atom(b"elst", &e)));
        }
        body.extend(atom(b"mdia", &mdia));
        atom(b"trak", &body)
    }

    #[test]
    fn sample_tables_give_times_sizes_and_keyframes() {
        let mut vstbl = atom(b"stts", &words(&[0, 1, 6, 512]));
        vstbl.extend(atom(b"stss", &words(&[0, 2, 1, 4])));
        vstbl.extend(atom(b"stsz", &words(&[0, 0, 6, 100, 10, 10, 90, 10, 10])));
        let mut astbl = atom(b"stts", &words(&[0, 1, 4, 1024]));
        astbl.extend(atom(b"stsz", &words(&[0, 7, 4])));
        let elst = words(&[0, 1, 1000, 1024, 0x0001_0000]);
        let mut moov = atom(b"mvhd", &words(&[0, 0, 0, 1000, 0]));
        moov.extend(trak(b"vide", 1536, vstbl, None));
        moov.extend(trak(b"soun", 48_000, astbl, Some(elst)));
        let idx = parse(&moov).unwrap();
        let third = SECOND / 3;
        assert_eq!(idx.video.keys, [0, 3 * 512 * SECOND / 1536]);
        assert_eq!(idx.video.keys[1], SECOND);
        assert_eq!(idx.video.total(), 230);
        assert_eq!(idx.video.bytes(0, SECOND), 120);
        assert_eq!(idx.video.bytes(SECOND, 2 * SECOND), 110);
        assert_eq!(idx.video.bytes(third, SECOND + 1), 20 + 90);
        assert_eq!(idx.audio.len(), 1);
        assert_eq!(idx.audio[0].keys.len(), 4);
        assert_eq!(idx.audio[0].keys[0], -1024 * SECOND / 48_000);
        assert_eq!(idx.audio[0].bytes(0, SECOND), 21);
    }
}
