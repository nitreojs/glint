//! Rotated outputs: Desktop Duplication hands out their surface in the panel's native
//! orientation, so their part of the region is drawn (rotated, 1:1 texels) instead of copied.

use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::core::s;

use super::Ctx;
use super::cursor::{blob_bytes, compile};
use crate::platform::rect::Rect;

const SHADER: &str = r#"
cbuffer C : register(b0) { int4 m; int4 o; };
Texture2D<float4> tex : register(t0);
float4 vs_main(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p.x * 2 - 1, 1 - p.y * 2, 0, 1);
}
float4 ps_main(float4 pos : SV_Position) : SV_Target {
    int2 q = int2(pos.xy) - o.zw;
    return tex.Load(int3(o.x + m.x * q.x + m.y * q.y, o.y + m.z * q.x + m.w * q.y, 0));
}
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rotation {
    Identity,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub(super) fn from_dxgi(r: DXGI_MODE_ROTATION) -> Rotation {
        match r {
            DXGI_MODE_ROTATION_ROTATE90 => Rotation::R90,
            DXGI_MODE_ROTATION_ROTATE180 => Rotation::R180,
            DXGI_MODE_ROTATION_ROTATE270 => Rotation::R270,
            _ => Rotation::Identity,
        }
    }
}

/// Where `part` (output pixels, desktop orientation) lies in the duplicated surface of an
/// output whose desktop is `w`x`h`. Move/dirty rects are reported in surface coordinates too.
pub(super) fn surface_rect(rot: Rotation, part: Rect, (w, h): (i32, i32)) -> Rect {
    match rot {
        Rotation::Identity => part,
        Rotation::R90 => Rect { x: part.y, y: w - part.right(), w: part.h, h: part.w },
        Rotation::R180 => Rect { x: w - part.right(), y: h - part.bottom(), w: part.w, h: part.h },
        Rotation::R270 => Rect { x: h - part.bottom(), y: part.x, w: part.h, h: part.w },
    }
}

/// Surface texel for pixel (u, v) of `part`: `(t[4] + t[0]*u + t[1]*v, t[5] + t[2]*u + t[3]*v)`.
pub(super) fn texel_map(rot: Rotation, part: Rect, (w, h): (i32, i32)) -> [i32; 6] {
    match rot {
        Rotation::Identity => [1, 0, 0, 1, part.x, part.y],
        Rotation::R90 => [0, 1, -1, 0, part.y, w - 1 - part.x],
        Rotation::R180 => [-1, 0, 0, -1, w - 1 - part.x, h - 1 - part.y],
        Rotation::R270 => [0, -1, 1, 0, h - 1 - part.y, part.x],
    }
}

pub(super) struct Rotator {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    raster: ID3D11RasterizerState,
    ring: Vec<ID3D11RenderTargetView>,
    compose: Option<ID3D11RenderTargetView>,
}

