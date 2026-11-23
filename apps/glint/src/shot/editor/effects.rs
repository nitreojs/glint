use glint_core::geom::rect::Rect;

use super::marks::{Mark, brush_radius};
use glint_shell::gfx::Dib;

/// Applies a blur / pixelate mark to `dib`, which shows the screen area `sel`.
pub(super) fn apply_effect(dib: &mut Dib, m: &Mark, sel: Rect, scale: f32) {
    let (r, blur, strength) = match m {
        Mark::Blur { rect, radius } => (*rect, true, *radius),
        Mark::Pixelate { rect, block } => (*rect, false, *block),
        Mark::EffectBrush { points, blur, strength } => {
            if let Some(bounds) = m.ink(scale).intersect(&sel) {
                paint_brush(dib.pixels_mut(), sel, bounds, points, *blur, *strength, scale);
            }
            return;
        }
        _ => return,
    };
    let Some(r) = r.intersect(&sel).map(|r| r.offset(-sel.x, -sel.y)) else { return };
    let stride = dib.w as usize;
    let px = dib.pixels_mut();
    if blur {
        box_blur(px, stride, r, (strength * scale).round().max(1.0) as usize);
    } else {
        pixelate(px, stride, r, (strength * scale).round().max(2.0) as i32);
    }
}

