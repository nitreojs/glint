use std::sync::Arc;

use glint_core::clip::timeline_cache::{THUMB_HEIGHT, Thumb};
use glint_core::record::negotiate::{Input, Matrix};
use glint_core::record::preview::FRAME_STEP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;
use windows::Win32::Media::MediaFoundation::{IMFDXGIBuffer, IMFSample};
use windows::core::Interface;

use super::Ctx;
use super::convert::Converter;

pub(super) struct Sampler {
    converter: Converter,
    size: (u32, u32),
    target: ID3D11Texture2D,
    staging: ID3D11Texture2D,
    pending: Option<i64>,
    next: i64,
}

impl Sampler {
    pub(super) fn new(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        size: (u32, u32),
        input: Input,
        matrix: Matrix,
    ) -> Result<Self, String> {
        let w = ((u64::from(size.0) * u64::from(THUMB_HEIGHT) + u64::from(size.1) / 2) / u64::from(size.1))
            .clamp(8, 512) as u32 & !1;
        let thumb = (w, THUMB_HEIGHT);
        let nv12 = input == Input::Nv12;
        let bgra = DXGI_FORMAT_B8G8R8A8_UNORM;
        let format = if nv12 { DXGI_FORMAT_NV12 } else { bgra };
        let converter = unsafe { Converter::scaled(device, ctx, size, thumb, format, bgra)? };
        converter.to_rgb(nv12, matrix);
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: thumb.0,
            Height: thumb.1,
            MipLevels: 1,
            ArraySize: 1,
            Format: bgra,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
            ..Default::default()
        };
        let mut target = None;
        let mut staging = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut target)) }.ctx("thumbnail target")?;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.ctx("thumbnail staging")?;

        Ok(Self { converter, size: thumb, target: target.unwrap(), staging: staging.unwrap(), pending: None, next: 0 })
    }

    pub(super) fn due(&self, at: i64) -> bool {
        self.pending.is_some() || at >= self.next
    }

    pub(super) fn frame(&mut self, ctx: &ID3D11DeviceContext, sample: &IMFSample, at: i64) -> Result<Option<Thumb>, String> {
        let ready = self.read(ctx, false)?;
        if self.pending.is_none() && at >= self.next {
            unsafe {
                let dx: IMFDXGIBuffer = sample.GetBufferByIndex(0).ctx("thumbnail buffer")?.cast().ctx("thumbnail DXGI buffer")?;
                let mut raw = std::ptr::null_mut();
                dx.GetResource(&ID3D11Texture2D::IID, &mut raw).ctx("thumbnail texture")?;
                let src = ID3D11Texture2D::from_raw(raw);
                let sub = dx.GetSubresourceIndex().ctx("thumbnail slice")?;
                self.converter.blit(&src, sub, &self.target, 0)?;
                ctx.CopyResource(&self.staging, &self.target);
            }
            self.pending = Some(at);
            self.next = (at.div_euclid(FRAME_STEP) + 1) * FRAME_STEP;
        }

        Ok(ready)
    }

    pub(super) fn read(&mut self, ctx: &ID3D11DeviceContext, finish: bool) -> Result<Option<Thumb>, String> {
        let Some(at) = self.pending else { return Ok(None) };
        let flags = if finish { 0 } else { D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32 };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        let (w, h) = (self.size.0 as usize, self.size.1 as usize);
        let rgb = unsafe {
            match ctx.Map(&self.staging, 0, D3D11_MAP_READ, flags, Some(&mut mapped)) {
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => return Ok(None),
                r => r.ctx("read thumbnail")?,
            }
            let mut rgb = Vec::with_capacity(w * h * 3);
            for y in 0..h {
                let p = mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize);
                let row = std::slice::from_raw_parts(p, w * 4);
                for c in row.chunks_exact(4) {
                    rgb.extend_from_slice(&[c[2], c[1], c[0]]);
                }
            }
            ctx.Unmap(&self.staging, 0);

            rgb
        };
        self.pending = None;

        Ok(Some(Thumb { at, size: self.size, rgb: Arc::new(rgb) }))
    }
}
