//! Pictures from the clipboard: Ctrl+V pastes the one on it (or its text), and the clipboard
//! tool's strip offers the recent ones, from the clipboard history (Win+V) and glint's own
//! screenshots, to click or drag in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use glint_core::geom::rect::Rect;
use glint_shell::gfx::{Canvas, rgba};
use glint_shell::paste::Image;

use super::marks::{MIN_SIDE, Mark, Picture, Tool};
use super::text::Style;
use super::textbar::{PANEL_MS, faded, progress};
use super::toolbar::{BAR_BG, Item, nearest};
use super::{Drag, Editor, Event};

/// Pictures the strip shows from the clipboard history, and from the screenshots.
const FROM_HISTORY: usize = 8;
const FROM_SHOTS: usize = 6;
/// The largest a thumbnail gets, in pixels.
const THUMB: (u32, u32) = (176, 112);

/// A picture the strip offers.
#[derive(Clone)]
pub(crate) struct Recent {
    pub(crate) thumb: Arc<Image>,
    pub(crate) full: Full,
    /// One of glint's screenshots, rather than from the clipboard history.
    pub(crate) shot: bool,
}

/// Where a recent picture's full size comes from, read when it's pasted.
#[derive(Clone)]
pub(crate) enum Full {
    Bytes(Arc<Vec<u8>>),
    File(PathBuf),
    #[cfg_attr(not(test), allow(dead_code))]
    Ready(Arc<Image>),
}

impl Recent {
    fn image(&self) -> Option<Arc<Image>> {
        match &self.full {
            Full::Bytes(b) => Image::decode(b).map(Arc::new),
            Full::File(p) => Image::decode(&std::fs::read(p).ok()?).map(Arc::new),
            Full::Ready(img) => Some(img.clone()),
        }
    }
}

/// How far the strip's pictures have loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Load {
    Idle,
    /// The strip opened: the picker starts loading them.
    Wanted,
    Loading,
    Done,
}

/// `img` shrunk to fit `max` (never grown), each pixel the average of the ones it covers.
pub(crate) fn thumbnail(img: &Image, max: (u32, u32)) -> Image {
    let k = (max.0 as f32 / img.w as f32).min(max.1 as f32 / img.h as f32).min(1.0);
    let side = |v: u32| ((v as f32 * k).round() as u32).clamp(1, v);
    shrunk(img, side(img.w), side(img.h))
}

