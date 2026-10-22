use std::cell::RefCell;

use glint_core::clip::clicks::{into_output, paint_bgrx};
use glint_core::clip::crop::Crop;
use glint_core::clip::export::Burn;
use glint_core::record::clicks::{Shape, shapes_at};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

use crate::clicks::Renderer;
use crate::record::Ctx;

pub(super) struct Burner {
    burn: Burn,
    crop: Crop,
    out: (u32, u32),
    shapes: RefCell<Vec<Shape>>,
    renderer: RefCell<Option<Renderer>>,
}

impl Burner {
    pub(super) fn new(burn: &Burn, crop: Crop, out: (u32, u32)) -> Burner {
        Burner { burn: burn.clone(), crop, out, shapes: RefCell::new(Vec::new()), renderer: RefCell::new(None) }
    }

    pub(super) fn draw(&self, sample: &IMFSample, t: i64) -> Result<(), String> {
        let mut shapes = self.shapes.borrow_mut();
        shapes_at(&self.burn.clicks, t, &self.burn.style, &mut shapes);
        if shapes.is_empty() {
            return Ok(());
        }
        into_output(&mut shapes, self.crop, self.out);
        unsafe {
            let buf = sample.GetBufferByIndex(0).ctx("GetBufferByIndex")?;
            match buf.cast::<IMFDXGIBuffer>() {
                Ok(dx) => self.on_gpu(&dx, &shapes),
                Err(_) => in_memory(&buf, self.out, &shapes),
            }
        }
    }

    unsafe fn on_gpu(&self, dx: &IMFDXGIBuffer, shapes: &[Shape]) -> Result<(), String> {
        unsafe {
            let mut raw = std::ptr::null_mut();
            dx.GetResource(&ID3D11Texture2D::IID, &mut raw).ctx("IMFDXGIBuffer::GetResource")?;
            let tex = ID3D11Texture2D::from_raw(raw);
            let sub = dx.GetSubresourceIndex().ctx("GetSubresourceIndex")?;
            let mut td = D3D11_TEXTURE2D_DESC::default();
            tex.GetDesc(&mut td);
            let device = tex.GetDevice().ctx("GetDevice")?;
            let mut desc = D3D11_RENDER_TARGET_VIEW_DESC {
                Format: td.Format,
                ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                ..Default::default()
            };
            desc.Anonymous.Texture2DArray = D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: sub, ArraySize: 1 };
            let mut rtv = None;
            device.CreateRenderTargetView(&tex, Some(&desc), Some(&mut rtv)).ctx("CreateRenderTargetView (clicks)")?;
            let rtv = rtv.ok_or("no render target for the clicks")?;
            let mut renderer = self.renderer.borrow_mut();
            if renderer.is_none() {
                *renderer = Some(Renderer::new(&device)?);
            }
            let ctx = device.GetImmediateContext().ctx("GetImmediateContext")?;
            let mt: ID3D11Multithread = device.cast().ctx("ID3D11Multithread")?;
            mt.Enter();
            let drawn = renderer.as_ref().map_or(Ok(()), |r| r.draw(&ctx, &rtv, self.out, shapes));
            mt.Leave();
            drawn
        }
    }
}

unsafe fn in_memory(buf: &IMFMediaBuffer, size: (u32, u32), shapes: &[Shape]) -> Result<(), String> {
    unsafe {
        if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
            let (mut scan0, mut pitch) = (std::ptr::null_mut(), 0);
            b2.Lock2D(&mut scan0, &mut pitch).ctx("Lock2D")?;
            if pitch > 0 {
                let px = std::slice::from_raw_parts_mut(scan0, pitch as usize * size.1 as usize);
                paint_bgrx(px, pitch as usize, size, shapes);
            }
            let _ = b2.Unlock2D();
            return if pitch > 0 { Ok(()) } else { Err("bottom-up frames can't take clicks".into()) };
        }
        let (mut data, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut data, None, Some(&mut len)).ctx("Lock")?;
        let pitch = size.0 as usize * 4;
        let fits = len as usize >= pitch * size.1 as usize;
        if fits {
            paint_bgrx(std::slice::from_raw_parts_mut(data, pitch * size.1 as usize), pitch, size, shapes);
        }
        let _ = buf.Unlock();
        if fits { Ok(()) } else { Err("the frame is smaller than expected".into()) }
    }
}
