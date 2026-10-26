use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

struct Atom {
    kind: [u8; 4],
    start: usize,
    body: usize,
    end: usize,
}

fn atoms(data: &[u8]) -> Result<Vec<Atom>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[at + 4..at + 8].try_into().unwrap();
        let (size, body) = match size {
            0 => ((data.len() - at) as u64, 8),
            1 if at + 16 <= data.len() => (u64::from_be_bytes(data[at + 8..at + 16].try_into().unwrap()), 16),
            _ => (size, 8),
        };
        if size < body as u64 || at as u64 + size > data.len() as u64 {
            return Err(format!("broken {} box", String::from_utf8_lossy(&kind)));
        }
        let end = at + size as usize;
        out.push(Atom { kind, start: at, body: at + body, end });
        at = end;
    }
    Ok(out)
}

fn atom(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 16);
    match u32::try_from(body.len() + 8) {
        Ok(size) => out.extend_from_slice(&size.to_be_bytes()),
        Err(_) => out.extend_from_slice(&1u32.to_be_bytes()),
    }
    out.extend_from_slice(kind);
    if body.len() + 8 > u32::MAX as usize {
        out.extend_from_slice(&(body.len() as u64 + 16).to_be_bytes());
    }
    out.extend_from_slice(body);
    out
}

pub(crate) fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    let found = atoms(data).ok()?.into_iter().find(|a| &a.kind == kind)?;
    Some(&data[found.body..found.end])
}

pub(crate) type Boxes<'a> = Vec<([u8; 4], &'a [u8])>;

pub(crate) fn boxes(data: &[u8]) -> Result<Boxes<'_>, String> {
    Ok(atoms(data)?.into_iter().map(|a| (a.kind, &data[a.body..a.end])).collect())
}

/// The body of the file's moov box.
pub(crate) fn moov(path: &Path) -> Result<Vec<u8>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let (_, _, data) = read_moov(&mut f)?;
    child(&data, b"moov").map(<[u8]>::to_vec).ok_or_else(|| "broken moov box".into())
}

// hdlr: version/flags, pre_defined, handler_type, 3 reserved words, then the name.
const HDLR_NAME: usize = 24;
// the name media foundation (and ffmpeg) give a sound track by default.
const UNNAMED: &str = "SoundHandler";

fn is_sound(trak: &[u8]) -> bool {
    let hdlr = child(trak, b"mdia").and_then(|m| child(m, b"hdlr"));
    hdlr.is_some_and(|h| h.len() >= HDLR_NAME && &h[8..12] == b"soun")
}

// ffmpeg writes a track title as trak/udta/name holding the bare utf-8 text; vlc and quicktime read it.
fn udta_with_name(udta: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let mut body = atom(b"name", name.as_bytes());
    for a in atoms(udta)?.iter().filter(|a| &a.kind != b"name") {
        body.extend_from_slice(&udta[a.start..a.end]);
    }
    Ok(atom(b"udta", &body))
}

fn rename_trak(trak: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(trak.len() + 2 * name.len() + 24);
    let mut named = false;
    for a in atoms(trak)? {
        let body = &trak[a.body..a.end];
        match &a.kind {
            b"mdia" => {
                let mut mdia = Vec::with_capacity(body.len() + name.len());
                for m in atoms(body)? {
                    if &m.kind == b"hdlr" {
                        let mut hdlr = body[m.body..m.body + HDLR_NAME].to_vec();
                        hdlr.extend_from_slice(name.as_bytes());
                        hdlr.push(0);
                        mdia.extend(atom(b"hdlr", &hdlr));
                    } else {
                        mdia.extend_from_slice(&body[m.start..m.end]);
                    }
                }
                out.extend(atom(b"mdia", &mdia));
            }
            b"udta" if !named => {
                out.extend(udta_with_name(body, name)?);
                named = true;
            }
            _ => out.extend_from_slice(&trak[a.start..a.end]),
        }
    }
    if !named {
        out.extend(udta_with_name(&[], name)?);
    }
    Ok(out)
}

