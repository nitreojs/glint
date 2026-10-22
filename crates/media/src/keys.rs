//! Draws keycap pills ([`Pill`]s from [`glint_core::record::keys::state_at`]) into a D3D11
//! render target: each distinct pill is painted once by Direct2D/DirectWrite into a cached
//! texture, then drawn as an alpha-blended textured quad (the keyboard analogue of
//! [`crate::clicks`], whose shapes are procedural rather than textured).

use std::cell::RefCell;
use std::collections::HashMap;
use std::mem::ManuallyDrop;

use glint_core::record::keys::{Key, Pill, Style, state_at};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_ROUNDED_RECT, ID2D1DeviceContext, ID2D1Factory1,
    ID2D1Image, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::core::{Interface, s, w};
use crate::record::Ctx;
use crate::record::cursor::{blob_bytes, compile};

const SHADER: &str = r#"
cbuffer C : register(b0) { float4 rect; float4 tint; };
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
// The cached text texture is premultiplied; tint folds in the pill's alpha.
float4 ps_main(V i) : SV_Target { return tex.Sample(smp, i.uv) * tint; }
"#;

#[repr(C)]
struct Quad {
    rect: [f32; 4],
    tint: [f32; 4],
}

/// Cached pills keyed by their text and pixel size; the baked text texture as an SRV.
type Cache = HashMap<(String, u32, u32), ID3D11ShaderResourceView>;

pub struct Renderer {
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    d2d: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    formats: RefCell<HashMap<u32, IDWriteTextFormat>>,
    widths: RefCell<HashMap<(String, u32), f32>>,
    cache: RefCell<Cache>,
}

