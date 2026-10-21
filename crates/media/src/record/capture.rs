use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use glint_core::geom::rect::Rect;
use glint_core::log::log;
use windows::Win32::Foundation::{E_ACCESSDENIED, HANDLE, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::core::Interface;

use super::rotate::{Rotation, Rotator, surface_rect, texel_map};
use super::{Ctx, INFINITE, Msg, Params, RING, adapter, create_device, qpc, texture_desc};
use crate::rect::WinRect;

pub(super) struct CaptureStats {
    pub(super) dropped: u64,
    pub(super) coalesced: u64,
}

/// One duplicated output and the part of the region it covers.
struct Feed {
    output: IDXGIOutput1,
    dupl: Option<IDXGIOutputDuplication>,
    desktop: Rect,
    rotation: Rotation,
    /// Covered part of the region, in output pixels.
    part: Rect,
    /// `part` in the duplicated surface, which rotated outputs deliver in panel orientation.
    src: Rect,
    /// Where `part` lands in the region image.
    dst: (u32, u32),
    /// An image of this output has been captured.
    seen: bool,
    pointer: Pointer,
}

#[derive(Default)]
struct Pointer {
    visible: bool,
    /// Desktop position of the shape's top-left corner.
    pos: (i32, i32),
    /// QPC time of the last position update.
    time: i64,
    /// Latest shape from this output's duplication, with its size and a change counter.
    shape: Option<(DXGI_OUTDUPL_POINTER_SHAPE_INFO, Vec<u8>, (i32, i32))>,
    generation: u64,
}

/// What one pass over the outputs produced.
#[derive(Default)]
struct Tick {
    /// Ring texture holding a new region image.
    slot: Option<usize>,
    /// The composed image changed (several outputs only).
    composed: bool,
    stamp: i64,
    mouse: i64,
}

pub(super) struct Capture {
    region: Rect,
    draw_cursor: bool,
    pub(super) adapter: IDXGIAdapter1,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    feeds: Vec<Feed>,
    /// The fastest output: polling waits on it, the others are only checked.
    pacer: usize,
    wait_ms: u32,
    /// The region assembled from all outputs; `None` with a single output, which copies
    /// straight into the ring.
    compose: Option<ID3D11Texture2D>,
    ring: Vec<(ID3D11Texture2D, IDXGIKeyedMutex)>,
    /// Present when an output is rotated.
    rotator: Option<Rotator>,
    pub(super) handles: Vec<HANDLE>,
    free: Vec<usize>,
    meta: Vec<u32>,
    stats: CaptureStats,
}

impl Capture {
    pub(super) unsafe fn new(p: &Params) -> Result<Self, String> {
        unsafe {
            let Some(first) = p.sources.first() else { return Err("no monitor to record".into()) };
            if p.sources.iter().any(|s| s.adapter_idx != first.adapter_idx) {
                return Err("recording across monitors on different GPUs isn't supported".into());
            }
            let r = p.region;
            if r.w <= 0 || r.h <= 0 || r.w % 2 != 0 || r.h % 2 != 0 {
                return Err(format!("invalid capture region {r:?}"));
            }
            let adapter = adapter(first.adapter_idx)?;
            let (device, ctx) = create_device(&adapter, D3D11_CREATE_DEVICE_FLAG(0))?;
            let mut feeds = Vec::with_capacity(p.sources.len());
            let mut rates = Vec::with_capacity(p.sources.len());
            for s in &p.sources {
                let output: IDXGIOutput1 =
                    adapter.EnumOutputs(s.output_idx).ctx("EnumOutputs")?.cast().ctx("IDXGIOutput1")?;
                let dupl = output.DuplicateOutput(&device).ctx("DuplicateOutput")?;
                let dd = dupl.GetDesc();
                let desktop = Rect::from_win(&output.GetDesc().ctx("IDXGIOutput::GetDesc")?.DesktopCoordinates);
                if desktop != s.desktop {
                    return Err("the monitor layout changed; select the region again".into());
                }
                let Some((part, dst)) = copy_rect(r, desktop) else {
                    return Err(format!("region {r:?} misses the monitor at {desktop:?}"));
                };
                let rotation = Rotation::from_dxgi(dd.Rotation);
                let src = surface_rect(rotation, part, (desktop.w, desktop.h));
                let rate = dd.ModeDesc.RefreshRate;
                rates.push(if rate.Denominator != 0 { rate.Numerator as f64 / rate.Denominator as f64 } else { 0.0 });
                let pointer = Pointer::default();
                feeds.push(Feed { output, dupl: Some(dupl), desktop, rotation, part, src, dst, seen: false, pointer });
            }

            let compose = if feeds.len() > 1 {
                let desc = texture_desc(r, D3D11_BIND_RENDER_TARGET, D3D11_RESOURCE_MISC_FLAG(0));
                let mut t = None;
                device.CreateTexture2D(&desc, None, Some(&mut t)).ctx("CreateTexture2D (compose)")?;
                let t = t.unwrap();
                clear(&device, &ctx, &t)?;
                Some(t)
            } else {
                None
            };
            let mut ring = Vec::with_capacity(RING);
            let mut handles = Vec::with_capacity(RING);
            let desc = texture_desc(
                r,
                D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET,
                D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX,
            );
            for _ in 0..RING {
                let mut t = None;
                device.CreateTexture2D(&desc, None, Some(&mut t)).ctx("CreateTexture2D (shared)")?;
                let t = t.unwrap();
                let km: IDXGIKeyedMutex = t.cast().ctx("IDXGIKeyedMutex")?;
                km.AcquireSync(0, INFINITE).ctx("AcquireSync")?;
                let cleared = clear(&device, &ctx, &t);
                km.ReleaseSync(0).ctx("ReleaseSync")?;
                cleared?;
                handles.push(t.cast::<IDXGIResource>().ctx("IDXGIResource")?.GetSharedHandle().ctx("GetSharedHandle")?);
                ring.push((t, km));
            }
            let rotator = if feeds.iter().any(|f| f.rotation != Rotation::Identity) {
                let targets: Vec<ID3D11Texture2D> = ring.iter().map(|(t, _)| t.clone()).collect();
                Some(Rotator::new(&device, &targets, compose.as_ref())?)
            } else {
                None
            };
            Ok(Capture {
                region: r,
                draw_cursor: p.draw_cursor,
                adapter,
                device,
                ctx,
                pacer: pacer(&rates),
                wait_ms: wait_ms(p.fps, feeds.len()),
                feeds,
                compose,
                ring,
                rotator,
                handles,
                free: (0..RING).rev().collect(),
                meta: Vec::new(),
                stats: CaptureStats { dropped: 0, coalesced: 0 },
            })
        }
    }

    pub(super) unsafe fn run(&mut self, stop: &AtomicBool, tx: &Sender<Msg>, free_rx: &Receiver<usize>) -> Result<CaptureStats, String> {
        unsafe {
            let r = self.region;
            // acquirenextframe timeouts round up to the system timer tick (15.6 ms by default).
            let _fine_timer = (self.feeds.len() > 1).then(FineTimer::new);
            let order: Vec<usize> =
                std::iter::once(self.pacer).chain((0..self.feeds.len()).filter(|&i| i != self.pacer)).collect();
            let mut sent_shape: Option<(usize, u64)> = None;
            let hits = |p: Option<Rect>| p.is_some_and(|p| p.intersect(&r).is_some());

            while !stop.load(Ordering::SeqCst) {
                while let Ok(i) = free_rx.try_recv() {
                    self.free.push(i);
                }
                let before = self.pointer(sent_shape);
                let mut tick = Tick::default();
                for &i in &order {
                    let timeout = if i == self.pacer { self.wait_ms } else { 0 };
                    self.poll(i, timeout, stop, &mut tick)?;
                }
                let ready = self.feeds.iter().all(|f| f.seen);
                if tick.composed
                    && ready
                    && let Some(compose) = &self.compose
                {
                    if let Some(i) = self.free.pop() {
                        let (tex, km) = &self.ring[i];
                        km.AcquireSync(0, INFINITE).ctx("AcquireSync")?;
                        self.ctx.CopyResource(tex, compose);
                        km.ReleaseSync(1).ctx("ReleaseSync")?;
                        tick.slot = Some(i);
                    } else {
                        self.stats.dropped += 1;
                    }
                }

                let mut changed = tick.slot.is_some();
                let mut stamp = tick.stamp;
                let mut after = None;
                if self.draw_cursor {
                    let mut shape_sent = false;
                    if let Some(t) = self.shape_source(sent_shape.map(|s| s.0))
                        && let Some((si, buf, _)) = &self.feeds[t].pointer.shape
                        && sent_shape != Some((t, self.feeds[t].pointer.generation))
                    {
                        let _ = tx.send(Msg::Shape(*si, buf.clone()));
                        sent_shape = Some((t, self.feeds[t].pointer.generation));
                        shape_sent = true;
                    }
                    after = self.pointer(sent_shape);
                    if (after != before || shape_sent) && (hits(before) || hits(after)) {
                        changed = true;
                        stamp = stamp.max(tick.mouse);
                    }
                }

                if changed && ready {
                    let cursor = after.map(|p| (p.x, p.y));
                    let qpc = if stamp != 0 { stamp } else { qpc() };
                    if tx.send(Msg::Frame { slot: tick.slot, qpc, cursor }).is_err() {
                        break; // encoder gave up
                    }
                }
            }
            Ok(CaptureStats { dropped: self.stats.dropped, coalesced: self.stats.coalesced })
        }
    }

    /// Takes the next frame of output `i` if one arrives within `timeout` ms: its part of the
    /// region goes into the ring (single output) or the composed image, pointer updates are kept.
    unsafe fn poll(&mut self, i: usize, timeout: u32, stop: &AtomicBool, tick: &mut Tick) -> Result<(), String> {
        unsafe {
            let Some(dupl) = self.feeds[i].dupl.clone() else { return self.reacquire(i, stop) };
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res = None;
            match dupl.AcquireNextFrame(timeout, &mut info, &mut res) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(()),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST || e.code() == E_ACCESSDENIED => {
                    log(&format!("duplication of output {i} lost ({}), reacquiring", e.message()));
                    self.feeds[i].dupl = None;
                    return Ok(());
                }
                Err(e) => return Err(format!("AcquireNextFrame: {} (0x{:08X})", e.message(), e.code().0 as u32)),
            }

            let f = &mut self.feeds[i];
            if info.LastPresentTime != 0
                && let Some(res) = &res
                && (!f.seen || region_changed(&dupl, &info, f.src, &mut self.meta))
            {
                if f.seen && info.AccumulatedFrames > 1 {
                    self.stats.coalesced += info.AccumulatedFrames as u64 - 1;
                }
                let desk: ID3D11Texture2D = res.cast().ctx("desktop texture")?;
                let rot = self.rotator.as_ref();
                if let Some(compose) = &self.compose {
                    put(&self.device, &self.ctx, rot, f, &desk, compose, None)?;
                    f.seen = true;
                    tick.composed = true;
                    tick.stamp = tick.stamp.max(info.LastPresentTime);
                } else if let Some(s) = self.free.pop() {
                    let (tex, km) = &self.ring[s];
                    km.AcquireSync(0, INFINITE).ctx("AcquireSync")?;
                    let placed = put(&self.device, &self.ctx, rot, f, &desk, tex, Some(s));
                    km.ReleaseSync(1).ctx("ReleaseSync")?;
                    placed?;
                    f.seen = true;
                    tick.slot = Some(s);
                    tick.stamp = tick.stamp.max(info.LastPresentTime);
                } else {
                    self.stats.dropped += 1;
                }
            }
            if info.LastMouseUpdateTime != 0 {
                let vis = info.PointerPosition.Visible.as_bool();
                f.pointer.visible = vis;
                if vis {
                    let pos = info.PointerPosition.Position;
                    f.pointer.pos = (f.desktop.x + pos.x, f.desktop.y + pos.y);
                }
                f.pointer.time = info.LastMouseUpdateTime;
                tick.mouse = tick.mouse.max(info.LastMouseUpdateTime);
            }
            if info.PointerShapeBufferSize > 0 && self.draw_cursor {
                let mut buf = vec![0u8; info.PointerShapeBufferSize as usize];
                let mut need = 0;
                let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                if dupl.GetFramePointerShape(buf.len() as u32, buf.as_mut_ptr() as _, &mut need, &mut si).is_ok() {
                    buf.truncate(need as usize);
                    let mono = si.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
                    let size = (si.Width as i32, if mono { si.Height as i32 / 2 } else { si.Height as i32 });
                    f.pointer.shape = Some((si, buf, size));
                    f.pointer.generation += 1;
                }
            }
            drop(res);
            let _ = dupl.ReleaseFrame();
            Ok(())
        }
    }

    /// The visible pointer as drawn: desktop position and the size of the shape last sent
    /// to the encoder (empty when none was).
    fn pointer(&self, sent_shape: Option<(usize, u64)>) -> Option<Rect> {
        let p = &self.feeds[newest_visible(self.feeds.iter().map(|f| (f.pointer.visible, f.pointer.time)))?].pointer;
        let (w, h) = sent_shape.and_then(|(i, _)| self.feeds[i].pointer.shape.as_ref()).map_or((0, 0), |s| s.2);
        Some(Rect { x: p.pos.0, y: p.pos.1, w, h })
    }

    /// The output whose pointer shape should be drawn: the one showing the pointer, else the
    /// one the current shape came from, else any that reported a shape.
    fn shape_source(&self, current: Option<usize>) -> Option<usize> {
        newest_visible(self.feeds.iter().map(|f| (f.pointer.visible, f.pointer.time)))
            .or(current)
            .or_else(|| self.feeds.iter().position(|f| f.pointer.shape.is_some()))
    }

    /// Re-creates output `i`'s duplication after a mode change, UAC prompt or full-screen switch.
    unsafe fn reacquire(&mut self, i: usize, stop: &AtomicBool) -> Result<(), String> {
        unsafe {
            let f = &mut self.feeds[i];
            while !stop.load(Ordering::SeqCst) {
                match f.output.DuplicateOutput(&self.device) {
                    Ok(d) => {
                        if Rotation::from_dxgi(d.GetDesc().Rotation) != f.rotation {
                            return Err("display orientation changed".into());
                        }
                        let desc = f.output.GetDesc().ctx("IDXGIOutput::GetDesc")?;
                        let desktop = Rect::from_win(&desc.DesktopCoordinates);
                        if (desktop.w, desktop.h) != (f.desktop.w, f.desktop.h) {
                            return Err("display resolution changed".into());
                        }
                        if desktop != f.desktop {
                            return Err("monitor arrangement changed".into());
                        }
                        f.dupl = Some(d);
                        return Ok(());
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            Ok(())
        }
    }
}

/// Raises the timer resolution to 1 ms while alive.
struct FineTimer;

impl FineTimer {
    fn new() -> FineTimer {
        unsafe {
            timeBeginPeriod(1);
        }
        FineTimer
    }
}

impl Drop for FineTimer {
    fn drop(&mut self) {
        unsafe {
            timeEndPeriod(1);
        }
    }
}

unsafe fn clear(device: &ID3D11Device, ctx: &ID3D11DeviceContext, tex: &ID3D11Texture2D) -> Result<(), String> {
    unsafe {
        let mut rtv = None;
        device.CreateRenderTargetView(tex, None, Some(&mut rtv)).ctx("CreateRenderTargetView")?;
        ctx.ClearRenderTargetView(rtv.as_ref().unwrap(), &[0.0, 0.0, 0.0, 1.0]);
        Ok(())
    }
}

/// Puts `f`'s part of the desktop image `desk` into `target`: ring texture `slot`, or the
/// composed image when `slot` is `None`.
unsafe fn put(
    device: &ID3D11Device,
    ctx: &ID3D11DeviceContext,
    rotator: Option<&Rotator>,
    f: &Feed,
    desk: &ID3D11Texture2D,
    target: &ID3D11Texture2D,
    slot: Option<usize>,
) -> Result<(), String> {
    unsafe {
        if f.rotation != Rotation::Identity
            && let Some(r) = rotator
        {
            let map = texel_map(f.rotation, f.part, (f.desktop.w, f.desktop.h));
            let viewport = Rect { x: f.dst.0 as i32, y: f.dst.1 as i32, w: f.part.w, h: f.part.h };
            return r.draw(device, ctx, slot, desk, map, viewport);
        }
        let bx = D3D11_BOX {
            left: f.src.x as u32,
            top: f.src.y as u32,
            front: 0,
            right: f.src.right() as u32,
            bottom: f.src.bottom() as u32,
            back: 1,
        };
        ctx.CopySubresourceRegion(target, 0, f.dst.0, f.dst.1, 0, desk, 0, Some(&bx));
        Ok(())
    }
}

/// The part of `region` (desktop) that the output at `desktop` shows: in output pixels, and
/// its offset inside the region.
fn copy_rect(region: Rect, desktop: Rect) -> Option<(Rect, (u32, u32))> {
    let part = region.intersect(&desktop)?;
    Some((part.offset(-desktop.x, -desktop.y), ((part.x - region.x) as u32, (part.y - region.y) as u32)))
}

/// Index of the highest refresh rate (the first on ties).
fn pacer(rates: &[f64]) -> usize {
    rates.iter().enumerate().fold(0, |best, (i, &r)| if r > rates[best] { i } else { best })
}

/// How long to wait for the pacing output: with several outputs the others are only checked
/// between waits, so it must not exceed a frame.
fn wait_ms(fps: f64, outputs: usize) -> u32 {
    if outputs > 1 { (1000.0 / fps).floor().clamp(1.0, 50.0) as u32 } else { 50 }
}

/// Among (visible, last update time) pointer states, the most recently updated visible one.
fn newest_visible(pointers: impl Iterator<Item = (bool, i64)>) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, (visible, time)) in pointers.enumerate() {
        if visible && best.is_none_or(|(_, t)| time > t) {
            best = Some((i, time));
        }
    }
    best.map(|b| b.0)
}

/// Whether this desktop update touched `r` (output pixels), per DDA move/dirty rects.
unsafe fn region_changed(dupl: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO, r: Rect, buf: &mut Vec<u32>) -> bool {
    let size = info.TotalMetadataBufferSize as usize;
    if size == 0 {
        return true;
    }
    buf.resize(size.div_ceil(4), 0);
    let mut need = 0u32;
    unsafe {
        if dupl.GetFrameMoveRects(size as u32, buf.as_mut_ptr() as *mut DXGI_OUTDUPL_MOVE_RECT, &mut need).is_err() {
            return true;
        }
        let moves = std::slice::from_raw_parts(
            buf.as_ptr() as *const DXGI_OUTDUPL_MOVE_RECT,
            need as usize / size_of::<DXGI_OUTDUPL_MOVE_RECT>(),
        );
        if moves.iter().any(|m| Rect::from_win(&m.DestinationRect).intersect(&r).is_some()) {
            return true;
        }
        if dupl.GetFrameDirtyRects(size as u32, buf.as_mut_ptr() as *mut RECT, &mut need).is_err() {
            return true;
        }
        let dirty = std::slice::from_raw_parts(buf.as_ptr() as *const RECT, need as usize / size_of::<RECT>());
        dirty.iter().any(|d| Rect::from_win(d).intersect(&r).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: Rect = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    const LEFT: Rect = Rect { x: -1920, y: 540, w: 1920, h: 1080 };
    const TOP: Rect = Rect { x: 365, y: -1440, w: 2560, h: 1440 };

    #[test]
    fn copy_rect_inside_one_output() {
        let region = Rect { x: 100, y: 200, w: 640, h: 480 };
        assert_eq!(copy_rect(region, MAIN), Some((region, (0, 0))));
        assert_eq!(copy_rect(region.offset(-1920, 540), LEFT), Some((Rect { x: 100, y: 200, w: 640, h: 480 }, (0, 0))));
    }

    #[test]
    fn copy_rect_spanning_with_gaps() {
        let region = Rect { x: -1000, y: 400, w: 2000, h: 1300 };
        assert_eq!(copy_rect(region, MAIN), Some((Rect { x: 0, y: 400, w: 1000, h: 1040 }, (1000, 0))));
        assert_eq!(copy_rect(region, LEFT), Some((Rect { x: 920, y: 0, w: 1000, h: 1080 }, (0, 140))));
        assert_eq!(copy_rect(region, TOP), None);
    }

    #[test]
    fn copy_rect_above_and_below() {
        let region = Rect { x: 0, y: -100, w: 1000, h: 300 };
        assert_eq!(copy_rect(region, TOP), Some((Rect { x: 0, y: 1340, w: 635, h: 100 }, (365, 0))));
        assert_eq!(copy_rect(region, MAIN), Some((Rect { x: 0, y: 0, w: 1000, h: 200 }, (0, 100))));
    }

    #[test]
    fn pacer_is_fastest_output() {
        assert_eq!(pacer(&[144.0]), 0);
        assert_eq!(pacer(&[144.0, 75.0]), 0);
        assert_eq!(pacer(&[75.0, 144.0, 200.0]), 2);
        assert_eq!(pacer(&[60.0, 60.0]), 0);
        assert_eq!(pacer(&[0.0, 0.0]), 0);
    }

    #[test]
    fn wait_fits_a_frame() {
        assert_eq!(wait_ms(144.0, 1), 50);
        assert_eq!(wait_ms(144.0, 2), 6);
        assert_eq!(wait_ms(200.0, 3), 5);
        assert_eq!(wait_ms(75.0, 2), 13);
        assert_eq!(wait_ms(10.0, 2), 50);
        assert_eq!(wait_ms(2000.0, 2), 1);
    }

    #[test]
    fn newest_visible_pointer_wins() {
        assert_eq!(newest_visible([(false, 5), (false, 9)].into_iter()), None);
        assert_eq!(newest_visible([(true, 5), (false, 9)].into_iter()), Some(0));
        assert_eq!(newest_visible([(true, 5), (true, 9), (true, 7)].into_iter()), Some(1));
        assert_eq!(newest_visible([(true, 9), (true, 9)].into_iter()), Some(0));
    }
}
