use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use color_quant::NeuQuant;
use gif::{DisposalMethod, Encoder, Frame, Repeat};

use super::crop::Crop;
use super::edit::source_time;
use super::export::GifJob;
use super::math::{gif_delay, gif_frames, gif_tick};
use super::media::{self, Decoded, Info};
use super::trim::Progress;

const COLORS: usize = 255;
const TRANSPARENT: u8 = 255;
const PALETTE_FRAMES: u64 = 16;
const SAME: i32 = 3;

pub(super) fn export(src: &Path, info: &Info, job: &GifJob, out: &Path, p: &Progress) -> Result<bool, String> {
    let res = run(src, info, job, out, p);
    if !matches!(res, Ok(true)) {
        let _ = std::fs::remove_file(out);
    }
    res
}

/// What the reader decodes and where the gif sits in it: the whole frame scaled so the crop comes out at the gif's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    full: (u32, u32),
    x: u32,
    y: u32,
    size: (u32, u32),
}

fn area(src: (u32, u32), crop: Option<Crop>, size: (u32, u32)) -> Area {
    let c = crop.unwrap_or(Crop::full(src));
    let (fx, fy) = (size.0 as f64 / c.w.max(1) as f64, size.1 as f64 / c.h.max(1) as f64);
    let full = (((src.0 as f64 * fx).round() as u32).max(size.0), ((src.1 as f64 * fy).round() as u32).max(size.1));
    let x = ((c.x as f64 * fx).round() as u32).min(full.0 - size.0);
    let y = ((c.y as f64 * fy).round() as u32).min(full.1 - size.1);
    Area { full, x, y, size }
}

fn bgrx(f: &Decoded, a: Area, out: &mut Vec<u8>) -> Result<(), String> {
    out.clear();
    media::with_pixels(&f.sample, a.full.0, |rows| {
        for y in 0..a.size.1 as usize {
            out.extend_from_slice(&rows.row(a.y as usize + y)[a.x as usize * 4..(a.x + a.size.0) as usize * 4]);
        }
    })
}