impl Renderer {
    pub fn new(device: &ID3D11Device) -> Result<Renderer, String> {
        unsafe {
            let vsb = compile(SHADER, s!("vs_main"), s!("vs_4_0")).ctx("keys vertex shader")?;
            let psb = compile(SHADER, s!("ps_main"), s!("ps_4_0")).ctx("keys pixel shader")?;
            let mut vs = None;
            device.CreateVertexShader(blob_bytes(&vsb), None, Some(&mut vs)).ctx("CreateVertexShader")?;
            let mut ps = None;
            device.CreatePixelShader(blob_bytes(&psb), None, Some(&mut ps)).ctx("CreatePixelShader")?;
            let mut cb = None;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: size_of::<Quad>() as u32,
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut cb),
                )
                .ctx("CreateBuffer")?;
            let mut sampler = None;
            device
                .CreateSamplerState(
                    &D3D11_SAMPLER_DESC {
                        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                        MaxLOD: f32::MAX,
                        ..Default::default()
                    },
                    Some(&mut sampler),
                )
                .ctx("CreateSamplerState")?;
            // Premultiplied "over"; the target's alpha is kept.
            let mut blend = D3D11_BLEND_DESC::default();
            blend.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ZERO,
                DestBlendAlpha: D3D11_BLEND_ONE,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut bs = None;
            device.CreateBlendState(&blend, Some(&mut bs)).ctx("CreateBlendState")?;
            let mut raster = None;
            device
                .CreateRasterizerState(
                    &D3D11_RASTERIZER_DESC {
                        FillMode: D3D11_FILL_SOLID,
                        CullMode: D3D11_CULL_NONE,
                        DepthClipEnable: true.into(),
                        ..Default::default()
                    },
                    Some(&mut raster),
                )
                .ctx("CreateRasterizerState")?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).ctx("D2D1CreateFactory")?;
            let dxgi: IDXGIDevice = device.cast().ctx("IDXGIDevice")?;
            let d2d_device = factory.CreateDevice(&dxgi).ctx("ID2D1Factory1::CreateDevice")?;
            let d2d = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE).ctx("CreateDeviceContext")?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ctx("DWriteCreateFactory")?;
            Ok(Renderer {
                device: device.clone(),
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                cb: cb.unwrap(),
                sampler: sampler.unwrap(),
                blend: bs.unwrap(),
                raster: raster.unwrap(),
                d2d,
                dwrite,
                formats: RefCell::new(HashMap::new()),
                widths: RefCell::new(HashMap::new()),
                cache: RefCell::new(HashMap::new()),
            })
        }
    }

    /// The width `text` takes at `font` pixels, from DirectWrite's layout metrics.
    pub fn measure(&self, text: &str, font: f32) -> f32 {
        let key = (text.to_string(), font.to_bits());
        if let Some(&w) = self.widths.borrow().get(&key) {
            return w;
        }
        let width = self.layout_width(text, font).unwrap_or_else(|| text.chars().count() as f32 * font * 0.6);
        self.widths.borrow_mut().insert(key, width);
        width
    }

    fn layout_width(&self, text: &str, font: f32) -> Option<f32> {
        let format = self.format(font).ok()?;
        let u: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let layout = self.dwrite.CreateTextLayout(&u, &format, f32::MAX, font * 2.0).ok()?;
            let mut m = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut m).ok()?;
            Some(m.width)
        }
    }

    fn format(&self, font: f32) -> Result<IDWriteTextFormat, String> {
        let key = font.to_bits();
        if let Some(f) = self.formats.borrow().get(&key) {
            return Ok(f.clone());
        }
        unsafe {
            let f = self
                .dwrite
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    font,
                    w!(""),
                )
                .ctx("CreateTextFormat")?;
            f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER).ctx("SetTextAlignment")?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER).ctx("SetParagraphAlignment")?;
            self.formats.borrow_mut().insert(key, f.clone());
            Ok(f)
        }
    }

    /// Paints `pill` as a keycap (shadow, raised bottom edge, lipped face, top highlight, outline
    /// and centred semibold label) into a premultiplied BGRA texture, the `w`×`h` cap bordered by
    /// [`margin`] so its drop shadow has room. The colours keep their own alpha; the pill's fade
    /// alpha is folded in by the quad's tint, so the bake stays frame-independent and cacheable.
    fn bake(&self, pill: &Pill, w: u32, h: u32) -> Result<ID3D11ShaderResourceView, String> {
        unsafe {
            let m = margin(pill.depth);
            let (tw, th) = (w + 2 * m, h + 2 * m);
            let desc = D3D11_TEXTURE2D_DESC {
                Width: tw,
                Height: th,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                ..Default::default()
            };
            let mut tex = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut tex)).ctx("CreateTexture2D (pill)")?;
            let tex = tex.unwrap();
            let surface: IDXGISurface = tex.cast().ctx("IDXGISurface")?;
            let bmp_props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET,
                colorContext: ManuallyDrop::new(None),
            };
            let bmp = self.d2d.CreateBitmapFromDxgiSurface(&surface, Some(&bmp_props)).ctx("CreateBitmapFromDxgiSurface")?;
            self.d2d.SetTarget(&bmp);
            let fill = brush(&self.d2d, pill.fill)?;
            let edge = brush(&self.d2d, pill.edge)?;
            let top = brush(&self.d2d, pill.top)?;
            let border = brush(&self.d2d, pill.border)?;
            let shadow = brush(&self.d2d, pill.shadow)?;
            let text = brush(&self.d2d, pill.text_color)?;
            let format = self.format(pill.font)?;
            let (mf, r, d) = (m as f32, pill.radius, pill.depth);
            // the cap's rectangle inside the bordered texture.
            let (l, t, rr, b) = (mf, mf, mf + w as f32, mf + h as f32);
            let rounded = |rect: D2D_RECT_F| D2D1_ROUNDED_RECT { rect, radiusX: r, radiusY: r };
            let face = rounded(D2D_RECT_F { left: l, top: t, right: rr, bottom: b - d });
            self.d2d.BeginDraw();
            self.d2d.Clear(Some(&color([0.0; 4])));
            // 1. drop shadow: the cap silhouette, offset down and outset 1px, behind everything.
            self.d2d.FillRoundedRectangle(&rounded(D2D_RECT_F { left: l - 1.0, top: t - 1.0 + d, right: rr + 1.0, bottom: b + 1.0 + d }), &shadow);
            // 2. raised bottom edge: the whole cap, the darker lip showing below the face.
            self.d2d.FillRoundedRectangle(&rounded(D2D_RECT_F { left: l, top: t, right: rr, bottom: b }), &edge);
            // 3. face: the cap minus its bottom `d` px, leaving the edge as the 3D lip.
            self.d2d.FillRoundedRectangle(&face, &fill);
            // 4. top highlight: a ~1.5px bright strip just inside the face's top.
            self.d2d.FillRectangle(&D2D_RECT_F { left: l + r, top: t + 0.5, right: rr - r, bottom: t + 2.0 }, &top);
            // 5. outline around the face.
            self.d2d.DrawRoundedRectangle(&face, &border, 1.0, None);
            // 6. label centred by ink in the face area above the lip.
            let line: Vec<u16> = pill.text.encode_utf16().collect();
            let box_ = D2D_RECT_F { left: l, top: t, right: rr, bottom: b - d };
            self.d2d.DrawText(&line, &format, &box_, &text, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            let ended = self.d2d.EndDraw(None, None);
            self.d2d.SetTarget(None::<&ID2D1Image>);
            ended.ctx("EndDraw (pill)")?;
            let mut srv = None;
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)).ctx("CreateShaderResourceView (pill)")?;
            Ok(srv.unwrap())
        }
    }

    fn srv(&self, pill: &Pill, w: u32, h: u32) -> Result<ID3D11ShaderResourceView, String> {
        let key = (pill.text.clone(), w, h);
        if !self.cache.borrow().contains_key(&key) {
            let baked = self.bake(pill, w, h)?;
            self.cache.borrow_mut().insert(key.clone(), baked);
        }
        Ok(self.cache.borrow()[&key].clone())
    }

    /// Draws `pills` (rectangles in target pixels) over `target`, which is `size` pixels.
    pub fn draw(
        &self,
        ctx: &ID3D11DeviceContext,
        target: &ID3D11RenderTargetView,
        size: (u32, u32),
        pills: &[Pill],
    ) -> Result<(), String> {
        if pills.is_empty() {
            return Ok(());
        }
        // Bake first: D2D draws through the immediate context and leaves its own state bound, so
        // every texture must exist before the quad pass sets the pipeline up.
        let mut quads: Vec<(ID3D11ShaderResourceView, Quad)> = Vec::with_capacity(pills.len());
        let (tw, th) = (size.0 as f32, size.1 as f32);
        let ndc_x = |x: f32| x / tw * 2.0 - 1.0;
        let ndc_y = |y: f32| 1.0 - y / th * 2.0;
        for pill in pills {
            let w = (pill.rect.max.x - pill.rect.min.x).round().max(1.0) as u32;
            let h = (pill.rect.max.y - pill.rect.min.y).round().max(1.0) as u32;
            let srv = self.srv(pill, w, h)?;
            // the quad covers the cap plus the shadow border the texture was baked with.
            let m = margin(pill.depth) as f32;
            let (x0, y0) = (pill.rect.min.x - m, pill.rect.min.y - m);
            let (x1, y1) = (x0 + w as f32 + 2.0 * m, y0 + h as f32 + 2.0 * m);
            let a = pill.alpha;
            quads.push((srv, Quad { rect: [ndc_x(x0), ndc_y(y0), ndc_x(x1), ndc_y(y1)], tint: [a, a, a, a] }));
        }
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: tw,
                Height: th,
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
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.OMSetBlendState(&self.blend, None, 0xFFFF_FFFF);
            for (srv, quad) in &quads {
                ctx.UpdateSubresource(&self.cb, 0, None, quad as *const Quad as _, 0, 0);
                ctx.PSSetShaderResources(0, Some(&[Some(srv.clone())]));
                ctx.Draw(4, 0);
            }
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
        }
        Ok(())
    }
}