fn rename_moov(moov: &[u8], names: &[String]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(moov.len() + 64 * names.len());
    let mut names = names.iter();
    for a in atoms(moov)? {
        let body = &moov[a.body..a.end];
        let sound = &a.kind == b"trak" && is_sound(body);
        match sound.then(|| names.next()).flatten() {
            Some(name) => out.extend(atom(b"trak", &rename_trak(body, name)?)),
            None => out.extend_from_slice(&moov[a.start..a.end]),
        }
    }
    Ok(out)
}

/// Where the top-level box `kind` starts in `f` (`len` bytes long), its size and its header's.
fn find_top(f: &mut std::fs::File, len: u64, kind: &[u8; 4]) -> Result<(u64, u64, u64), String> {
    find_box(f, 0, len, kind)?.ok_or_else(|| format!("no {} box", String::from_utf8_lossy(kind)))
}

/// The box `kind` among the boxes from `at` up to `end` in `f`: where it starts, its size and its
/// header's; seeks from header to header, reading nothing else.
fn find_box(f: &mut std::fs::File, mut at: u64, end: u64, kind: &[u8; 4]) -> Result<Option<(u64, u64, u64)>, String> {
    let err = |e: std::io::Error| e.to_string();
    let mut header = [0u8; 16];
    while at + 8 <= end {
        f.seek(SeekFrom::Start(at)).map_err(err)?;
        let n = if at + 16 <= end { 16 } else { 8 };
        f.read_exact(&mut header[..n]).map_err(err)?;
        let (size, head) = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => (end - at, 8),
            1 if n == 16 => (u64::from_be_bytes(header[8..16].try_into().unwrap()), 16),
            s => (s as u64, 8),
        };
        if size < head || at + size > end {
            return Err("broken box".into());
        }
        if &header[4..8] == kind {
            return Ok(Some((at, size, head)));
        }
        at += size;
    }
    Ok(None)
}

fn read_moov(f: &mut std::fs::File) -> Result<(u64, u64, Vec<u8>), String> {
    let err = |e: std::io::Error| e.to_string();
    let len = f.metadata().map_err(err)?.len();
    let (at, size, _) = find_top(f, len, b"moov")?;
    let mut data = vec![0; size as usize];
    f.seek(SeekFrom::Start(at)).map_err(err)?;
    f.read_exact(&mut data).map_err(err)?;
    Ok((at, len, data))
}

/// How long the movie at `path` plays, in milliseconds, from its movie header (or a fragmented
/// file's `mehd`): a few small reads, however long the file is. `None` when it doesn't say.
pub fn duration_ms(path: &Path) -> Option<u32> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let (at, size, head) = find_top(&mut f, len, b"moov").ok()?;
    let (moov, end) = (at + head, at + size);
    let be32 = |b: &[u8], i: usize| b.get(i..i + 4).map(|w| u32::from_be_bytes(w.try_into().unwrap()) as u64);
    let be64 = |b: &[u8], i: usize| b.get(i..i + 8).map(|w| u64::from_be_bytes(w.try_into().unwrap()));
    // version and flags, then the times: 32-bit fields in version 0, 64-bit in version 1
    let body = body_of(&mut f, moov, end, b"mvhd", 32)?;
    let (scale, mut duration) = match body.first()? {
        0 => (be32(&body, 12)?, be32(&body, 16)?),
        1 => (be32(&body, 20)?, be64(&body, 24)?),
        _ => return None,
    };
    if duration == 0 || duration == u32::MAX as u64 || duration == u64::MAX {
        let (mvex, mvex_size, mvex_head) = find_box(&mut f, moov, end, b"mvex").ok()??;
        let body = body_of(&mut f, mvex + mvex_head, mvex + mvex_size, b"mehd", 12)?;
        duration = if body.first() == Some(&1) { be64(&body, 4)? } else { be32(&body, 4)? };
    }
    if scale == 0 || duration == 0 || duration == u64::MAX {
        return None;
    }
    u32::try_from(duration as u128 * 1000 / scale as u128).ok()
}