fn paint_brush(
    pixels: &mut [u32],
    canvas: Rect,
    bounds: Rect,
    points: &[(f32, f32)],
    blur: bool,
    strength: f32,
    scale: f32,
) {
    let (w, h) = (bounds.w as usize, bounds.h as usize);
    let stride = canvas.w as usize;
    let at = (bounds.y - canvas.y) as usize * stride + (bounds.x - canvas.x) as usize;
    let mut filtered = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = at + y * stride;
        filtered.extend_from_slice(&pixels[row..row + w]);
    }

    let local = Rect { x: 0, y: 0, w: bounds.w, h: bounds.h };
    if blur {
        box_blur(&mut filtered, w, local, (strength * scale).round().max(1.0) as usize);
    } else {
        let block = (strength * scale).round().max(2.0) as i32;
        pixelate_aligned(&mut filtered, w, local, block, (bounds.x.rem_euclid(block), bounds.y.rem_euclid(block)));
    }

    let radius = brush_radius(blur, strength, scale);
    let mut segment = |(ax, ay): (f32, f32), (bx, by): (f32, f32)| {
        let dx = bx - ax;
        let dy = by - ay;
        let len2 = dx * dx + dy * dy;
        let y0 = ((ay.min(by) - radius).floor() as i32).max(bounds.y);
        let y1 = ((ay.max(by) + radius).ceil() as i32).min(bounds.bottom() - 1);
        for y in y0..=y1 {
            let fy = y as f32 + 0.5;
            let (lo, hi) = if dy == 0.0 {
                (0.0, 1.0)
            } else {
                let a = (fy - radius - ay) / dy;
                let b = (fy + radius - ay) / dy;

                (a.min(b).clamp(0.0, 1.0), a.max(b).clamp(0.0, 1.0))
            };
            let left = ax + lo * dx;
            let right = ax + hi * dx;
            let x0 = ((left.min(right) - radius).floor() as i32).max(bounds.x);
            let x1 = ((left.max(right) + radius).ceil() as i32).min(bounds.right() - 1);
            let row = (y - bounds.y) as usize;
            for x in x0..=x1 {
                let (x_rel, y_rel) = (x as f32 + 0.5 - ax, fy - ay);
                let t = if len2 > 0.0 { ((x_rel * dx + y_rel * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let (rx, ry) = (x_rel - t * dx, y_rel - t * dy);
                if rx * rx + ry * ry <= radius * radius {
                    let col = (x - bounds.x) as usize;
                    pixels[at + row * stride + col] = filtered[row * w + col];
                }
            }
        }
    };
    if let [point] = points {
        segment(*point, *point);
    } else {
        for pair in points.windows(2) {
            segment(pair[0], pair[1]);
        }
    }
}

/// Three box passes per axis (close to a Gaussian), clamped to the area's own edges so
/// nothing from outside the box bleeds in.
fn box_blur(px: &mut [u32], stride: usize, r: Rect, radius: usize) {
    let (w, h) = (r.w as usize, r.h as usize);
    let mut line = vec![0u32; w.max(h)];
    let mut tmp = vec![0u32; w.max(h)];
    for _ in 0..3 {
        for y in 0..h {
            let row = (r.y as usize + y) * stride + r.x as usize;
            line[..w].copy_from_slice(&px[row..row + w]);
            box_pass(&mut line[..w], &mut tmp[..w], radius);
            px[row..row + w].copy_from_slice(&line[..w]);
        }
        for x in 0..w {
            let col = r.y as usize * stride + r.x as usize + x;
            for y in 0..h {
                line[y] = px[col + y * stride];
            }
            box_pass(&mut line[..h], &mut tmp[..h], radius);
            for y in 0..h {
                px[col + y * stride] = line[y];
            }
        }
    }
}

fn box_pass(line: &mut [u32], tmp: &mut [u32], radius: usize) {
    let n = line.len() as isize;
    let get = |i: isize| line[i.clamp(0, n - 1) as usize];
    let ch = |p: u32| [(p >> 16) & 255, (p >> 8) & 255, p & 255];
    let div = (2 * radius + 1) as u32;
    let mut sum = [0u32; 3];
    for k in -(radius as isize)..=radius as isize {
        for (s, c) in sum.iter_mut().zip(ch(get(k))) {
            *s += c;
        }
    }
    for i in 0..n {
        tmp[i as usize] = ((sum[0] / div) << 16) | ((sum[1] / div) << 8) | (sum[2] / div);
        let (add, sub) = (ch(get(i + radius as isize + 1)), ch(get(i - radius as isize)));
        for k in 0..3 {
            sum[k] = sum[k] + add[k] - sub[k];
        }
    }
    line.copy_from_slice(tmp);
}

/// Averages `block`-sized squares, aligned to the area's top-left corner.
fn pixelate(px: &mut [u32], stride: usize, r: Rect, block: i32) {
    pixelate_aligned(px, stride, r, block, (0, 0));
}

fn pixelate_aligned(px: &mut [u32], stride: usize, r: Rect, block: i32, phase: (i32, i32)) {
    let mut by = r.y;
    while by < r.bottom() {
        let bh = (if by == r.y { block - phase.1 } else { block }).min(r.bottom() - by);
        let mut bx = r.x;
        while bx < r.right() {
            let bw = (if bx == r.x { block - phase.0 } else { block }).min(r.right() - bx);
            let mut sum = [0u64; 3];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let p = px[y as usize * stride + x as usize];
                    sum[0] += ((p >> 16) & 255) as u64;
                    sum[1] += ((p >> 8) & 255) as u64;
                    sum[2] += (p & 255) as u64;
                }
            }
            let n = (bw * bh) as u64;
            let avg = ((sum[0] / n) as u32) << 16 | ((sum[1] / n) as u32) << 8 | (sum[2] / n) as u32;
            for y in by..by + bh {
                for x in bx..bx + bw {
                    px[y as usize * stride + x as usize] = avg;
                }
            }
            bx += block;
        }
        by += block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_and_pixelate_stay_inside_their_box() {
        // 8x4 image: left half black, right half white; effect on the middle 4x2 only.
        let (stride, h) = (8usize, 4usize);
        let fresh = || (0..stride * h).map(|i| if i % stride < 4 { 0 } else { 0x00FF_FFFF }).collect::<Vec<u32>>();
        let area = Rect { x: 2, y: 1, w: 4, h: 2 };
        for blur in [true, false] {
            let mut px = fresh();
            if blur { box_blur(&mut px, stride, area, 2) } else { pixelate(&mut px, stride, area, 4) }
            for (i, (&a, &b)) in px.iter().zip(&fresh()).enumerate() {
                let (x, y) = ((i % stride) as i32, (i / stride) as i32);
                if area.contains(x, y) {
                    assert!(a != 0 && a != 0x00FF_FFFF, "{blur} {x},{y} should be mixed, got {a:#x}");
                } else {
                    assert_eq!(a, b, "{blur} {x},{y} outside the box changed");
                }
            }
        }
    }
}