fn color(c: [f32; 4]) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c[0], g: c[1], b: c[2], a: c[3] }
}

fn brush(rt: &ID2D1DeviceContext, c: [f32; 4]) -> Result<ID2D1SolidColorBrush, String> {
    unsafe { rt.CreateSolidColorBrush(&color(c), None).ctx("CreateSolidColorBrush") }
}

/// Texture border (px) around a cap of `depth`, so its drop shadow fits inside the baked texture.
fn margin(depth: f32) -> u32 {
    depth.ceil() as u32 + 2
}

/// The keycap stack for `keys` at time `t`, laid out in `region` with `r`'s text measurements.
pub fn pills(keys: &[Key], t: i64, style: &Style, region: (f32, f32), r: &Renderer) -> Vec<Pill> {
    state_at(keys, t, style, region, &|s| r.measure(s, style.font))
}

/// The reusable texture, its render target, a staging copy and the size they were made for.
type Frame = (ID3D11Texture2D, ID3D11RenderTargetView, ID3D11Texture2D, (u32, u32));

/// A standalone device that draws key pills onto CPU pixels, for the gif and software export
/// paths that have no render target of their own.
pub(crate) struct Surface {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    renderer: Renderer,
    frame: RefCell<Option<Frame>>,
}

impl Surface {
    pub(crate) fn new() -> Result<Surface, String> {
        let (device, ctx) = device()?;
        let renderer = Renderer::new(&device)?;
        Ok(Surface { device, ctx, renderer, frame: RefCell::new(None) })
    }