/// The first `most` bytes of the body of box `kind` among those from `at` up to `end`.
fn body_of(f: &mut std::fs::File, at: u64, end: u64, kind: &[u8; 4], most: u64) -> Option<Vec<u8>> {
    let (at, size, head) = find_box(f, at, end, kind).ok()??;
    let mut buf = vec![0; (size - head).min(most) as usize];
    f.seek(SeekFrom::Start(at + head)).ok()?;
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

// the media foundation sink writes moov after mdat, so rewriting it moves no sample data.
fn rewrite_moov(path: &Path, edit: impl FnOnce(&[u8]) -> Result<Vec<u8>, String>) -> Result<(), String> {
    let err = |e: String| format!("{}: {e}", path.display());
    let mut f = OpenOptions::new().read(true).write(true).open(path).map_err(|e| err(e.to_string()))?;
    let (at, len, data) = read_moov(&mut f).map_err(err)?;
    if at + data.len() as u64 != len {
        return Err(err("moov isn't the last box".into()));
    }
    let m = &atoms(&data)?[0];
    let rewritten = atom(b"moov", &edit(&data[m.body..m.end]).map_err(err)?);
    let write = |f: &mut std::fs::File| {
        f.seek(SeekFrom::Start(at))?;
        f.write_all(&rewritten)?;
        f.set_len(at + rewritten.len() as u64)
    };
    write(&mut f).map_err(|e| err(e.to_string()))
}

pub fn name_tracks(path: &Path, names: &[String]) -> Result<(), String> {
    rewrite_moov(path, |moov| rename_moov(moov, names))
}

fn with_movie_box(moov: &[u8], kind: &[u8; 4], body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(moov.len() + body.len() + 16);
    let mut placed = false;
    for a in atoms(moov)? {
        if &a.kind != b"udta" || placed {
            out.extend_from_slice(&moov[a.start..a.end]);
            continue;
        }
        let udta = &moov[a.body..a.end];
        let mut inner = Vec::with_capacity(udta.len() + body.len() + 8);
        for u in atoms(udta)?.iter().filter(|u| &u.kind != kind) {
            inner.extend_from_slice(&udta[u.start..u.end]);
        }
        inner.extend(atom(kind, body));
        out.extend(atom(b"udta", &inner));
        placed = true;
    }
    if !placed {
        out.extend(atom(b"udta", &atom(kind, body)));
    }
    Ok(out)
}

/// Puts `body` into the box moov/udta/`kind`, replacing one that's there.
pub fn set_movie_box(path: &Path, kind: &[u8; 4], body: &[u8]) -> Result<(), String> {
    rewrite_moov(path, |moov| with_movie_box(moov, kind, body))
}

/// The body of moov/udta/`kind`, if the file has one.
pub fn movie_box(path: &Path, kind: &[u8; 4]) -> Result<Option<Vec<u8>>, String> {
    let err = |e: String| format!("{}: {e}", path.display());
    let mut f = std::fs::File::open(path).map_err(|e| err(e.to_string()))?;
    let (_, _, data) = read_moov(&mut f).map_err(err)?;
    let moov = child(&data, b"moov").ok_or_else(|| err("broken moov box".into()))?;
    Ok(child(moov, b"udta").and_then(|u| child(u, kind)).map(<[u8]>::to_vec))
}

pub fn track_names(path: &Path) -> Vec<Option<String>> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let Ok((_, _, data)) = read_moov(&mut f) else { return Vec::new() };
    let Some(moov) = child(&data, b"moov") else { return Vec::new() };
    let Ok(traks) = atoms(moov) else { return Vec::new() };
    traks
        .iter()
        .filter(|a| &a.kind == b"trak" && is_sound(&moov[a.body..a.end]))
        .map(|a| {
            let trak = &moov[a.body..a.end];
            let titled = child(trak, b"udta").and_then(|u| child(u, b"name")).map(|n| String::from_utf8_lossy(n));
            let hdlr = child(child(trak, b"mdia")?, b"hdlr").map(|h| String::from_utf8_lossy(&h[HDLR_NAME..]));
            let name = titled.filter(|n| !n.trim_end_matches('\0').is_empty()).or(hdlr)?;
            let name = name.trim_end_matches('\0').to_string();
            (!name.is_empty() && name != UNNAMED).then_some(name)
        })
        .collect()
}

