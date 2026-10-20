use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use windows::Win32::Foundation::{E_ACCESSDENIED, HANDLE, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED};
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

use super::{Ctx, INFINITE, Msg, Params, RING, adapter_and_output, create_device, qpc, texture_desc};
use crate::platform::log::log;
use crate::platform::rect::Rect;

pub(super) struct CaptureStats {
    pub(super) dropped: u64,
    pub(super) coalesced: u64,
}

pub(super) struct Capture {
    region: Rect,
    draw_cursor: bool,
    output: IDXGIOutput1,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dupl: Option<IDXGIOutputDuplication>,
    desktop_size: (u32, u32),
    ring: Vec<(ID3D11Texture2D, IDXGIKeyedMutex)>,
    pub(super) handles: Vec<HANDLE>,
}

impl Capture {
    pub(super) unsafe fn new(p: &Params) -> Result<Self, String> {
        unsafe {
            let (adapter, output) = adapter_and_output(p)?;
            let (device, ctx) = create_device(&adapter, D3D11_CREATE_DEVICE_FLAG(0))?;
            let dupl = output.DuplicateOutput(&device).ctx("DuplicateOutput")?;
            let dd = dupl.GetDesc();
            if dd.Rotation != DXGI_MODE_ROTATION_IDENTITY && dd.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
                return Err("rotated (portrait) monitors are not supported yet".into());
            }
            let desktop_size = (dd.ModeDesc.Width, dd.ModeDesc.Height);
            let r = p.region;
            if r.x < 0 || r.y < 0 || r.w <= 0 || r.h <= 0 || r.w % 2 != 0 || r.h % 2 != 0 {
                return Err(format!("invalid capture region {r:?}"));
            }
            if r.right() as u32 > desktop_size.0 || r.bottom() as u32 > desktop_size.1 {
                return Err(format!("region {r:?} is outside the {}x{} desktop", desktop_size.0, desktop_size.1));
            }
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
                handles.push(t.cast::<IDXGIResource>().ctx("IDXGIResource")?.GetSharedHandle().ctx("GetSharedHandle")?);
                ring.push((t, km));
            }
            Ok(Capture {
                region: r,
                draw_cursor: p.draw_cursor,
                output,
                device,
                ctx,
                dupl: Some(dupl),
                desktop_size,
                ring,
                handles,
            })
        }
    }

    pub(super) unsafe fn run(&mut self, stop: &AtomicBool, tx: &Sender<Msg>, free_rx: &Receiver<usize>) -> Result<CaptureStats, String> {
        unsafe {
            let r = self.region;
            let mut free: Vec<usize> = (0..RING).rev().collect();
            let mut have_image = false;
            let mut ptr_visible = false;
            let mut ptr_pos = (0i32, 0i32);
            let mut shape_size: Option<(i32, i32)> = None;
            let mut shape_buf = Vec::<u8>::new();
            let mut meta = Vec::<u32>::new();
            let mut stats = CaptureStats { dropped: 0, coalesced: 0 };
            let hits = |size: Option<(i32, i32)>, vis: bool, (x, y): (i32, i32)| match size {
                Some((w, h)) if vis => Rect { x, y, w, h }.intersect(&r).is_some(),
                _ => false,
            };

            while !stop.load(Ordering::SeqCst) {
                while let Ok(i) = free_rx.try_recv() {
                    free.push(i);
                }
                let Some(dupl) = self.dupl.clone() else {
                    self.reacquire(stop)?;
                    continue;
                };
                let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
                let mut res = None;
                match dupl.AcquireNextFrame(50, &mut info, &mut res) {
                    Ok(()) => {}
                    Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                    Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST || e.code() == E_ACCESSDENIED => {
                        log(&format!("duplication lost ({}), reacquiring", e.message()));
                        self.dupl = None;
                        continue;
                    }
                    Err(e) => return Err(format!("AcquireNextFrame: {} (0x{:08X})", e.message(), e.code().0 as u32)),
                }

                let mut slot = None;
                let mut changed = false;
                let mut stamp = 0i64;
                if info.LastPresentTime != 0
                    && let Some(res) = &res
                    && (!have_image || self.region_changed(&dupl, &info, &mut meta))
                {
                    if have_image && info.AccumulatedFrames > 1 {
                        stats.coalesced += info.AccumulatedFrames as u64 - 1;
                    }
                    if let Some(i) = free.pop() {
                        let desk: ID3D11Texture2D = res.cast().ctx("desktop texture")?;
                        let bx = D3D11_BOX {
                            left: r.x as u32,
                            top: r.y as u32,
                            front: 0,
                            right: r.right() as u32,
                            bottom: r.bottom() as u32,
                            back: 1,
                        };
                        let (tex, km) = &self.ring[i];
                        km.AcquireSync(0, INFINITE).ctx("AcquireSync")?;
                        self.ctx.CopySubresourceRegion(tex, 0, 0, 0, 0, &desk, 0, Some(&bx));
                        km.ReleaseSync(1).ctx("ReleaseSync")?;
                        slot = Some(i);
                        have_image = true;
                        changed = true;
                        stamp = info.LastPresentTime;
                    } else {
                        stats.dropped += 1;
                    }
                }
                if info.LastMouseUpdateTime != 0 {
                    let vis = info.PointerPosition.Visible.as_bool();
                    let pos = (info.PointerPosition.Position.x, info.PointerPosition.Position.y);
                    if self.draw_cursor
                        && (vis != ptr_visible || pos != ptr_pos)
                        && (hits(shape_size, ptr_visible, ptr_pos) || hits(shape_size, vis, pos))
                    {
                        changed = true;
                        stamp = stamp.max(info.LastMouseUpdateTime);
                    }
                    ptr_visible = vis;
                    if vis {
                        ptr_pos = pos;
                    }
                }
                if info.PointerShapeBufferSize > 0 && self.draw_cursor {
                    shape_buf.resize(info.PointerShapeBufferSize as usize, 0);
                    let mut need = 0;
                    let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
                    if dupl
                        .GetFramePointerShape(shape_buf.len() as u32, shape_buf.as_mut_ptr() as _, &mut need, &mut si)
                        .is_ok()
                    {
                        let mono = si.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 as u32;
                        shape_size = Some((si.Width as i32, if mono { si.Height as i32 / 2 } else { si.Height as i32 }));
                        let _ = tx.send(Msg::Shape(si, shape_buf[..need.min(shape_buf.len() as u32) as usize].to_vec()));
                        if hits(shape_size, ptr_visible, ptr_pos) {
                            changed = true;
                        }
                    }
                }
                drop(res);
                let _ = dupl.ReleaseFrame();

                if changed && have_image {
                    let cursor = (self.draw_cursor && ptr_visible).then_some(ptr_pos);
                    let qpc = if stamp != 0 { stamp } else { qpc() };
                    if tx.send(Msg::Frame { slot, qpc, cursor }).is_err() {
                        break; // encoder gave up
                    }
                }
            }
            Ok(stats)
        }
    }

    /// Whether this desktop update touched the capture region (per DDA move/dirty rects).
    unsafe fn region_changed(&self, dupl: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO, buf: &mut Vec<u32>) -> bool {
        let size = info.TotalMetadataBufferSize as usize;
        if size == 0 {
            return true;
        }
        buf.resize(size.div_ceil(4), 0);
        let r = self.region;
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

    /// Re-creates the duplication after a mode change, UAC prompt or full-screen switch.
    unsafe fn reacquire(&mut self, stop: &AtomicBool) -> Result<(), String> {
        unsafe {
            while !stop.load(Ordering::SeqCst) {
                match self.output.DuplicateOutput(&self.device) {
                    Ok(d) => {
                        let dd = d.GetDesc();
                        if (dd.ModeDesc.Width, dd.ModeDesc.Height) != self.desktop_size {
                            return Err("display resolution changed".into());
                        }
                        self.dupl = Some(d);
                        return Ok(());
                    }
                    Err(_) => thread::sleep(std::time::Duration::from_millis(100)),
                }
            }
            Ok(())
        }
    }
}
