//! Draws click highlights ([`Shape`]s from [`glint_core::record::clicks::state_at`]) into a
//! D3D11 render target: one instanced quad per shape, the ring's coverage worked out per pixel.

use glint_core::record::clicks::Shape;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::core::s;

use crate::record::Ctx;
use crate::record::cursor::{blob_bytes, compile};

const BATCH: usize = 64;

const SHADER: &str = r#"
cbuffer C : register(b0) { float4 view; float4 shapes[128]; };
struct V {
    float4 pos : SV_Position;
    float2 local : TEXCOORD0;
    nointerpolation float2 ring : TEXCOORD1;
    nointerpolation float4 color : COLOR0;
};
V vs_main(uint id : SV_VertexID, uint inst : SV_InstanceID) {
    float4 g = shapes[inst * 2];
    float extent = g.z + g.w * 0.5 + 1.0;
    float2 corner = float2(id & 1, id >> 1) * 2.0 - 1.0;
    float2 p = g.xy + corner * extent;
    V o;
    o.pos = float4(p.x * view.x - 1.0, 1.0 - p.y * view.y, 0, 1);
    o.local = corner * extent;
    o.ring = g.zw;
    o.color = shapes[inst * 2 + 1];
    return o;
}
float4 ps_main(V i) : SV_Target {
    float cover = saturate(i.ring.y * 0.5 - abs(length(i.local) - i.ring.x) + 0.5);
    float a = i.color.a * cover;
    return float4(i.color.rgb * a, a);
}
"#;

#[repr(C)]
struct Constants {
    view: [f32; 4],
    shapes: [[f32; 4]; 2 * BATCH],
}

pub struct Renderer {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
}

impl Renderer {
    pub fn new(device: &ID3D11Device) -> Result<Renderer, String> {
        unsafe {
            let vsb = compile(SHADER, s!("vs_main"), s!("vs_4_0")).ctx("click vertex shader")?;
            let psb = compile(SHADER, s!("ps_main"), s!("ps_4_0")).ctx("click pixel shader")?;
            let mut vs = None;
            device.CreateVertexShader(blob_bytes(&vsb), None, Some(&mut vs)).ctx("CreateVertexShader")?;
            let mut ps = None;
            device.CreatePixelShader(blob_bytes(&psb), None, Some(&mut ps)).ctx("CreatePixelShader")?;
            let mut cb = None;
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<Constants>() as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            device.CreateBuffer(&desc, None, Some(&mut cb)).ctx("CreateBuffer")?;
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
            let rd = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            };
            device.CreateRasterizerState(&rd, Some(&mut raster)).ctx("CreateRasterizerState")?;
            Ok(Renderer { vs: vs.unwrap(), ps: ps.unwrap(), cb: cb.unwrap(), blend: bs.unwrap(), raster: raster.unwrap() })
        }
    }

    /// Draws `shapes` (positions in target pixels) over `target`, which is `size` pixels.
    pub fn draw(
        &self,
        ctx: &ID3D11DeviceContext,
        target: &ID3D11RenderTargetView,
        size: (u32, u32),
        shapes: &[Shape],
    ) -> Result<(), String> {
        if shapes.is_empty() {
            return Ok(());
        }
        let (w, h) = (size.0 as f32, size.1 as f32);
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(target.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w,
                Height: h,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.OMSetBlendState(&self.blend, None, 0xFFFF_FFFF);
            let mut drawn = Ok(());
            for batch in shapes.chunks(BATCH) {
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                drawn = ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).ctx("Map (clicks)");
                if drawn.is_err() {
                    break;
                }
                let c = mapped.pData as *mut Constants;
                (*c).view = [2.0 / w, 2.0 / h, 0.0, 0.0];
                for (i, s) in batch.iter().enumerate() {
                    (*c).shapes[2 * i] = [s.x, s.y, s.radius, s.width];
                    (*c).shapes[2 * i + 1] = s.color;
                }
                ctx.Unmap(&self.cb, 0);
                ctx.DrawInstanced(4, batch.len() as u32, 0, 0);
            }
            ctx.OMSetRenderTargets(None, None);
            ctx.OMSetBlendState(None, None, 0xFFFF_FFFF);
            drawn
        }
    }
}

#[cfg(test)]
mod tests {
    use glint_core::record::clicks::{Button, Click, Style, state_at};
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_WARP;
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::IDXGIAdapter;

    use super::*;

    const SIZE: u32 = 128;
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

    /// BGRA pixels of a SIZE² target after drawing `shapes` over BACKGROUND (WARP, no GPU needed).
    fn render(shapes: &[Shape]) -> Vec<[u8; 4]> {
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
            Renderer::new(&device).unwrap().draw(&ctx, &rtv, (SIZE, SIZE), shapes).unwrap();
            let staging = texture(&device, D3D11_BIND_FLAG(0), true);
            ctx.CopyResource(&staging, &target);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let mut px = Vec::with_capacity((SIZE * SIZE) as usize);
            for y in 0..SIZE as usize {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), SIZE as usize * 4);
                px.extend(row.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]));
            }
            ctx.Unmap(&staging, 0);
            px
        }
    }

    #[test]
    #[ignore = "renders on the WARP software device"]
    fn a_ripple_changes_pixels_only_around_the_click() {
        let style = Style::new([255, 0, 0], 1.0);
        let click = Click { button: Button::Left, down: 0, up: Some(1_000_000), x: 64.5, y: 64.5 };
        let shapes = state_at(&[click], 2_250_000, &style);
        assert_eq!(shapes.len(), 1);
        let ring = shapes[0];
        let px = render(&shapes);
        let mut changed = 0;
        for (i, p) in px.iter().enumerate() {
            let (x, y) = ((i as u32 % SIZE) as f32 + 0.5, (i as u32 / SIZE) as f32 + 0.5);
            let d = ((x - 64.5).powi(2) + (y - 64.5).powi(2)).sqrt();
            let off = (d - ring.radius).abs();
            if *p != BACKGROUND {
                changed += 1;
                assert!(off < ring.width / 2.0 + 1.0, "pixel ({x}, {y}) {d:.2} from the click changed to {p:?}");
                assert_eq!(p[3], 255, "alpha kept");
            }
            if off < 0.5 {
                assert!(p[2] > BACKGROUND[2] + 60 && p[0] < BACKGROUND[0], "ring pixel ({x}, {y}) is {p:?}");
            }
        }
        assert!(changed > 300, "{changed} pixels changed");
        assert_eq!(px[(64 * SIZE + 64) as usize], BACKGROUND);
        assert_eq!(render(&[]), vec![BACKGROUND; (SIZE * SIZE) as usize]);
    }
}