pub fn trak_kinds(path: &Path) -> Vec<[u8; 4]> {
    let Ok(mut f) = std::fs::File::open(path) else { return Vec::new() };
    let Ok((_, _, data)) = read_moov(&mut f) else { return Vec::new() };
    let Some(moov) = child(&data, b"moov") else { return Vec::new() };
    let Ok(traks) = atoms(moov) else { return Vec::new() };
    traks
        .iter()
        .filter(|a| &a.kind == b"trak")
        .map(|a| {
            let hdlr = child(&moov[a.body..a.end], b"mdia").and_then(|m| child(m, b"hdlr"));
            hdlr.filter(|h| h.len() >= 12).map_or([0; 4], |h| h[8..12].try_into().unwrap())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hdlr(kind: &[u8; 4], name: &str) -> Vec<u8> {
        let mut body = vec![0u8; 8];
        body.extend_from_slice(kind);
        body.extend_from_slice(&[0; 12]);
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        atom(b"hdlr", &body)
    }

    fn trak(kind: &[u8; 4], name: &str, extra: &[u8]) -> Vec<u8> {
        let mut mdia = atom(b"mdhd", &[7; 24]);
        mdia.extend(hdlr(kind, name));
        mdia.extend(atom(b"minf", &[9; 40]));
        let mut body = atom(b"tkhd", &[1; 84]);
        body.extend(atom(b"mdia", &mdia));
        body.extend_from_slice(extra);
        atom(b"trak", &body)
    }

    fn file() -> Vec<u8> {
        let mut moov = atom(b"mvhd", &[2; 100]);
        moov.extend(trak(b"vide", "VideoHandler", &[]));
        moov.extend(trak(b"soun", UNNAMED, &[]));
        moov.extend(trak(b"soun", "old", &atom(b"udta", &atom(b"cprt", b"x"))));
        moov.extend(atom(b"udta", &atom(b"meta", &[3; 20])));
        let mut f = atom(b"ftyp", b"mp42\0\0\0\0isommp42");
        f.extend(atom(b"mdat", &[5; 1000]));
        f.extend(atom(b"moov", &moov));
        f
    }

    fn hdlr_names(data: &[u8]) -> Vec<String> {
        let moov = child(data, b"moov").unwrap();
        atoms(moov)
            .unwrap()
            .iter()
            .filter(|a| &a.kind == b"trak")
            .map(|a| {
                let h = child(child(&moov[a.body..a.end], b"mdia").unwrap(), b"hdlr").unwrap();
                String::from_utf8_lossy(&h[HDLR_NAME..h.len() - 1]).into_owned()
            })
            .collect()
    }

    #[test]
    fn audio_tracks_get_names_and_the_rest_stays() {
        let path = std::env::temp_dir().join(format!("glint_mp4_names_{}.mp4", std::process::id()));
        let original = file();
        std::fs::write(&path, &original).unwrap();
        assert_eq!(track_names(&path), [None, Some("old".to_string())]);
        name_tracks(&path, &["mix".into(), "mic: Shure MV7".into()]).unwrap();
        let data = std::fs::read(&path).unwrap();
        let names = track_names(&path);
        let kinds = trak_kinds(&path);
        std::fs::remove_file(&path).unwrap();

        assert_eq!(names, [Some("mix".to_string()), Some("mic: Shure MV7".to_string())]);
        assert_eq!(kinds, [*b"vide", *b"soun", *b"soun"]);
        assert_eq!(hdlr_names(&data), ["VideoHandler", "mix", "mic: Shure MV7"]);
        let top = atoms(&data).unwrap();
        assert_eq!(top.iter().map(|a| a.kind).collect::<Vec<_>>(), [*b"ftyp", *b"mdat", *b"moov"]);
        assert_eq!(top.last().unwrap().end, data.len());
        assert_eq!(&data[..top[2].start], &original[..top[2].start]);
        let moov = child(&data, b"moov").unwrap();
        let kinds: Vec<[u8; 4]> = atoms(moov).unwrap().iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [*b"mvhd", *b"trak", *b"trak", *b"trak", *b"udta"]);
        assert_eq!(child(moov, b"udta").unwrap(), atom(b"meta", &[3; 20]));
        let traks: Vec<&[u8]> =
            atoms(moov).unwrap().into_iter().filter(|a| &a.kind == b"trak").map(|a| &moov[a.body..a.end]).collect();
        assert_eq!(child(traks[0], b"udta"), None);
        let inner = |t: &[u8]| atoms(t).unwrap().iter().map(|a| a.kind).collect::<Vec<_>>();
        assert_eq!(inner(traks[1]), [*b"tkhd", *b"mdia", *b"udta"]);
        let mut name = atom(b"name", b"mic: Shure MV7");
        name.extend(atom(b"cprt", b"x"));
        assert_eq!(child(traks[2], b"udta").unwrap(), name);
    }

    #[test]
    fn track_titles_match_what_ffmpeg_writes() {
        // trak/udta of `ffmpeg -metadata:s:a:0 title=mixname -f mp4` (ffmpeg 6.1.1).
        let ffmpeg = b"\x00\x00\x00\x17udta\x00\x00\x00\x0fnamemixname";
        let mut body = atom(b"tkhd", &[1; 84]);
        let mut mdia = atom(b"mdhd", &[7; 24]);
        mdia.extend(hdlr(b"soun", UNNAMED));
        body.extend(atom(b"mdia", &mdia));
        let renamed = rename_trak(&body, "mixname").unwrap();
        assert!(renamed.ends_with(ffmpeg));
        let again = rename_trak(&renamed, "mixname").unwrap();
        assert_eq!(again, renamed);
    }

    #[test]
    fn files_it_cant_rewrite_are_left_alone() {
        let path = std::env::temp_dir().join(format!("glint_mp4_moov_first_{}.mp4", std::process::id()));
        let mut data = atom(b"ftyp", b"isom");
        data.extend(atom(b"moov", &trak(b"soun", UNNAMED, &[])));
        data.extend(atom(b"mdat", &[5; 100]));
        std::fs::write(&path, &data).unwrap();
        assert!(name_tracks(&path, &["mix".into()]).is_err());
        let after = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(after, data);
    }

    #[test]
    fn movie_boxes_go_into_udta_and_replace_their_old_copy() {
        let path = std::env::temp_dir().join(format!("glint_mp4_movie_box_{}.mp4", std::process::id()));
        let original = file();
        std::fs::write(&path, &original).unwrap();
        assert_eq!(movie_box(&path, b"glck"), Ok(None));
        set_movie_box(&path, b"glck", b"first").unwrap();
        set_movie_box(&path, b"glck", b"{\"v\":2}").unwrap();
        let found = movie_box(&path, b"glck");
        let data = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(found, Ok(Some(b"{\"v\":2}".to_vec())));
        let top = atoms(&data).unwrap();
        assert_eq!(&data[..top[2].start], &original[..top[2].start]);
        assert_eq!(top.last().unwrap().end, data.len());
        let moov = child(&data, b"moov").unwrap();
        let kinds: Vec<[u8; 4]> = atoms(moov).unwrap().iter().map(|a| a.kind).collect();
        assert_eq!(kinds, [*b"mvhd", *b"trak", *b"trak", *b"trak", *b"udta"]);
        let mut udta = atom(b"meta", &[3; 20]);
        udta.extend(atom(b"glck", b"{\"v\":2}"));
        assert_eq!(child(moov, b"udta").unwrap(), udta);
        assert_eq!(&moov[..moov.len() - 8 - udta.len()], &child(&original, b"moov").unwrap()[..moov.len() - 8 - udta.len()]);
    }

    #[test]
    fn a_movie_box_adds_udta_when_there_is_none() {
        let moov = [atom(b"mvhd", &[2; 100]), trak(b"vide", "VideoHandler", &[])].concat();
        let out = with_movie_box(&moov, b"glck", b"[]").unwrap();
        assert_eq!(out, [moov.clone(), atom(b"udta", &atom(b"glck", b"[]"))].concat());
        assert_eq!(with_movie_box(&out, b"glck", b"{}").unwrap(), [moov, atom(b"udta", &atom(b"glck", b"{}"))].concat());
    }

    /// A version-0 `mvhd` body: flags, two times, then `scale` and `duration`, and the rest.
    fn mvhd0(scale: u32, duration: u32) -> Vec<u8> {
        let mut b = vec![0u8; 12];
        b.extend(scale.to_be_bytes());
        b.extend(duration.to_be_bytes());
        b.extend([0; 80]);
        atom(b"mvhd", &b)
    }

    fn mvhd1(scale: u32, duration: u64) -> Vec<u8> {
        let mut b = vec![1u8, 0, 0, 0];
        b.extend([0; 16]);
        b.extend(scale.to_be_bytes());
        b.extend(duration.to_be_bytes());
        b.extend([0; 80]);
        atom(b"mvhd", &b)
    }

    #[test]
    fn lengths_come_from_the_movie_header_wherever_it_is() {
        let dir = std::env::temp_dir().join(format!("glint_mp4_length_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let length = |name: &str, data: &[u8]| {
            let path = dir.join(name);
            std::fs::write(&path, data).unwrap();
            duration_ms(&path)
        };
        let ftyp = atom(b"ftyp", b"isomiso2avc1mp41");
        let mdat = atom(b"mdat", &[7; 5000]);
        let moov = |head: Vec<u8>| atom(b"moov", &[head, trak(b"vide", "VideoHandler", &[])].concat());
        // media foundation's layout: the samples, then the index
        assert_eq!(length("end.mp4", &[ftyp.clone(), mdat.clone(), moov(mvhd0(1000, 4250))].concat()), Some(4250));
        // fast start: the index first, a 90 kHz clock
        assert_eq!(length("start.mp4", &[ftyp.clone(), moov(mvhd0(90_000, 2_700_000)), mdat.clone()].concat()), Some(30_000));
        // version 1 times behind a 64-bit mdat size
        let mut big = atom(b"mdat", &[1; 64]);
        big.splice(0..8, [1u32.to_be_bytes().as_slice(), b"mdat", &(64u64 + 16).to_be_bytes()].concat());
        assert_eq!(length("v1.mp4", &[ftyp.clone(), big, moov(mvhd1(600, 600 * 125 + 300))].concat()), Some(125_500));
        // fragmented: the header says nothing, mvex/mehd does
        let mehd = atom(b"mehd", &[[0u8; 4].as_slice(), &1_500u32.to_be_bytes()].concat());
        let fragmented = atom(b"moov", &[mvhd0(1000, 0), atom(b"mvex", &[mehd, atom(b"trex", &[0; 24])].concat())].concat());
        assert_eq!(length("frag.mp4", &[ftyp.clone(), fragmented, mdat.clone()].concat()), Some(1_500));
        // nothing to go by
        assert_eq!(length("zero.mp4", &[ftyp.clone(), mdat.clone(), moov(mvhd0(1000, 0))].concat()), None);
        assert_eq!(length("noscale.mp4", &[ftyp.clone(), moov(mvhd0(0, 50))].concat()), None);
        assert_eq!(length("nomoov.mp4", &[ftyp.clone(), mdat.clone()].concat()), None);
        assert_eq!(length("cut.mp4", &[ftyp.clone(), mdat[..400].to_vec()].concat()), None);
        assert_eq!(length("text.mp4", b"not a movie at all"), None);
        assert_eq!(duration_ms(&dir.join("missing.mp4")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
