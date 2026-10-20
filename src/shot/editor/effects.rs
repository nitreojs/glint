use super::marks::Mark;
use crate::platform::gfx::Dib;
use crate::platform::rect::Rect;

/// Applies a blur / pixelate mark to `dib`, which shows the screen area `sel`.
pub(super) fn apply_effect(dib: &mut Dib, m: &Mark, sel: Rect, scale: f32) {
    let (r, blur, strength) = match m {
        Mark::Blur { rect, radius } => (*rect, true, *radius),
        Mark::Pixelate { rect, block } => (*rect, false, *block),
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
    let mut by = r.y;
    while by < r.bottom() {
        let bh = block.min(r.bottom() - by);
        let mut bx = r.x;
        while bx < r.right() {
            let bw = block.min(r.right() - bx);
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
