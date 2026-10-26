//! Thumbnails and previews on the GPU without holding a frame up. egui's own way (`load_texture`,
//! `TextureHandle::set`) makes a new texture for each picture and hands the driver all of its
//! pixels in one call, and NVIDIA's OpenGL driver waits on both until its queue has drained, the
//! last frame's present included: about 5 ms a picture, so a frame with thumbnails coming in missed
//! its refresh. Here textures are made in batches and reused once dropped, and pixels go up in
//! strips small enough for the driver to queue.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use eframe::egui::{Color32, ColorImage, TextureId};
use eframe::glow::{self, HasContext, PixelUnpackData};

/// Textures are made this many at a time: making one waits like a big upload does unless nothing
/// is queued yet (the first frame), so growing costs one wait per batch.
const BATCH: usize = 256;

/// The most bytes handed to the driver in one call: it copies this much and returns; a bigger
/// upload waits for its queue.
const STRIP: usize = 64 * 1024;

struct Slot {
    id: TextureId,
    native: glow::Texture,
    /// What its storage holds now, in pixels.
    size: [usize; 2],
}

/// Textures made and not in use.
type Spare = Rc<RefCell<Vec<Slot>>>;

pub(super) struct Textures {
    gl: Arc<glow::Context>,
    spare: Spare,
}

/// A picture on the GPU; dropped, its texture is kept for the next one.
pub(super) struct Texture {
    slot: Option<Slot>,
    spare: Spare,
}

impl Texture {
    pub fn id(&self) -> TextureId {
        self.slot.as_ref().map_or_else(TextureId::default, |s| s.id)
    }

    pub fn size(&self) -> [usize; 2] {
        self.slot.as_ref().map_or([0, 0], |s| s.size)
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            self.spare.borrow_mut().push(slot);
        }
    }
}

impl Textures {
    pub fn new(gl: Arc<glow::Context>) -> Textures {
        Textures { gl, spare: Rc::default() }
    }

    /// Makes sure `n` textures are ready. Called before the frame uploads anything: a texture made
    /// after an upload waits for the driver like a big upload does.
    pub fn reserve(&mut self, frame: &mut eframe::Frame, n: usize) {
        let have = self.spare.borrow().len();
        if have >= n {
            return;
        }
        for _ in 0..BATCH.max(n - have) {
            // SAFETY: the window's GL context is current while its frame is made
            let Ok(native) = (unsafe { self.gl.create_texture() }) else { return };
            let id = frame.register_native_glow_texture(native);
            self.spare.borrow_mut().push(Slot { id, native, size: [0, 0] });
        }
    }

    /// `image` in a spare texture; `None` when none is left this frame.
    pub fn upload(&self, image: &ColorImage, mipmaps: bool) -> Option<Texture> {
        let slot = self.spare.borrow_mut().pop()?;
        let mut tex = Texture { slot: Some(slot), spare: self.spare.clone() };
        self.replace(&mut tex, image, mipmaps);
        Some(tex)
    }

    /// Puts `image` in `tex` (a playing preview's next frame).
    pub fn replace(&self, tex: &mut Texture, image: &ColorImage, mipmaps: bool) {
        let Some(slot) = &mut tex.slot else { return };
        let [w, h] = image.size;
        if w == 0 || h == 0 {
            return;
        }
        // SAFETY: `Color32` is `#[repr(C)]` over `[u8; 4]`, so the pixels are `4 * len` bytes
        let bytes = unsafe { std::slice::from_raw_parts(image.pixels.as_ptr().cast::<u8>(), size_of::<Color32>() * image.pixels.len()) };
        let rows = (STRIP / (w * 4)).max(1);
        let gl = &self.gl;
        // SAFETY: the window's GL context is current while its frame is made; every strip lies
        // inside the storage made for `image.size`
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(slot.native));
            if slot.size != image.size {
                let (w, h) = (w as i32, h as i32);
                gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, PixelUnpackData::Slice(None));
                slot.size = image.size;
            }
            let min = if mipmaps { glow::LINEAR_MIPMAP_LINEAR } else { glow::LINEAR };
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, min as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            for y in (0..h).step_by(rows) {
                let n = rows.min(h - y);
                let strip = PixelUnpackData::Slice(Some(&bytes[y * w * 4..(y + n) * w * 4]));
                gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, y as i32, w as i32, n as i32, glow::RGBA, glow::UNSIGNED_BYTE, strip);
            }
            if mipmaps {
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }
}