impl Rotator {
    pub(super) unsafe fn new(
        dev: &ID3D11Device,
        ring: &[ID3D11Texture2D],
        compose: Option<&ID3D11Texture2D>,
    ) -> Result<Rotator, String> {
        unsafe {
            let vsb = compile(SHADER, s!("vs_main"), s!("vs_4_0")).ctx("rotation vertex shader")?;
            let psb = compile(SHADER, s!("ps_main"), s!("ps_4_0")).ctx("rotation pixel shader")?;
            let mut vs = None;
            dev.CreateVertexShader(blob_bytes(&vsb), None, Some(&mut vs)).ctx("CreateVertexShader")?;
            let mut ps = None;
            dev.CreatePixelShader(blob_bytes(&psb), None, Some(&mut ps)).ctx("CreatePixelShader")?;
            let mut cb = None;
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: 32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };
            dev.CreateBuffer(&desc, None, Some(&mut cb)).ctx("CreateBuffer")?;
            let mut raster = None;
            let desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            };
            dev.CreateRasterizerState(&desc, Some(&mut raster)).ctx("CreateRasterizerState")?;
            let rtv = |t: &ID3D11Texture2D| {
                let mut v = None;
                dev.CreateRenderTargetView(t, None, Some(&mut v)).ctx("CreateRenderTargetView").map(|()| v.unwrap())
            };
            Ok(Rotator {
                vs: vs.unwrap(),
                ps: ps.unwrap(),
                cb: cb.unwrap(),
                raster: raster.unwrap(),
                ring: ring.iter().map(rtv).collect::<Result<_, _>>()?,
                compose: compose.map(rtv).transpose()?,
            })
        }
    }

    /// Draws surface `src` into `viewport` of ring texture `slot` (the composed image when
    /// `None`), viewport pixels picking texels per [`texel_map`] output `map`.
    pub(super) unsafe fn draw(
        &self,
        dev: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        slot: Option<usize>,
        src: &ID3D11Texture2D,
        map: [i32; 6],
        viewport: Rect,
    ) -> Result<(), String> {
        unsafe {
            let rtv = match slot {
                Some(i) => &self.ring[i],
                None => self.compose.as_ref().ok_or_else(|| "no composed image to draw into".to_string())?,
            };
            let mut srv = None;
            dev.CreateShaderResourceView(src, None, Some(&mut srv)).ctx("CreateShaderResourceView")?;
            let cb = [map[0], map[1], map[2], map[3], map[4], map[5], viewport.x, viewport.y];
            ctx.UpdateSubresource(&self.cb, 0, None, cb.as_ptr() as _, 0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: viewport.x as f32,
                TopLeftY: viewport.y as f32,
                Width: viewport.w as f32,
                Height: viewport.h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShaderResources(0, Some(&[srv]));
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Rotation; 4] = [Rotation::Identity, Rotation::R90, Rotation::R180, Rotation::R270];

    fn texel(rot: Rotation, part: Rect, size: (i32, i32), u: i32, v: i32) -> (i32, i32) {
        let t = texel_map(rot, part, size);
        (t[4] + t[0] * u + t[1] * v, t[5] + t[2] * u + t[3] * v)
    }

    #[test]
    fn surface_rects_match_the_dxgi_sample() {
        // mappings from the dxgidesktopduplication sample, DisplayManager::SetDirtyVert (surface -> desktop)
        let (w, h) = (1440, 2560);
        let d = Rect::from_points(100, 300, 400, 700);
        let desk90 = Rect::from_points(w - d.bottom(), d.x, w - d.y, d.right());
        let desk270 = Rect::from_points(d.y, h - d.right(), d.bottom(), h - d.x);
        assert_eq!(surface_rect(Rotation::R90, desk90, (w, h)), d);
        assert_eq!(surface_rect(Rotation::R270, desk270, (w, h)), d);
        let (w, h) = (2560, 1440);
        let desk180 = Rect::from_points(w - d.right(), h - d.bottom(), w - d.x, h - d.y);
        assert_eq!(surface_rect(Rotation::R180, desk180, (w, h)), d);
        assert_eq!(surface_rect(Rotation::Identity, d, (w, h)), d);
    }

    #[test]
    fn texels_cover_the_surface_rect_exactly_once() {
        let (w, h) = (24, 16);
        for rot in ALL {
            let size = if matches!(rot, Rotation::R90 | Rotation::R270) { (h, w) } else { (w, h) };
            let part = Rect { x: 3, y: 5, w: 7, h: 4 };
            let s = surface_rect(rot, part, size);
            let mut seen = std::collections::HashSet::new();
            for v in 0..part.h {
                for u in 0..part.w {
                    let t = texel(rot, part, size, u, v);
                    assert!(s.contains(t.0, t.1), "{rot:?} ({u},{v}) -> {t:?} outside {s:?}");
                    assert!(seen.insert(t), "{rot:?} texel {t:?} used twice");
                }
            }
            assert_eq!(seen.len() as i32, s.w * s.h);
        }
    }

    #[test]
    fn corners_rotate_the_right_way() {
        // portrait desktop on a landscape panel: its top-left is the surface's bottom-left (90), top-right (270)
        let part = Rect { x: 0, y: 0, w: 1440, h: 2560 };
        assert_eq!(texel(Rotation::R90, part, (1440, 2560), 0, 0), (0, 1439));
        assert_eq!(texel(Rotation::R270, part, (1440, 2560), 0, 0), (2559, 0));
        assert_eq!(texel(Rotation::R180, Rect { x: 0, y: 0, w: 2560, h: 1440 }, (2560, 1440), 0, 0), (2559, 1439));
        assert_eq!(texel(Rotation::Identity, part, (1440, 2560), 5, 6), (5, 6));
    }
}