    pub(crate) fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    /// Composites `pills` over the `size` BGRA(X) pixels in `px` (`pitch` bytes per row).
    pub(crate) fn draw(&self, px: &mut [u8], pitch: usize, size: (u32, u32), pills: &[Pill]) -> Result<(), String> {
        if pills.is_empty() {
            return Ok(());
        }
        self.ensure(size)?;
        let frame = self.frame.borrow();
        let (tex, rtv, staging, _) = frame.as_ref().unwrap();
        unsafe {
            self.ctx.UpdateSubresource(tex, 0, None, px.as_ptr() as _, pitch as u32, 0);
            self.renderer.draw(&self.ctx, rtv, size, pills)?;
            self.ctx.CopyResource(staging, tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).ctx("Map (keys surface)")?;
            let row = size.0 as usize * 4;
            for y in 0..size.1 as usize {
                let src = (m.pData as *const u8).add(y * m.RowPitch as usize);
                let dst = px.as_mut_ptr().add(y * pitch);
                std::ptr::copy_nonoverlapping(src, dst, row);
            }
            self.ctx.Unmap(staging, 0);
        }
        Ok(())
    }

    fn ensure(&self, size: (u32, u32)) -> Result<(), String> {
        let mut frame = self.frame.borrow_mut();
        if frame.as_ref().map(|f| f.3) == Some(size) {
            return Ok(());
        }
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC {
                Width: size.0,
                Height: size.1,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            let mut tex = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut tex)).ctx("CreateTexture2D (surface)")?;
            let tex = tex.unwrap();
            let mut rtv = None;
            self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv)).ctx("CreateRenderTargetView (surface)")?;
            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            let mut staging = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut staging)).ctx("CreateTexture2D (staging)")?;
            *frame = Some((tex, rtv.unwrap(), staging.unwrap(), size));
        }
        Ok(())
    }
}

/// A BGRA-capable device for offscreen pill rendering: hardware, or WARP if there is no GPU.
fn device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut dev = None;
        let mut ctx = None;
        let r = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )
        };
        if r.is_ok() {
            return Ok((dev.unwrap(), ctx.unwrap()));
        }
    }
    Err("no d3d11 device for the keystroke overlay".into())
}

#[cfg(test)]
mod tests {
    use glint_core::record::keys::{Key, KeyLabel, Mods, Style};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

    use super::*;

    const SIZE: u32 = 256;
    const BACKGROUND: [u8; 4] = [40, 60, 80, 255];