fn run(src: &Path, info: &Info, job: &GifJob, out: &Path, p: &Progress) -> Result<bool, String> {
    let (kept, size, fps) = (&job.kept[..], job.size, job.fps);
    let area = area(info.size, job.crop, size);
    let (w, h) = (u16::try_from(size.0).map_err(|_| "too wide")?, u16::try_from(size.1).map_err(|_| "too tall")?);
    let frames = gif_frames(kept.iter().map(|r| r.len()).sum(), fps);
    if frames == 0 {
        return Err("the selection is empty".into());
    }
    let reader = media::rgb_reader(src, area.full)?;

    let picks = frames.min(PALETTE_FRAMES);
    let mut sample = Vec::with_capacity(picks as usize * size.0 as usize * size.1 as usize * 4);
    let mut px = Vec::new();
    for j in 0..picks {
        if p.cancelled() {
            return Ok(false);
        }
        let Some((_, t)) = source_time(kept, gif_tick(0, j * frames / picks, fps)) else { continue };
        media::seek(&reader, t)?;
        if let (Some(f), _) = media::decode_until(&reader, t, None)? {
            bgrx(&f, area, &mut px)?;
            sample.extend(px.chunks_exact(4).flat_map(|c| [c[2], c[1], c[0], 255]));
        }
        p.set(j as i64 + 1, picks as i64 * 10);
    }
    if sample.is_empty() {
        return Err("the selection has no frames".into());
    }
    let nq = NeuQuant::new(10, COLORS, &sample);
    drop(sample);
    let mut q = Quantizer::new(&nq.color_map_rgb());
    let mut palette = nq.color_map_rgb();
    palette.resize(256 * 3, 0);

    let file = File::create(out).map_err(|e| format!("can't create {}: {e}", out.display()))?;
    let mut enc = Encoder::new(BufWriter::new(file), w, h, &palette).map_err(|e| e.to_string())?;
    enc.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;
    let mut dither = Dither::new(size);
    let mut pending: Option<Frame> = None;
    let (mut cur, mut next): (Option<Decoded>, Option<Decoded>) = (None, None);
    let mut eof = false;
    let mut shown_ts = None;
    let mut segment = None;
    for k in 0..frames {
        if p.cancelled() {
            return Ok(false);
        }
        let Some((s, t)) = source_time(kept, gif_tick(0, k, fps)) else { break };
        if segment != Some(s) {
            segment = Some(s);
            media::seek(&reader, kept[s].start)?;
            (cur, next, eof) = (None, None, false);
        }
        loop {
            if next.is_none() && !eof {
                next = media::next_video(&reader)?;
                eof = next.is_none();
            }
            match next.take() {
                Some(n) if n.ts <= t || cur.is_none() => cur = Some(n),
                n => {
                    next = n;
                    break;
                }
            }
        }
        let Some(f) = &cur else { break };
        let delay = gif_delay(k, fps);
        let changed = if shown_ts == Some(f.ts) {
            None
        } else {
            shown_ts = Some(f.ts);
            bgrx(f, area, &mut px)?;
            dither.frame(&mut q, &px)
        };
        if changed.is_none()
            && let Some(prev) = &mut pending
        {
            prev.delay = prev.delay.saturating_add(delay);
        } else {
            if let Some(prev) = pending.take() {
                enc.write_frame(&prev).map_err(|e| e.to_string())?;
            }
            let (left, top, width, height, buffer) = changed.unwrap_or((0, 0, 1, 1, vec![TRANSPARENT]));
            pending = Some(Frame {
                delay,
                dispose: DisposalMethod::Keep,
                transparent: Some(TRANSPARENT),
                left,
                top,
                width,
                height,
                buffer: Cow::Owned(buffer),
                ..Frame::default()
            });
        }
        p.set(100 + (k as i64 + 1) * 900 / frames as i64, 1000);
    }
    if let Some(prev) = pending.take() {
        enc.write_frame(&prev).map_err(|e| e.to_string())?;
    }
    let mut file = enc.into_inner().map_err(|e| e.to_string())?;
    file.flush().map_err(|e| format!("can't write {}: {e}", out.display()))?;
    Ok(true)
}

struct Quantizer {
    pal: Vec<[i32; 3]>,
    cache: Vec<u8>,
    known: Vec<bool>,
}

impl Quantizer {
    fn new(rgb: &[u8]) -> Self {
        let pal = rgb.chunks_exact(3).map(|c| [c[0] as i32, c[1] as i32, c[2] as i32]).collect();
        Quantizer { pal, cache: vec![0; 1 << 18], known: vec![false; 1 << 18] }
    }

    fn nearest(&mut self, c: [i32; 3]) -> u8 {
        let key = ((c[0] >> 2) << 12 | (c[1] >> 2) << 6 | (c[2] >> 2)) as usize;
        if !self.known[key] {
            let q = c.map(|v| (v & !3) + 2);
            let dist = |p: &[i32; 3]| {
                let (r, g, b) = (p[0] - q[0], p[1] - q[1], p[2] - q[2]);
                2 * r * r + 4 * g * g + 3 * b * b
            };
            let best = (0..self.pal.len()).min_by_key(|&i| dist(&self.pal[i])).unwrap_or(0);
            self.cache[key] = best as u8;
            self.known[key] = true;
        }
        self.cache[key]
    }
}

struct Dither {
    w: usize,
    h: usize,
    shown: Vec<[u8; 3]>,
    err: [Vec<[i32; 3]>; 2],
}

type Changed = (u16, u16, u16, u16, Vec<u8>);

impl Dither {
    fn new(size: (u32, u32)) -> Self {
        let (w, h) = (size.0 as usize, size.1 as usize);
        Dither { w, h, shown: Vec::new(), err: [vec![[0; 3]; w + 2], vec![[0; 3]; w + 2]] }
    }