/// `img` shrunk to `w`×`h` (each no bigger than it is), each pixel the average of the ones it covers.
pub(super) fn shrunk(img: &Image, w: u32, h: u32) -> Image {
    let (w, h) = (w.clamp(1, img.w), h.clamp(1, img.h));
    let mut px = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let (y0, y1) = (y * img.h / h, ((y + 1) * img.h / h).max(y * img.h / h + 1));
        for x in 0..w {
            let (x0, x1) = (x * img.w / w, ((x + 1) * img.w / w).max(x * img.w / w + 1));
            let mut sum = [0u64; 4];
            for row in y0..y1 {
                for &p in &img.px[(row * img.w + x0) as usize..(row * img.w + x1) as usize] {
                    for (k, s) in sum.iter_mut().enumerate() {
                        *s += ((p >> (k * 8)) & 0xFF) as u64;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            px.push(sum.iter().enumerate().fold(0u32, |acc, (k, s)| acc | (((s / n) as u32) << (k * 8))));
        }
    }
    Image { w, h, px }
}

fn is_picture(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["png", "jpg", "jpeg", "bmp", "gif", "webp"].contains(&e.to_ascii_lowercase().as_str()))
}

/// Reads the strip's pictures: the clipboard history's, then the latest screenshots', calling
/// `each` with the list so far after every one (the same picture only once). Blocks: run it
/// off the UI thread, in a COM apartment.
pub(crate) fn load_recents(mut each: impl FnMut(&[Recent])) {
    let mut out: Vec<Recent> = Vec::new();
    let mut add = |out: &mut Vec<Recent>, img: Image, full: Full, shot: bool| {
        let thumb = thumbnail(&img, THUMB);
        if out.iter().all(|r| *r.thumb != thumb) {
            out.push(Recent { thumb: Arc::new(thumb), full, shot });
            each(out);
        }
    };
    for bytes in glint_shell::paste::history_images(FROM_HISTORY) {
        if let Some(img) = Image::decode(&bytes) {
            add(&mut out, img, Full::Bytes(Arc::new(bytes)), false);
        }
    }
    let shots = glint_core::history::recent(64).into_iter().filter(|e| e.kind == glint_core::history::Kind::Shot);
    let paths = shots.filter_map(|e| e.path).filter(|p| is_picture(p)).take(FROM_SHOTS);
    for path in paths {
        if let Some(img) = std::fs::read(&path).ok().and_then(|b| Image::decode(&b)) {
            add(&mut out, img, Full::File(path), true);
        }
    }
}

/// Where the editor gets what Ctrl+V pastes and puts what Ctrl+C copies from a text: the system
/// clipboard, or a stand-in for tests.
pub(crate) trait Clip {
    fn image(&self) -> Option<Image>;
    fn text(&self) -> Option<String>;
    fn set_text(&self, text: &str);
}

/// The Windows clipboard.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct System;

/// What a new editor pastes from: the Windows clipboard, and in tests an empty stand-in, so no
/// test ever reads or changes the user's clipboard.
#[cfg(not(test))]
pub(super) fn default_clip() -> Box<dyn Clip> {
    Box::new(System)
}

#[cfg(test)]
pub(super) fn default_clip() -> Box<dyn Clip> {
    Box::new(std::rc::Rc::new(tests::Fake::default()))
}

impl Clip for System {
    fn image(&self) -> Option<Image> {
        glint_shell::paste::read_image()
    }

    fn text(&self) -> Option<String> {
        glint_shell::paste::read_text().filter(|t| !t.is_empty())
    }

    fn set_text(&self, text: &str) {
        if let Err(e) = glint_shell::clipboard::copy_text(None, text) {
            glint_core::log::log(&format!("copying text: {e}"));
        }
    }
}

/// `(w, h)` shrunk (never grown) to fit inside `room`, keeping its proportions.
pub(super) fn fit((w, h): (u32, u32), room: (i32, i32)) -> (i32, i32) {
    let k = (room.0 as f32 / w.max(1) as f32).min(room.1 as f32 / h.max(1) as f32).min(1.0);
    let side = |v: u32| ((v as f32 * k).round() as i32).max(MIN_SIDE);
    (side(w), side(h))
}

impl Editor {
    /// Ctrl+V outside a text: the clipboard's picture as a new mark at the mouse, or its text as a
    /// new text there.
    pub(super) fn paste(&mut self) -> Event {
        self.commit();
        if let Some(image) = self.clip.image() {
            return self.paste_image(Arc::new(image), self.mouse);
        }
        match self.clip.text() {
            Some(text) => self.paste_text(&text),
            None => {
                self.notice = Some(("nothing to paste".into(), std::time::Instant::now()));
                Event::Repaint
            }
        }
    }

    /// Where a `w`×`h` picture pasted at `at` lands: centred on it, shrunk to fit its monitor and
    /// kept on it.
    pub(super) fn place_image(&self, (w, h): (u32, u32), at: (i32, i32)) -> Rect {
        let m = nearest(&self.monitors, at);
        let (w, h) = fit((w, h), (m.w, m.h));
        let x = (at.0 - w / 2).clamp(m.x, (m.right() - w).max(m.x));
        let y = (at.1 - h / 2).clamp(m.y, (m.bottom() - h).max(m.y));
        Rect { x, y, w, h }
    }

    /// `image` as a new picked mark centred on `at`: one undo step, ready to move and resize.
    pub(super) fn paste_image(&mut self, image: Arc<Image>, at: (i32, i32)) -> Event {
        self.commit();
        let rect = self.place_image((image.w, image.h), at);
        self.remember();
        self.marks.push(Mark::Image { rect, picture: Picture::new(image) });
        self.picked = Some(self.marks.len() - 1);
        self.tool = Some(Tool::Image);
        self.pick = None;
        self.repaint_ink(vec![rect]);
        self.marks_rev += 1;
        Event::Repaint
    }

    /// Pasted text as a new picked text mark at the mouse, in the last style used.
    fn paste_text(&mut self, text: &str) -> Event {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.tool = Some(Tool::Text);
        self.start_typing(self.mouse);
        self.insert(&text);
        let n = self.marks.len();
        self.commit();
        if self.marks.len() == n {
            self.picked = Some(n - 1);
        }
        Event::Repaint
    }

    /// The clipboard tool's button: opens the strip, or with it open puts the tool away.
    pub(super) fn clipboard_tool(&mut self) {
        if self.tool == Some(Tool::Image) && self.strip_open {
            self.tool = None;
            self.strip_open = false;
            return;
        }
        self.tool = Some(Tool::Image);
        self.strip_open = true;
        if self.recents_load == Load::Idle {
            self.recents_load = Load::Wanted;
        }
    }

    /// Whether the strip just asked for its pictures; the picker then loads them and hands them
    /// over with [`Editor::set_recents`].
    pub(crate) fn take_recents_request(&mut self) -> bool {
        if self.recents_load != Load::Wanted {
            return false;
        }
        self.recents_load = Load::Loading;
        true
    }

    /// The strip's pictures so far; `done` once they're all there. New ones fade in.
    pub(crate) fn set_recents(&mut self, list: Vec<Recent>, done: bool) -> Event {
        let now = Instant::now();
        let kept = self.recents.len().min(list.len());
        let arrived: Vec<Instant> = self.recents.iter().take(kept).map(|(_, at)| *at).collect();
        self.recents = list.into_iter().enumerate().map(|(i, r)| (r, arrived.get(i).copied().unwrap_or(now))).collect();
        if done {
            self.recents_load = Load::Done;
        }
        Event::Repaint
    }

    /// The strip: a cell per picture (the clipboard's, then a gap, then the screenshots'), as
    /// many as fit on the monitor.
    pub(super) fn strip_layout(&self) -> (Rect, Vec<(Item, Rect)>) {
        let (cw, ch) = (self.px(84.0) as i32, self.px(56.0) as i32);
        let (pad, gap, split) = (self.px(6.0) as i32, self.px(5.0) as i32, self.px(12.0) as i32);
        let room = self.bar_monitor.w - 2 * pad;
        let mut x = 0;
        let mut cells = Vec::new();
        let mut last_shot = None;
        for (i, (r, _)) in self.recents.iter().enumerate() {
            if i > 0 {
                x += if last_shot != Some(r.shot) { split } else { gap };
            }
            if x + cw > room {
                break;
            }
            last_shot = Some(r.shot);
            cells.push((Item::Recent(i), Rect { x, y: 0, w: cw, h: ch }));
            x += cw;
        }
        // nothing yet: room for the placeholders or the note
        let w = if cells.is_empty() { self.px(200.0) as i32 } else { x } + 2 * pad;
        let rect = self.place_panel((w, ch + 2 * pad));
        for (_, c) in &mut cells {
            *c = c.offset(rect.x + pad, rect.y + pad);
        }
        (rect, cells)
    }

    /// Pastes the strip's picture `i`, centred on `at` or else in the middle of the shot.
    pub(super) fn paste_recent(&mut self, i: usize, at: Option<(i32, i32)>) -> Event {
        let Some(img) = self.recents.get(i).and_then(|(r, _)| r.image()) else { return Event::None };
        let s = self.output_rect();
        self.paste_image(img, at.unwrap_or((s.x + s.w / 2, s.y + s.h / 2)))
    }

    /// A press on the strip's picture `index` moved to `p`: once it leaves the strip, the picture
    /// is pasted under the mouse and follows it.
    pub(super) fn drag_recent(&mut self, index: usize, press: (i32, i32), p: (i32, i32)) -> Event {
        let slop = self.px(4.0) as i32;
        let moved = (p.0 - press.0).abs() > slop || (p.1 - press.1).abs() > slop;
        let on_strip = self.panel_layout().is_some_and(|(_, r, _)| r.contains(p.0, p.1));
        if !moved || on_strip {
            return Event::None;
        }
        let event = self.paste_recent(index, Some(p));
        if let Some(i) = self.picked.filter(|_| matches!(event, Event::Repaint)) {
            let from = self.marks[i].clone();
            self.drag = Some(Drag::Edit { index: i, grip: None, grab: p, from, retype: false, fresh: true });
        }
        event
    }

    /// The strip, drawn onto `c` (which shows its box `rect`).
    pub(super) fn draw_strip(&self, c: &Canvas, rect: Rect, cells: &[(Item, Rect)]) {
        let (s, t) = (self.scale, self.panel_t());
        let slide = (1.0 - t) * self.px(6.0) * if rect.y >= self.bar.y { -1.0 } else { 1.0 };
        c.round_rect(0.0, slide, rect.w as f32, rect.h as f32, 10.0 * s, faded(BAR_BG, t));
        let pad = self.px(6.0);
        if cells.is_empty() {
            let ui = Style { bold: false, ..Style::default() };
            let inner = [pad + 6.0 * s, pad + slide, rect.w as f32 - 2.0 * pad, rect.h as f32 - 2.0 * pad];
            match self.recents_load {
                Load::Done => self.fonts.draw_line(c, "nothing copied yet", &ui, 13.0 * s, inner, rgba(255, 255, 255, 0.5 * t)),
                _ => {
                    let w = (inner[2] - 2.0 * self.px(5.0)) / 3.0;
                    for i in 0..3 {
                        let x = pad + i as f32 * (w + self.px(5.0));
                        c.round_rect(x, pad + slide, w, inner[3], 6.0 * s, rgba(255, 255, 255, 0.06 * t));
                    }
                }
            }
            return;
        }
        for &(item, r) in cells {
            let Item::Recent(i) = item else { continue };
            let Some((recent, arrived)) = self.recents.get(i) else { continue };
            let a = t * progress(*arrived, PANEL_MS);
            let (x, y, w, h) = ((r.x - rect.x) as f32, (r.y - rect.y) as f32 + slide, r.w as f32, r.h as f32);
            let lit = if self.pressed() == Some(item) { 0.24 } else if self.hover == Some(item) { 0.16 } else { 0.06 };
            c.round_rect(x, y, w, h, 6.0 * s, rgba(255, 255, 255, lit * t));
            let img = &recent.thumb;
            let inset = 3.0 * s;
            let k = ((w - 2.0 * inset) / img.w as f32).min((h - 2.0 * inset) / img.h as f32);
            let (iw, ih) = (img.w as f32 * k, img.h as f32 * k);
            let at = [x + (w - iw) / 2.0, y + (h - ih) / 2.0, iw, ih];
            c.image_faded(&img.px, img.w as usize, (img.w, img.h), at, a);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A clipboard holding whatever the test puts on it.
    #[derive(Default)]
    pub(crate) struct Fake {
        pub(crate) image: RefCell<Option<Image>>,
        pub(crate) text: RefCell<Option<String>>,
    }

    impl Clip for std::rc::Rc<Fake> {
        fn image(&self) -> Option<Image> {
            self.image.borrow().clone()
        }

        fn text(&self) -> Option<String> {
            self.text.borrow().clone()
        }

        fn set_text(&self, text: &str) {
            *self.text.borrow_mut() = Some(text.to_owned());
        }
    }

    #[test]
    fn pictures_shrink_to_fit_but_never_grow() {
        assert_eq!(fit((400, 200), (1920, 1080)), (400, 200));
        assert_eq!(fit((3840, 2160), (1920, 1080)), (1920, 1080));
        assert_eq!(fit((4000, 1000), (1000, 1000)), (1000, 250));
        assert_eq!(fit((1, 5000), (1000, 1000)), (MIN_SIDE, 1000));
    }
}
