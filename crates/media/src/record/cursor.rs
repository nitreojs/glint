//! Draws the mouse pointer onto captured frames. Desktop Duplication delivers the
//! desktop image without the cursor, plus the pointer shape and position separately.

use glint_core::record::pointer::PointerShape;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, ID3DBlob};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::{Error, PCSTR, Result, s};

const SHADER: &str = r#"
cbuffer C : register(b0) { float4 rect; };
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
V vs_main(uint id : SV_VertexID) {
    float2 uv = float2(id & 1, id >> 1);
    V o;
    o.pos = float4(lerp(rect.x, rect.z, uv.x), lerp(rect.y, rect.w, uv.y), 0, 1);
    o.uv = uv;
    return o;
}
Texture2D tex : register(t0);
SamplerState smp : register(s0);
float4 ps_main(V i) : SV_Target { return tex.Sample(smp, i.uv); }
"#;

struct Shape {
    w: i32,
    h: i32,
    /// Alpha-blended part.
    normal: ID3D11ShaderResourceView,
    /// Optional inverting part (monochrome / masked-color cursors such as the text I-beam).
    invert: Option<ID3D11ShaderResourceView>,
}

pub(crate) struct CursorRenderer {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend_alpha: ID3D11BlendState,
    blend_invert: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    shape: Option<Shape>,
}

pub(super) fn compile(source: &str, entry: PCSTR, target: PCSTR) -> Result<ID3DBlob> {
    let mut code = None;
    let mut errors = None;
    let r = unsafe {
        D3DCompile(
            source.as_ptr() as _,
            source.len(),
            None,
            None,
            None,
            entry,
            target,
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .map(|b| unsafe {
                String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()))
                    .into_owned()
            })
            .unwrap_or_default();
        return Err(Error::new(e.code(), format!("shader compile failed: {msg}")));
    }
    code.ok_or_else(|| Error::from_hresult(windows::Win32::Foundation::E_FAIL))
}

pub(super) fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

impl CursorRenderer {
    pub(crate) fn new(dev: &ID3D11Device) -> Result<Self> {
        unsafe {
            let vsb = compile(SHADER, s!("vs_main"), s!("vs_4_0"))?;
            let psb = compile(SHADER, s!("ps_main"), s!("ps_4_0"))?;
            let mut vs = None;
            dev.CreateVertexShader(blob_bytes(&vsb), None, Some(&mut vs))?;
            let mut ps = None;
            dev.CreatePixelShader(blob_bytes(&psb), None, Some(&mut ps))?;

            let mut cb = None;
            dev.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 16,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut cb),
            )?;

            let mut sampler = None;
            dev.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;

            // Standard "over" blending; destination alpha is kept as is.
            let blend_alpha = Self::blend(dev, D3D11_BLEND_SRC_ALPHA, D3D11_BLEND_INV_SRC_ALPHA)?;
            // dst = src*(1-dst) + dst*(1-src): exact XOR for 0/1 values, i.e. screen inversion.
            let blend_invert = Self::blend(dev, D3D11_BLEND_INV_DEST_COLOR, D3D11_BLEND_INV_SRC_COLOR)?;

            let mut raster = None;
            dev.CreateRasterizerState(
                &D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                Some(&mut raster),
            )?;

            Ok(Self {
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                cb: cb.unwrap(),
                sampler: sampler.unwrap(),
                blend_alpha,
                blend_invert,
                raster: raster.unwrap(),
                shape: None,
            })
        }
    }

    fn blend(dev: &ID3D11Device, src: D3D11_BLEND, dst: D3D11_BLEND) -> Result<ID3D11BlendState> {
        let mut desc = D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: src,
            DestBlend: dst,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_ZERO,
            DestBlendAlpha: D3D11_BLEND_ONE,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut s = None;
        unsafe { dev.CreateBlendState(&desc, Some(&mut s))? };
        Ok(s.unwrap())
    }

    /// Pointer size (w, h) of the current shape, if any.
    pub(crate) fn size(&self) -> Option<(i32, i32)> {
        self.shape.as_ref().map(|s| (s.w, s.h))
    }

    /// Converts a pointer shape into BGRA textures.
    pub(crate) fn set_shape(&mut self, dev: &ID3D11Device, shape: &PointerShape) -> Result<()> {
        let Some(b) = shape.bitmaps() else {
            self.shape = None;
            return Ok(());
        };
        let normal = Self::texture(dev, b.width, b.height, &b.normal)?;
        let invert = match b.invert {
            Some(px) => Some(Self::texture(dev, b.width, b.height, &px)?),
            None => None,
        };
        self.shape = Some(Shape { w: b.width as i32, h: b.height as i32, normal, invert });
        Ok(())
    }

    fn texture(dev: &ID3D11Device, w: usize, h: usize, px: &[u32]) -> Result<ID3D11ShaderResourceView> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w as u32,
            Height: h as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_IMMUTABLE,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: px.as_ptr() as _, SysMemPitch: (w * 4) as u32, SysMemSlicePitch: 0 };
        unsafe {
            let mut tex = None;
            dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
            let mut srv = None;
            dev.CreateShaderResourceView(tex.as_ref().unwrap(), None, Some(&mut srv))?;
            Ok(srv.unwrap())
        }
    }

    /// Draws the pointer with its top-left corner at (x, y) in render-target pixels.
    pub(crate) fn draw(&self, ctx: &ID3D11DeviceContext, rtv: &ID3D11RenderTargetView, tw: i32, th: i32, x: i32, y: i32) {
        let Some(shape) = &self.shape else { return };
        if x >= tw || y >= th || x + shape.w <= 0 || y + shape.h <= 0 {
            return;
        }
        let ndc_x = |px: i32| px as f32 / tw as f32 * 2.0 - 1.0;
        let ndc_y = |py: i32| 1.0 - py as f32 / th as f32 * 2.0;
        let rect = [ndc_x(x), ndc_y(y), ndc_x(x + shape.w), ndc_y(y + shape.h)];
        unsafe {
            ctx.UpdateSubresource(&self.cb, 0, None, rect.as_ptr() as _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: tw as f32,
                Height: th as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));

            ctx.PSSetShaderResources(0, Some(&[Some(shape.normal.clone())]));
            ctx.OMSetBlendState(&self.blend_alpha, None, 0xFFFF_FFFF);
            ctx.Draw(4, 0);
            if let Some(inv) = &shape.invert {
                ctx.PSSetShaderResources(0, Some(&[Some(inv.clone())]));
                ctx.OMSetBlendState(&self.blend_invert, None, 0xFFFF_FFFF);
                ctx.Draw(4, 0);
            }

            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
        }
    }
}