    fn frame(&mut self, q: &mut Quantizer, bgrx: &[u8]) -> Option<Changed> {
        let first = self.shown.is_empty();
        if first {
            self.shown = vec![[0; 3]; self.w * self.h];
        }
        let mut out = vec![TRANSPARENT; self.w * self.h];
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..self.h {
            let [cur, next] = &mut self.err;
            next.fill([0; 3]);
            for x in 0..self.w {
                let i = y * self.w + x;
                let s = [bgrx[i * 4 + 2], bgrx[i * 4 + 1], bgrx[i * 4]];
                let old = self.shown[i];
                if !first && (0..3).all(|c| (s[c] as i32 - old[c] as i32).abs() <= SAME) {
                    continue;
                }
                let e = cur[x + 1];
                let v = [0, 1, 2].map(|c| (s[c] as i32 + e[c] / 16).clamp(0, 255));
                let idx = q.nearest(v);
                let p = q.pal[idx as usize];
                let d = [0, 1, 2].map(|c| v[c] - p[c]);
                for c in 0..3 {
                    cur[x + 2][c] += d[c] * 7;
                    next[x][c] += d[c] * 3;
                    next[x + 1][c] += d[c] * 5;
                    next[x + 2][c] += d[c];
                }
                out[i] = idx;
                self.shown[i] = s;
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            }
            self.err.swap(0, 1);
        }
        if x0 == usize::MAX {
            return None;
        }
        let (cw, ch) = (x1 - x0 + 1, y1 - y0 + 1);
        let mut buf = Vec::with_capacity(cw * ch);
        for y in y0..=y1 {
            buf.extend_from_slice(&out[y * self.w + x0..y * self.w + x1 + 1]);
        }
        Some((x0 as u16, y0 as u16, cw as u16, ch as u16, buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_frames_produce_nothing_and_changes_are_cropped() {
        let pal: Vec<u8> = (0..COLORS).flat_map(|i| [i as u8, i as u8, i as u8]).collect();
        let mut q = Quantizer::new(&pal);
        let mut d = Dither::new((8, 4));
        let mut px = vec![100u8; 8 * 4 * 4];
        let first = d.frame(&mut q, &px).unwrap();
        assert_eq!((first.0, first.1, first.2, first.3), (0, 0, 8, 4));
        assert!(first.4.iter().all(|&i| i != TRANSPARENT));
        assert!(d.frame(&mut q, &px).is_none());
        let i = (2 * 8 + 5) * 4;
        px[i..i + 3].copy_from_slice(&[200, 200, 200]);
        let (x, y, w, h, buf) = d.frame(&mut q, &px).unwrap();
        assert_eq!((x, y, w, h), (5, 2, 1, 1));
        assert!(buf[0] != TRANSPARENT && buf[0].abs_diff(200) <= 3);
    }

    #[test]
    fn crops_are_cut_from_the_frame_scaled_to_the_gif() {
        let crop = Crop { x: 960, y: 540, w: 960, h: 540 };
        assert_eq!(area((1920, 1080), Some(crop), (480, 270)), Area { full: (960, 540), x: 480, y: 270, size: (480, 270) });
        assert_eq!(area((1920, 1080), None, (640, 360)), Area { full: (640, 360), x: 0, y: 0, size: (640, 360) });
        let edge = Crop { x: 1001, y: 0, w: 919, h: 1080 };
        let a = area((1920, 1080), Some(edge), (306, 360));
        assert!(a.x + a.size.0 <= a.full.0 && a.full.1 == 360);
    }

    #[test]
    fn dithering_keeps_the_average_colour() {
        let pal = [0u8, 0, 0, 255, 255, 255];
        let mut q = Quantizer::new(&pal);
        let mut d = Dither::new((64, 64));
        let px: Vec<u8> = std::iter::repeat_n([64u8, 64, 64, 0], 64 * 64).flatten().collect();
        let (.., buf) = d.frame(&mut q, &px).unwrap();
        let white = buf.iter().filter(|&&i| i == 1).count() as f64 / buf.len() as f64;
        assert!((white - 0.25).abs() < 0.03, "{white}");
    }
}