    fn texture(device: &ID3D11Device, bind: D3D11_BIND_FLAG, staging: bool) -> ID3D11Texture2D {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: SIZE,
            Height: SIZE,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
            BindFlags: bind.0 as u32,
            CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
            MiscFlags: 0,
        };
        let mut t = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)).unwrap() };
        t.unwrap()
    }

    /// Composes a typed-word cap and a `Ctrl`+`s` chord row over BACKGROUND on the WARP device and
    /// checks the keycap look: pixels change only inside the caps (plus their shadow border), the
    /// frame's alpha survives, and each cap's dark face is darker than its light label ink.
    #[test]
    #[ignore = "renders on the WARP software device"]
    fn keycaps_draw_dark_faces_with_light_labels() {
        unsafe {
            let (mut device, mut ctx) = (None, None);
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .unwrap();
            let (device, ctx) = (device.unwrap(), ctx.unwrap());
            let target = texture(&device, D3D11_BIND_RENDER_TARGET, false);
            let mut rtv = None;
            device.CreateRenderTargetView(&target, None, Some(&mut rtv)).unwrap();
            let rtv = rtv.unwrap();
            let [b, g, r, a] = BACKGROUND.map(|c| c as f32 / 255.0);
            ctx.ClearRenderTargetView(&rtv, &[r, g, b, a]);

            let renderer = Renderer::new(&device).unwrap();
            let style = Style::new([230, 120, 40], 1.0);
            let ch = |at: i64, c: &str| Key { at, hold: Some(at + 20_000), mods: Mods::default(), main: KeyLabel::Text(c.into()) };
            let chord = Key {
                at: 50_000,
                hold: Some(70_000),
                mods: Mods { ctrl: true, ..Mods::default() },
                main: KeyLabel::Text("s".into()),
            };
            // "hi" batches into one wide text cap; the chord is a two-cap row (`Ctrl`, `s`).
            let keys = [ch(0, "h"), ch(10_000, "i"), chord];
            let shown = pills(&keys, 3_000_000, &style, (SIZE as f32, SIZE as f32), &renderer);
            let texts: Vec<&str> = shown.iter().map(|p| p.text.as_str()).collect();
            assert_eq!(texts, ["Ctrl", "s", "hi"], "two rows: the chord at the bottom, the word above");
            renderer.draw(&ctx, &rtv, (SIZE, SIZE), &shown).unwrap();

            let staging = texture(&device, D3D11_BIND_FLAG(0), true);
            ctx.CopyResource(&staging, &target);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let at = |x: usize, y: usize| {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), SIZE as usize * 4);
                let p = &row[x * 4..x * 4 + 4];
                [p[0], p[1], p[2], p[3]]
            };
            let lum = |p: [u8; 4]| 0.114 * p[0] as f32 + 0.587 * p[1] as f32 + 0.299 * p[2] as f32;

            // every changed pixel sits within some cap rect, grown by the shadow border it bakes.
            let pad = margin(style.depth) as f32 + 1.0;
            let mut changed = 0;
            for y in 0..SIZE as usize {
                for x in 0..SIZE as usize {
                    let p = at(x, y);
                    if p == BACKGROUND {
                        continue;
                    }
                    changed += 1;
                    assert_eq!(p[3], 255, "the frame's alpha is kept");
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    let inside = shown.iter().any(|q| {
                        fx >= q.rect.min.x - pad && fx <= q.rect.max.x + pad && fy >= q.rect.min.y - pad && fy <= q.rect.max.y + pad
                    });
                    assert!(inside, "pixel ({fx}, {fy}) changed to {p:?} outside every cap");
                }
            }
            assert!(changed > 400, "{changed} pixels changed");

            // each cap: a dark face dominating the area, with much lighter label ink on top.
            for q in &shown {
                let (mut peak, mut sum, mut n) = (0.0_f32, 0.0_f32, 0u32);
                for y in q.rect.min.y as usize..q.rect.max.y as usize {
                    for x in q.rect.min.x as usize..q.rect.max.x as usize {
                        let l = lum(at(x, y));
                        peak = peak.max(l);
                        sum += l;
                        n += 1;
                    }
                }
                let avg = sum / n as f32;
                assert!(avg < 120.0, "{:?} cap face not dark (avg lum {avg})", q.text);
                assert!(peak > 180.0, "{:?} cap has no light label ink (peak lum {peak})", q.text);
                assert!(peak > avg + 100.0, "{:?} label not clearly lighter than its face", q.text);
            }
            ctx.Unmap(&staging, 0);
        }
    }
}
