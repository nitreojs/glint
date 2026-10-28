//! Pasting pictures, the clipboard strip, and the text tool: headless, driving the editor like
//! the picker does. `review_shots` (ignored) renders what they look like to PNGs.

use std::rc::Rc;
use std::sync::Arc;

use glint_core::config::toolbar::KeyKind;
use glint_shell::gfx::Painter;
use glint_shell::paste::Image;

use super::paste::tests::Fake;
use super::paste::{Full, Recent};
use super::text::{Align, Back, VAlign};
use super::*;

const VK_BACK: u32 = 0x08;
const VK_LEFT: u32 = 0x25;
const VK_HOME: u32 = 0x24;
const VK_END: u32 = 0x23;
const VK_DELETE: u32 = 0x2E;
const VK_ESCAPE: u32 = 0x1B;

fn editor() -> (Editor, Rc<Fake>) {
    let mut ed = Editor::new(Rect { x: 0, y: 0, w: 400, h: 300 }, None, vec![Rect { x: 0, y: 0, w: 1920, h: 1080 }], 1.0);
    let clip = Rc::new(Fake::default());
    ed.clip = Box::new(clip.clone());
    (ed, clip)
}

fn picture(w: u32, h: u32) -> Image {
    let px = (0..w * h).map(|i| 0xFF00_0000 | (i * 2_654_435_761) >> 8).collect();
    Image { w, h, px }
}

/// Held by tests that change what editors remember between shots (the last text style, the
/// custom colours), so they don't see each other's.
static MEMORY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn memory() -> std::sync::MutexGuard<'static, ()> {
    MEMORY.lock().unwrap_or_else(|e| e.into_inner())
}

fn drag(ed: &mut Editor, from: (i32, i32), to: (i32, i32)) {
    ed.mouse_down(from, 0);
    ed.mouse_move(to);
    ed.mouse_up(to);
    ed.last_click = None;
}

fn click(ed: &mut Editor, p: (i32, i32)) {
    ed.mouse_down(p, 0);
    ed.mouse_up(p);
    ed.last_click = None;
}

fn type_text(ed: &mut Editor, text: &str) {
    for u in text.encode_utf16() {
        ed.char(u);
    }
}

fn image_rect(ed: &Editor, i: usize) -> Rect {
    match &ed.marks[i] {
        Mark::Image { rect, .. } => *rect,
        m => panic!("not a picture: {m:?}"),
    }
}

fn text_of(ed: &Editor, i: usize) -> &str {
    match &ed.marks[i] {
        Mark::Text { text, .. } => text,
        m => panic!("not a text: {m:?}"),
    }
}

fn style_of(ed: &Editor, i: usize) -> Style {
    match &ed.marks[i] {
        Mark::Text { style, .. } => style.clone(),
        m => panic!("not a text: {m:?}"),
    }
}

#[test]
fn ctrl_v_pastes_a_picture_that_fits_moves_resizes_and_undoes() {
    let (mut ed, clip) = editor();
    ed.mouse_move((200, 150));
    assert!(matches!(ed.key(0x56, true, false), Event::Repaint));
    assert!(ed.marks.is_empty(), "nothing on the clipboard, nothing pasted");
    assert!(ed.notice.is_some(), "but it says so");

    *clip.image.borrow_mut() = Some(picture(3000, 500));
    ed.key(0x56, true, false);
    assert_eq!(image_rect(&ed, 0), Rect { x: 0, y: 0, w: 1920, h: 320 }, "shrunk to the monitor, kept on it");
    assert_eq!((ed.picked, ed.tool), (Some(0), Some(Tool::Image)), "picked, with the clipboard tool");

    *clip.image.borrow_mut() = Some(picture(200, 100));
    ed.key(0x56, true, false);
    assert_eq!(image_rect(&ed, 1), Rect { x: 100, y: 100, w: 200, h: 100 }, "centred on the mouse, full size");

    drag(&mut ed, (150, 120), (180, 140));
    assert_eq!(image_rect(&ed, 1), Rect { x: 130, y: 120, w: 200, h: 100 }, "it moves");
    drag(&mut ed, (330, 220), (370, 230));
    assert_eq!(image_rect(&ed, 1), Rect { x: 130, y: 120, w: 240, h: 110 }, "a corner resizes freely");
    ed.set_mods(false, false, true);
    drag(&mut ed, (370, 230), (410, 230));
    assert_eq!(image_rect(&ed, 1), Rect { x: 130, y: 120, w: 280, h: 140 }, "Shift keeps its proportions");
    ed.set_mods(false, false, false);

    let painter = Painter::new().unwrap();
    let mut bright = Dib::new(1920, 1080).unwrap();
    bright.pixels_mut().iter_mut().for_each(|p| *p = 0xFF20_2020);
    let (px, out, _) = ed.pixels(&bright, &painter).unwrap();
    let at = |x: i32, y: i32| px[(y * out.w + x) as usize] & 0x00FF_FFFF;
    assert_ne!(at(200, 200), 0x20_2020, "the picture is in the saved shot");

    assert!(matches!(ed.key(VK_DELETE, false, false), Event::Repaint));
    assert_eq!(ed.marks.len(), 1, "Delete removes it");
    ed.undo();
    assert_eq!(image_rect(&ed, 1).w, 280);
    for w in [240, 200, 200] {
        ed.undo();
        assert_eq!(image_rect(&ed, 1).w, w);
    }
    ed.undo();
    assert_eq!(ed.marks.len(), 1, "the paste was one step");
    ed.redo();
    assert_eq!(image_rect(&ed, 1), Rect { x: 100, y: 100, w: 200, h: 100 });
}

#[test]
fn ctrl_v_with_only_text_on_the_clipboard_makes_a_text() {
    let (mut ed, clip) = editor();
    *clip.text.borrow_mut() = Some("from\r\nelsewhere".into());
    ed.mouse_move((50, 60));
    ed.key(0x56, true, false);
    assert_eq!(text_of(&ed, 0), "from\nelsewhere");
    assert!(ed.typing.is_none() && ed.picked == Some(0));
    ed.undo();
    assert!(ed.marks.is_empty());
}

#[test]
fn the_strip_pastes_recent_pictures_by_click_or_drag() {
    let (mut ed, _) = editor();
    ed.key(0x56, false, false); // V: the clipboard tool
    assert_eq!((ed.tool, ed.panel()), (Some(Tool::Image), Some(textbar::Panel::Strip)));
    assert!(ed.take_recents_request(), "opening the strip asks for its pictures");
    assert!(!ed.take_recents_request(), "once");
    let recent = |w, h, shot| {
        let img = Arc::new(picture(w, h));
        Recent { thumb: Arc::new(paste::thumbnail(&img, (176, 112))), full: Full::Ready(img), shot }
    };
    ed.set_recents(vec![recent(120, 80, false), recent(60, 60, true)], true);
    let (_, strip, cells) = ed.panel_layout().unwrap();
    assert_eq!(cells.len(), 2);
    assert!(strip.y > ed.bar.bottom(), "under the bar, away from the selection");
    let centre = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);

    click(&mut ed, centre(cells[0].1));
    assert_eq!(image_rect(&ed, 0), Rect { x: 140, y: 110, w: 120, h: 80 }, "a click pastes it in the middle of the shot");

    let steps = ed.history.len();
    ed.mouse_down(centre(cells[1].1), 0);
    ed.mouse_move((300, 60));
    assert_eq!(image_rect(&ed, 1), Rect { x: 270, y: 30, w: 60, h: 60 }, "pulled out of the strip, it's under the mouse");
    ed.mouse_move((320, 80));
    ed.mouse_up((320, 80));
    assert_eq!(image_rect(&ed, 1), Rect { x: 290, y: 50, w: 60, h: 60 }, "and follows it");
    assert_eq!(ed.history.len(), steps + 1, "dragging one in is one step");
    ed.undo();
    assert_eq!(ed.marks.len(), 1);

    ed.key(0x56, false, false);
    assert_eq!((ed.tool, ed.panel()), (None, None), "V again puts it away");
}

#[test]
fn thumbnails_average_what_they_cover() {
    let img = Image { w: 4, h: 2, px: vec![0xFF00_0000, 0xFFFF_FFFF, 0xFF00_0000, 0xFFFF_FFFF, 0xFF00_0000, 0xFFFF_FFFF, 0xFF00_0000, 0xFFFF_FFFF] };
    let t = paste::thumbnail(&img, (2, 2));
    assert_eq!((t.w, t.h), (2, 1));
    assert_eq!(t.px, [0xFF7F_7F7F, 0xFF7F_7F7F]);
    assert_eq!(paste::thumbnail(&img, (100, 100)), img, "never grown");
}

#[test]
fn text_editing_keys_work_like_a_text_box() {
    let (mut ed, clip) = editor();
    ed.key(0x54, false, false); // T
    click(&mut ed, (100, 100));
    type_text(&mut ed, "hello world");
    ed.key(0x41, true, false);
    assert_eq!(ed.selected_text().as_deref(), Some("hello world"), "Ctrl+A selects all of it");
    type_text(&mut ed, "x");
    assert_eq!(text_of(&ed, 0), "x", "typing replaces the selection");
    type_text(&mut ed, " big sunny day");
    ed.key(VK_BACK, true, false);
    assert_eq!(text_of(&ed, 0), "x big sunny ", "Ctrl+Backspace takes the word");
    ed.key(VK_LEFT, true, true);
    ed.key(VK_LEFT, true, true);
    assert_eq!(ed.selected_text().as_deref(), Some("big sunny "), "Ctrl+Shift+Left selects by words");
    ed.key(0x43, true, false);
    assert_eq!(clip.text.borrow().as_deref(), Some("big sunny "), "Ctrl+C copies the selection");
    ed.key(0x58, true, false);
    assert_eq!(text_of(&ed, 0), "x ", "Ctrl+X cuts it");
    ed.key(VK_HOME, false, false);
    ed.key(0x56, true, false);
    assert_eq!(text_of(&ed, 0), "big sunny x ", "Ctrl+V pastes at the caret");
    ed.key(VK_END, false, false);
    type_text(&mut ed, "\rnext");
    assert_eq!(text_of(&ed, 0), "big sunny x \nnext", "Enter starts a line");
    ed.key(VK_HOME, false, false);
    ed.key(VK_DELETE, true, false);
    assert_eq!(text_of(&ed, 0), "big sunny x \n", "Ctrl+Delete takes the word after");

    ed.key(0x5A, true, false);
    assert_eq!(text_of(&ed, 0), "big sunny x \nnext", "Ctrl+Z undoes one edit inside the text");
    ed.key(0x5A, true, false);
    assert_eq!(text_of(&ed, 0), "big sunny x \n", "a word typed is one step");
    ed.key(0x59, true, false);
    assert_eq!(text_of(&ed, 0), "big sunny x \nnext", "Ctrl+Y redoes it");
    assert!(ed.history.is_empty(), "nothing reaches the shot's history while typing");

    assert!(matches!(ed.key(VK_ESCAPE, false, false), Event::Repaint));
    assert!(ed.typing.is_none());
    assert_eq!(ed.history.len(), 1, "the whole text is one step");
    ed.undo();
    assert!(ed.marks.is_empty());
}

#[test]
fn ctrl_c_with_nothing_selected_still_copies_the_shot() {
    let (mut ed, _) = editor();
    ed.key(0x54, false, false);
    click(&mut ed, (100, 100));
    type_text(&mut ed, "abc");
    assert!(matches!(ed.key(0x43, true, false), Event::Finish(Output::Copy)));
}

#[test]
fn mouse_selects_text_and_double_click_picks_a_word() {
    let (mut ed, _) = editor();
    ed.key(0x54, false, false);
    click(&mut ed, (100, 100));
    type_text(&mut ed, "one two three");
    let b = ed.marks[0].bbox();
    let y = b.y + b.h / 2;
    let mid = (b.x + b.w / 2, y);
    ed.mouse_down(mid, 0);
    ed.mouse_move((b.right() + 30, y));
    ed.mouse_up((b.right() + 30, y));
    let selected = ed.selected_text().unwrap();
    assert!(selected.ends_with("three") && selected.len() < 13, "dragging selects: {selected:?}");
    ed.last_click = None;
    ed.mouse_down(mid, 0);
    ed.mouse_up(mid);
    ed.mouse_down(mid, 0);
    ed.mouse_up(mid);
    assert_eq!(ed.selected_text().as_deref(), Some("two"), "a double-click picks the word");
    assert!(!ed.selection_rects().is_empty());
}

#[test]
fn texts_move_reopen_and_resize() {
    let (mut ed, _) = editor();
    ed.key(0x54, false, false);
    click(&mut ed, (100, 100));
    type_text(&mut ed, "move me");
    ed.key(VK_ESCAPE, false, false);
    let b = ed.marks[0].bbox();
    let inside = (b.x + b.w / 2, b.y + b.h / 2);

    drag(&mut ed, inside, (inside.0 + 50, inside.1 + 30));
    assert_eq!(ed.marks[0].bbox(), b.offset(50, 30), "a drag moves it, text tool and all");
    assert!(ed.typing.is_none(), "without typing into it");
    assert_eq!(ed.history.len(), 2);

    let b = ed.marks[0].bbox();
    click(&mut ed, (b.x + b.w / 2, b.y + b.h / 2));
    assert!(ed.typing.is_some(), "a click types into it");
    ed.key(VK_END, false, false);
    type_text(&mut ed, "!");
    assert_eq!(text_of(&ed, 0), "move me!");

    // the corners scale the font, keeping the opposite corner put
    let size = ed.marks[0].strength().unwrap();
    let b = ed.marks[0].bbox();
    drag(&mut ed, (b.right(), b.bottom()), (b.right() + b.w, b.bottom() + b.h));
    let grown = ed.marks[0].strength().unwrap();
    assert!(grown > size * 1.8, "{size} -> {grown}");
    assert_eq!((ed.marks[0].bbox().x, ed.marks[0].bbox().y), (b.x, b.y));
    assert!(ed.typing.is_some(), "resizing the text being typed keeps typing");
    ed.key(0x5A, true, false);
    assert_eq!(ed.marks[0].strength(), Some(size), "and Ctrl+Z undoes the resize first");

    // a side sets the width it wraps at
    let b = ed.marks[0].bbox();
    let left = (b.x, b.y + b.h / 2);
    drag(&mut ed, left, (left.0 + b.w / 2, left.1));
    let m = &ed.marks[0];
    assert!(matches!(m, Mark::Text { width: Some(_), .. }));
    assert!(m.bbox().h > b.h, "narrower, it wraps onto another line");
    assert_eq!(m.bbox().right(), b.right(), "the other side stays");

    ed.key(VK_ESCAPE, false, false);
    assert!(matches!(ed.key(VK_DELETE, false, false), Event::Repaint));
    assert!(ed.marks.is_empty(), "Delete removes a picked text");
}

#[test]
fn styles_apply_round_trip_and_carry_over() {
    let _memory = memory();
    let (mut ed, _) = editor();
    ed.key(0x54, false, false);
    click(&mut ed, (100, 100));
    type_text(&mut ed, "styled");
    let start = style_of(&ed, 0);
    let w = ed.marks[0].bbox().w;
    ed.key(0x42, true, false); // Ctrl+B
    assert_eq!(style_of(&ed, 0).bold, !start.bold);
    assert_ne!(ed.marks[0].bbox().w, w, "laid out again");
    ed.key(0x49, true, false);
    ed.key(0x55, true, false);
    let s = style_of(&ed, 0);
    assert!(s.italic != start.italic && s.underline != start.underline);
    ed.activate(Item::Strike);
    ed.activate(Item::Align);
    let back = start.back.next();
    ed.activate(Item::Back);
    let s = style_of(&ed, 0);
    assert!(s.strike != start.strike && s.align == start.align.next() && s.back == back);

    ed.activate(Item::Font);
    assert!(ed.font_list().is_some(), "the font button opens the list");
    let families = families();
    let target = families.last().cloned().unwrap();
    for c in target.chars() {
        ed.char(c as u16);
    }
    let list = ed.font_list().unwrap();
    assert_eq!(&*list.rows[0].2, &*target, "searching finds it");
    assert_eq!(text_of(&ed, 0), "styled", "and doesn't type into the text");
    ed.key(0x0D, false, false);
    assert!(ed.fonts_open.is_none());
    assert_eq!(style_of(&ed, 0).font, target, "Enter takes it");

    ed.key(VK_ESCAPE, false, false);
    assert_eq!(ed.history.len(), 1, "typing and styling: one step");
    ed.picked = Some(0);
    ed.activate(Item::Back);
    assert_eq!(ed.history.len(), 2, "restyling a picked text is a step of its own");
    ed.undo();
    assert_eq!(style_of(&ed, 0).back, back);

    click(&mut ed, (100, 250));
    type_text(&mut ed, "next");
    let next = style_of(&ed, 1);
    assert_eq!((next.font.clone(), next.bold, next.align), (target.clone(), !start.bold, start.align.next()), "the last style is remembered");
    let fresh = editor().0;
    assert_eq!(fresh.text_style.font, target, "by the next shot too");
}

#[test]
fn typing_repaints_only_around_the_text() {
    let screen = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    let mut ed = Editor::new(screen, None, vec![screen], 1.0);
    ed.activate(Item::Tool(Tool::Text));
    click(&mut ed, (1700, 700));
    for u in "the quick brown fox".encode_utf16() {
        let before = ed.frame();
        assert!(matches!(ed.char(u), Event::Repaint));
        let damage = ed.damage(&before);
        let big = damage.iter().filter(|r| r.w * r.h > 600 * 200).collect::<Vec<_>>();
        assert!(big.iter().all(|r| *r == &ed.bar || ed.chrome().contains(r)), "{damage:?}");
    }
}

/// Renders the pieces to look at: `%TEMP%\glint-review\shot\*.png`.
#[test]
#[ignore = "writes review PNGs: cargo test --release -- --ignored review_shots"]
fn review_shots() {
    let dir = std::env::temp_dir().join("glint-review").join("shot");
    std::fs::create_dir_all(&dir).unwrap();
    let painter = Painter::new().unwrap();
    let font = painter.font(13.0).unwrap();
    let screen = Rect { x: 0, y: 0, w: 1280, h: 800 };
    // a busy "screen": stripes and blocks, so readability aids have something to fight
    let mut bright = Dib::new(screen.w, screen.h).unwrap();
    for (i, p) in bright.pixels_mut().iter_mut().enumerate() {
        let (x, y) = ((i as i32 % screen.w) as u32, (i as i32 / screen.w) as u32);
        let band = if (x / 40 + y / 60) % 2 == 0 { 0x00E8_EEF4 } else { 0x0030_4058 };
        *p = 0xFF00_0000 | band ^ ((x * 3 + y * 2) & 0x1F) << 8;
    }
    let mut dim = Dib::new(screen.w, screen.h).unwrap();
    for (d, b) in dim.pixels_mut().iter_mut().zip(bright.pixels()) {
        *d = ((b & 0x00FE_FEFE) >> 1) | 0xFF00_0000;
    }
    let shoot = |ed: &mut Editor, name: &str, area: Rect| {
        let window = Dib::new(screen.w, screen.h).unwrap();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::BitBlt(window.dc, 0, 0, screen.w, screen.h, Some(dim.dc), 0, 0, windows::Win32::Graphics::Gdi::SRCCOPY);
        }
        // the panels slide out: show them at rest
        ed.panel_t();
        if let Some((p, _)) = ed.panel_since.get() {
            ed.panel_since.set(Some((p, Instant::now() - Duration::from_secs(1))));
        }
        if let Some(f) = ed.fonts_open.as_mut() {
            f.since = Instant::now() - Duration::from_secs(1);
        }
        if let Some(p) = ed.picker.as_mut() {
            p.since = Instant::now() - Duration::from_secs(1);
        }
        for (_, at) in &mut ed.recents {
            *at = Instant::now() - Duration::from_secs(1);
        }
        ed.render(&bright, &painter, &font).unwrap();
        ed.paint(window.dc, screen, &dim, &painter, &font).unwrap();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::GdiFlush();
        }
        let a = area.intersect(&screen).unwrap();
        let mut px = Vec::with_capacity((a.w * a.h) as usize);
        for y in a.y..a.bottom() {
            px.extend_from_slice(&window.pixels()[(y * screen.w + a.x) as usize..(y * screen.w + a.right()) as usize]);
        }
        let png = glint_media::image::encode(&px, a.w as u32, a.h as u32, false, glint_media::image::ImageFormat::Png, 100).unwrap();
        std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
    };
    let new = || {
        let mut ed = Editor::new(Rect { x: 140, y: 80, w: 1000, h: 520 }, None, vec![screen], 1.0);
        ed.clip = Box::new(Rc::new(Fake::default()));
        ed
    };

    // a pasted picture with its handles
    let mut ed = new();
    let mut pic = Image { w: 360, h: 220, px: Vec::new() };
    pic.px = (0..360 * 220)
        .map(|i| {
            let (x, y) = (i % 360, i / 360);
            let edge = x < 3 || y < 3 || x > 356 || y > 216;
            if edge { 0xFF10_1014 } else { 0xFF00_0000 | (x * 255 / 360) << 16 | (y * 255 / 220) << 8 | 0xC0 }
        })
        .collect();
    ed.mouse_move((560, 330));
    ed.paste_image(Arc::new(pic.clone()), (560, 330));
    shoot(&mut ed, "paste-handles", Rect { x: 120, y: 60, w: 1040, h: 640 });

    // the clipboard strip
    let mut ed = new();
    ed.activate(Item::Tool(Tool::Image));
    let mut list = Vec::new();
    for (i, (w, h)) in [(360, 220), (200, 200), (400, 120), (300, 300), (240, 160)].into_iter().enumerate() {
        let tint = [0x00FF_6B6B, 0x0035_C7FF, 0x00FF_D60A, 0x0034_C759, 0x00BF_5AF2][i];
        let img = Image { w, h, px: (0..w * h).map(|j| 0xFF00_0000 | if (j % w / 20 + j / w / 20) % 2 == 0 { tint } else { 0x0020_2228 }).collect() };
        let img = Arc::new(img);
        list.push(Recent { thumb: Arc::new(paste::thumbnail(&img, (176, 112))), full: Full::Ready(img), shot: i >= 3 });
    }
    ed.set_recents(list, true);
    ed.hover = Some(Item::Recent(1));
    let (_, strip, _) = ed.panel_layout().unwrap();
    let area = hull(ed.bar, strip).inflate(16);
    shoot(&mut ed, "clipboard-strip", area);

    // text in each style, with the text bar
    let mut ed = new();
    type Row = (&'static str, fn(&mut Style), usize);
    let rows: [Row; 9] = [
        ("outline (default)", |_| {}, 4),
        ("shade pill", |s| s.back = Back::Shade, 4),
        ("tag pill", |s| s.back = Back::Tag, 1),
        ("no backdrop", |s| s.back = Back::None, 3),
        ("bold off", |s| s.bold = false, 0),
        ("italic", |s| s.italic = true, 2),
        ("underline", |s| s.underline = true, 4),
        ("strikethrough", |s| s.strike = true, 0),
        ("Georgia, centred\nover two lines", |s| {
            s.font = Arc::from("Georgia");
            s.align = Align::Centre;
            s.back = Back::Shade;
        }, 4),
    ];
    for (i, (text, f, color)) in rows.into_iter().enumerate() {
        let mut style = Style::default();
        f(&mut style);
        ed.text_style = style;
        ed.color = PALETTE[color];
        ed.tool = Some(Tool::Text);
        let at = (190 + (i as i32 % 2) * 470, 110 + (i as i32 / 2) * 95);
        ed.start_typing(at);
        ed.insert(text);
        ed.commit();
    }
    ed.text_style = Style::default();
    ed.tool = Some(Tool::Text);
    ed.start_typing((660, 490));
    ed.insert("typing: selected");
    ed.key(0x25, true, true);
    shoot(&mut ed, "text-styles", Rect { x: 120, y: 60, w: 1040, h: 740 });

    // the font list: at once in the UI's face, then each family in its own as it loads
    ed.activate(Item::Font);
    ed.char('s' as u16);
    ed.char('e' as u16);
    let list = ed.font_list().unwrap();
    let (_, panel, _) = ed.panel_layout().unwrap();
    let area = hull(hull(ed.bar, panel), list.rect).inflate(16);
    shoot(&mut ed, "text-bar-fonts-opening", area);
    let until = Instant::now() + Duration::from_secs(5);
    while ed.fonts_pending() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    shoot(&mut ed, "text-bar-fonts", area);

    // text boxes taller than their text, aligned top / middle / bottom, one picked
    let mut ed = new();
    for (i, (valign, back, color)) in
        [(VAlign::Top, Back::Shade, 4), (VAlign::Middle, Back::Tag, 3), (VAlign::Bottom, Back::Tag, 1)].into_iter().enumerate()
    {
        ed.text_style = Style { valign, back, ..Style::default() };
        ed.color = PALETTE[color];
        ed.tool = Some(Tool::Text);
        ed.start_typing((200 + i as i32 * 300, 200));
        ed.insert(["top", "middle\nof the box", "bottom"][i]);
        ed.commit();
        if let Mark::Text { height, width, .. } = &mut ed.marks[i] {
            (*height, *width) = (Some(180.0), Some(220.0));
        }
        let mut m = ed.marks[i].clone();
        ed.refit(&mut m);
        ed.marks[i] = m;
    }
    ed.picked = Some(1);
    ed.touch();
    shoot(&mut ed, "text-box-heights", Rect { x: 120, y: 60, w: 1040, h: 640 });

    // the colour picker over a picked rectangle, then its colours kept on the palette
    let mut ed = new();
    ed.marks.push(Mark::Rectangle { rect: Rect { x: 300, y: 180, w: 420, h: 260 }, color: PALETTE[0], width: 6.0 });
    ed.marks.push(Mark::Arrow { from: (820.0, 480.0), to: (700.0, 380.0), color: PALETTE[3], width: 6.0 });
    ed.picked = Some(0);
    ed.toggle_picker();
    let l = ed.picker_layout().unwrap();
    let drag = ed.grab_picker(Part::Strip, (l.strip.x + l.strip.w * 78 / 100, l.strip.y + 4));
    ed.drag_picker(Part::Square, (l.square.x + l.square.w * 7 / 10, l.square.y + l.square.h / 5));
    if let Drag::Color { before, .. } = drag {
        ed.drop_picker(before);
    }
    ed.hover = Some(Item::Picker);
    let area = hull(hull(ed.bar, l.rect), Rect { x: 280, y: 160, w: 560, h: 340 }).inflate(16);
    shoot(&mut ed, "colour-picker", area);
    ed.close_picker();
    ed.set_ink(Ink([0x10, 0xE0, 0xD0]));
    ed.toggle_picker();
    ed.close_picker();
    ed.hover = None;
    let area = ed.bar.inflate(16);
    shoot(&mut ed, "colour-customs", area);

    // the toolbar as it comes, then laid out in the settings: groups, three tools hidden, eraser on 6
    let mut ed = new();
    ed.hover = Some(Item::Tool(Tool::Eraser));
    let area = ed.bar.inflate(16);
    shoot(&mut ed, "toolbar-default", Rect { h: area.h + 40, ..area });
    let (t, s) = (Slot::Tool, Slot::Separator);
    ed.slots = vec![t(0), t(1), t(2), s, t(3), t(4), s, t(7), t(8), s, t(6), t(10)];
    ed.keys.set(6, KeyKind::Digit, Some(b'6'));
    ed.cells.clear();
    ed.place_bar();
    let area = ed.bar.inflate(16);
    shoot(&mut ed, "toolbar-custom", Rect { h: area.h + 40, ..area });
}

#[test]
fn text_boxes_take_a_height_and_align_inside_it() {
    let _memory = memory();
    let (mut ed, _) = editor();
    // a known look, whatever other tests left remembered
    ed.text_style = Style::default();
    ed.strength[Tool::Text as usize] = 24.0;
    ed.key(0x54, false, false);
    // high up: the box grows down, and mustn't reach the toolbar under the selection
    click(&mut ed, (100, 40));
    type_text(&mut ed, "box");
    let b = ed.marks[0].bbox();
    let lines = b.h;
    // the bottom handle sits on the dashed outline, under the middle of the text
    let o = ed.outline(&ed.marks[0]);
    let bottom = (o.x + o.w / 2, o.bottom());
    drag(&mut ed, bottom, (bottom.0, bottom.1 + 100));
    let tall = ed.marks[0].bbox();
    assert_eq!((tall.y, tall.h), (b.y, lines + 100), "the bottom handle makes the box taller");
    assert!(matches!(ed.marks[0], Mark::Text { height: Some(_), .. }));
    let caret = ed.caret_rect().unwrap();
    assert!(caret.y < b.y + lines, "top-aligned: the text stays at the top");

    ed.activate(Item::VAlign);
    let middle = ed.caret_rect().unwrap();
    assert!((middle.y - (caret.y + 50)).abs() <= 1, "middle: {} vs {}", middle.y, caret.y);
    ed.activate(Item::VAlign);
    assert!((ed.caret_rect().unwrap().y - (caret.y + 100)).abs() <= 1, "bottom");

    let o = ed.outline(&ed.marks[0]);
    let top = (o.x + o.w / 2, o.y);
    drag(&mut ed, top, (top.0, top.1 + 400));
    let fitted = ed.marks[0].bbox();
    assert_eq!(fitted.h, lines, "never shorter than the text: it fits it again");
    assert!(matches!(ed.marks[0], Mark::Text { height: None, .. }));
    assert_eq!(fitted.bottom(), tall.bottom(), "the bottom stayed put");

    let o = ed.outline(&ed.marks[0]);
    let bottom = (o.x + o.w / 2, o.bottom());
    drag(&mut ed, bottom, (bottom.0, bottom.1 + 30));
    let h = ed.marks[0].bbox().h;
    type_text(&mut ed, "\r2\r3");
    assert!(ed.marks[0].bbox().h > h, "typing past the box grows it");

    let size = ed.marks[0].strength().unwrap();
    let o = ed.outline(&ed.marks[0]);
    ed.set_mods(true, false, false);
    drag(&mut ed, (o.right(), o.bottom()), (o.right() + 30, o.bottom() + 20));
    ed.set_mods(false, false, false);
    assert_eq!(ed.marks[0].strength(), Some(size), "Ctrl+corner sizes the box, not the font");
    assert!(matches!(ed.marks[0], Mark::Text { width: Some(_), height: Some(_), .. }));
    let o = ed.outline(&ed.marks[0]);
    drag(&mut ed, (o.right(), o.bottom()), (o.right() + 30, o.bottom() + 20));
    assert!(ed.marks[0].strength().unwrap() > size, "a plain corner scales the font");
}

#[test]
fn the_picker_recolours_marks_and_keeps_custom_colours() {
    let _memory = memory();
    let (mut ed, _) = editor();
    ed.marks.push(Mark::Rectangle { rect: Rect { x: 50, y: 50, w: 200, h: 100 }, color: PALETTE[0], width: 4.0 });
    ed.picked = Some(0);
    let swatch = ed.cells.iter().find(|(i, _)| *i == Item::Picker).unwrap().1;
    click(&mut ed, (swatch.x + swatch.w / 2, swatch.y + swatch.h / 2));
    let l = ed.picker_layout().expect("the swatch opens the picker");
    assert!(l.rect.y > ed.bar.bottom(), "under the bar");

    // a drag on the strip and the square recolours the picked mark live, as one undo step
    let steps = ed.history.len();
    ed.mouse_down((l.strip.x + l.strip.w / 3, l.strip.y + 3), 0);
    ed.mouse_up((l.strip.x + l.strip.w / 3, l.strip.y + 3));
    ed.mouse_down((l.square.right() - 1, l.square.y), 0);
    ed.mouse_move((l.square.right() - 1, l.square.y + 1));
    ed.mouse_up((l.square.right() - 1, l.square.y + 1));
    let picked = ed.marks[0].color().unwrap();
    assert_ne!(picked, PALETTE[0]);
    let [r, g, b] = picked.0;
    assert!(g > 240 && r < 20 && b < 20, "a third of the way round: green, {picked:?}");
    assert_eq!(ed.history.len(), steps + 2, "one step per press");
    ed.undo();
    ed.undo();
    assert_eq!(ed.marks[0].color(), Some(PALETTE[0]));
    ed.redo();
    ed.redo();

    // the hex field: six digits make the colour
    for c in "#1a2b3c".chars() {
        ed.char(c as u16);
    }
    assert_eq!(ed.marks[0].color(), Some(Ink([0x1A, 0x2B, 0x3C])));
    ed.key(0x41, false, false); // A: a hex digit, not the arrow tool
    assert_eq!(ed.tool, None);
    assert!(matches!(ed.key(0x1B, false, false), Event::Repaint));
    assert!(ed.picker.is_none(), "Esc puts the picker away first");
    assert_eq!(ed.customs[0], Ink([0x1A, 0x2B, 0x3C]), "and its colour joins the palette");
    assert!(ed.cells.iter().any(|(i, _)| *i == Item::Custom(0)));
    let fresh = editor().0;
    assert!(fresh.customs.contains(&Ink([0x1A, 0x2B, 0x3C])), "next shot too");

    // a custom swatch colours what's drawn next
    ed.picked = None;
    let custom = ed.cells.iter().find(|(i, _)| *i == Item::Custom(0)).unwrap().1;
    click(&mut ed, (custom.x + custom.w / 2, custom.y + custom.h / 2));
    ed.key(0x4E, false, false); // N: steps
    click(&mut ed, (300, 200));
    assert_eq!(ed.marks[1].color(), Some(Ink([0x1A, 0x2B, 0x3C])));
}

/// Frame time while dragging a pasted 4K picture across a 3440x1440 selection: the event, the
/// view patch and the composed paint of the damage (into an off-screen DIB).
#[test]
#[ignore = "timing: cargo test --release -- --ignored --nocapture picture_drag"]
fn picture_drag_latency() {
    let painter = Painter::new().unwrap();
    let font = painter.font(13.0).unwrap();
    let screen = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    let mut bright = Dib::new(screen.w, screen.h).unwrap();
    bright.pixels_mut().iter_mut().enumerate().for_each(|(i, p)| *p = (i as u32).wrapping_mul(2_654_435_761) >> 8);
    let (dim, window) = (Dib::new(screen.w, screen.h).unwrap(), Dib::new(screen.w, screen.h).unwrap());
    let mut ed = Editor::new(screen, None, vec![screen], 1.0);
    ed.paste_image(Arc::new(picture(3840, 2160)), (1720, 720));
    ed.render(&bright, &painter, &font).unwrap();
    ed.paint(window.dc, screen, &dim, &painter, &font).unwrap();
    let r = image_rect(&ed, 0);
    let mut p = (r.x + r.w / 2, r.y + r.h / 2);
    ed.mouse_down(p, 0);
    let mut times = Vec::new();
    for _ in 0..40 {
        p = (p.0 - 7, p.1 + 3);
        let t0 = Instant::now();
        let before = ed.frame();
        ed.mouse_move(p);
        let damage = ed.damage(&before);
        let rp = damage.iter().copied().reduce(hull).unwrap();
        ed.render(&bright, &painter, &font).unwrap();
        ed.paint(window.dc, rp, &dim, &painter, &font).unwrap();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::GdiFlush();
        }
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    ed.mouse_up(p);
    times.sort_by(f64::total_cmp);
    let n = times.len();
    println!("dragging a 4K picture: median {:.2} ms, p90 {:.2} ms, max {:.2} ms", times[n / 2], times[n * 9 / 10], times[n - 1]);
}

/// Opening the font list (first time in the process: the system fonts aren't known yet) and
/// scrolling it through every installed family: the event plus the composed paint of what changed.
#[test]
#[ignore = "timing: cargo test --release -- --ignored --nocapture font_list_latency"]
fn font_list_latency() {
    let painter = Painter::new().unwrap();
    let font = painter.font(13.0).unwrap();
    let screen = Rect { x: 0, y: 0, w: 3440, h: 1440 };
    let bright = Dib::new(screen.w, screen.h).unwrap();
    let (dim, window) = (Dib::new(screen.w, screen.h).unwrap(), Dib::new(screen.w, screen.h).unwrap());
    let t0 = Instant::now();
    let mut ed = Editor::new(Rect { x: 600, y: 300, w: 2000, h: 700 }, None, vec![screen], 1.0);
    let names = families();
    let read = t0.elapsed().as_secs_f64() * 1000.0;
    ed.activate(Item::Tool(Tool::Text));
    ed.render(&bright, &painter, &font).unwrap();
    ed.paint(window.dc, screen, &dim, &painter, &font).unwrap();
    let step = |ed: &mut Editor, f: &mut dyn FnMut(&mut Editor) -> Event| {
        let t0 = Instant::now();
        let before = ed.frame();
        f(ed);
        let damage = ed.damage(&before);
        ed.render(&bright, &painter, &font).unwrap();
        if let Some(rp) = damage.iter().copied().reduce(hull) {
            ed.paint(window.dc, rp, &dim, &painter, &font).unwrap();
        }
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::GdiFlush();
        }
        t0.elapsed().as_secs_f64() * 1000.0
    };
    let open = step(&mut ed, &mut |e| e.activate(Item::Font));
    // the rows swap into their own faces as the font thread loads them
    let shown = Instant::now();
    let mut swaps = 0;
    while ed.fonts_pending() && shown.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(2));
        swaps += 1;
        step(&mut ed, &mut |e| e.tick());
    }
    let faces = shown.elapsed().as_secs_f64() * 1000.0;
    let mut frames = Vec::new();
    let families = names.len();
    for _ in 0..families {
        frames.push(step(&mut ed, &mut |e| e.scroll_fonts(1)));
    }
    frames.sort_by(f64::total_cmp);
    let n = frames.len().max(1);
    println!(
        "font list: families read in the background in {read:.0} ms; open {open:.1} ms, own faces after {faces:.0} ms ({swaps} ticks); scrolling {families} families: median {:.2} ms, p90 {:.2} ms, max {:.2} ms, total {:.0} ms",
        frames[n / 2],
        frames[n * 9 / 10],
        frames[n - 1],
        frames.iter().sum::<f64>()
    );
}

/// The installed families, once the font thread has read them.
fn families() -> Arc<Vec<Arc<str>>> {
    fonts::start();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(names) = fonts::names() {
            return names;
        }
        assert!(Instant::now() < until, "the font families never arrived");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Whether the list painted at opacity `painted` now looks stale at `now`: off by more than an
/// alpha step or two, or short of fully opaque once the fade is over.
fn stale(painted: f32, now: f32) -> bool {
    painted < 1.0 && (now == 1.0 || ((now - painted) * 255.0) >= 2.0)
}

/// Whether `damage` repaints all of the font list.
fn repaints_list(ed: &Editor, damage: &[Rect]) -> bool {
    let list = ed.font_list().unwrap().rect;
    damage.iter().any(|r| r.intersect(&list) == Some(list))
}

/// The editor painted off-screen the way the picker's WM_PAINT paints it.
struct Offscreen {
    painter: Painter,
    font: windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
    screen: Rect,
    bright: Dib,
    dim: Dib,
    window: Dib,
}

impl Offscreen {
    fn new(screen: Rect) -> Offscreen {
        let painter = Painter::new().unwrap();
        let font = painter.font(13.0).unwrap();
        let dib = || Dib::new(screen.w, screen.h).unwrap();
        Offscreen { painter, font, screen, bright: dib(), dim: dib(), window: dib() }
    }

    /// Runs `f` on the editor and paints what it damaged; returns the damage and how long the UI
    /// thread spent on it all (ms).
    fn show(&self, ed: &mut Editor, f: impl FnOnce(&mut Editor) -> Event) -> (Vec<Rect>, f64) {
        let t0 = Instant::now();
        let before = ed.frame();
        f(ed);
        let damage = ed.damage(&before);
        ed.render(&self.bright, &self.painter, &self.font).unwrap();
        if let Some(rp) = damage.iter().copied().reduce(hull) {
            ed.paint(self.window.dc, rp, &self.dim, &self.painter, &self.font).unwrap();
        }
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::GdiFlush();
        }
        (damage, t0.elapsed().as_secs_f64() * 1000.0)
    }

    fn paint_all(&self, ed: &mut Editor) {
        ed.render(&self.bright, &self.painter, &self.font).unwrap();
        ed.paint(self.window.dc, self.screen, &self.dim, &self.painter, &self.font).unwrap();
    }
}

fn list_opacity(ed: &Editor) -> f32 {
    ed.fonts_open.as_ref().map_or(0.0, |f| textbar::progress(f.since, textbar::PANEL_MS))
}

#[test]
fn the_font_list_fades_in_without_input() {
    let screen = Rect { x: 0, y: 0, w: 1280, h: 800 };
    let off = Offscreen::new(screen);
    let mut ed = Editor::new(Rect { x: 140, y: 80, w: 1000, h: 520 }, None, vec![screen], 1.0);
    ed.activate(Item::Tool(Tool::Text));
    off.paint_all(&mut ed);
    off.show(&mut ed, |e| e.activate(Item::Font));
    let mut painted = list_opacity(&ed);
    assert!(painted < 1.0, "it fades in");
    // nothing but the ticks the editor asks for, as the picker's timer gives them
    let mut frames = 0;
    while let Some(wait) = ed.next_tick() {
        assert!(wait <= Duration::from_millis(16), "a frame at a time: {wait:?}");
        std::thread::sleep(wait);
        let (damage, _) = off.show(&mut ed, |e| e.tick());
        let now = list_opacity(&ed);
        if repaints_list(&ed, &damage) {
            painted = now;
        }
        assert!(!stale(painted, now), "frame {frames}: the list shows {painted} but is at {now}");
        frames += 1;
        assert!(frames < 300, "it never settles");
    }
    assert_eq!(painted, 1.0, "painted fully opaque with no input at all");
    assert!(frames >= 5, "{frames} frames");
}

/// The font list opening on a cold start, in a test process of its own (a fresh font thread
/// and DirectWrite), with the font thread slowed down like a cold font cache.
#[test]
fn the_font_list_never_waits_for_a_cold_start() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "shot::editor::checks::font_list_cold_child", "--nocapture", "--test-threads=1"])
        .env("GLINT_TEST_COLD_FONTS", "1")
        .env("GLINT_TEST_FONT_DELAY_MS", "600")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    print!("{stdout}");
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "the cold start failed");
    assert!(stdout.contains("1 passed"), "the cold start didn't run");
}

#[test]
#[ignore = "run by the_font_list_never_waits_for_a_cold_start in a process of its own"]
fn font_list_cold_child() {
    if std::env::var_os("GLINT_TEST_COLD_FONTS").is_none() {
        return;
    }
    const WM_FONTS: u32 = 0x8000 + 43;
    let probe = fonts::probe::window();
    fonts::wake(probe, WM_FONTS);
    let screen = Rect { x: 0, y: 0, w: 1280, h: 800 };
    let off = Offscreen::new(screen);
    let t0 = Instant::now();
    let mut ed = Editor::new(Rect { x: 140, y: 80, w: 1000, h: 520 }, None, vec![screen], 1.0);
    ed.activate(Item::Tool(Tool::Text));
    off.paint_all(&mut ed);
    let built = t0.elapsed().as_secs_f64() * 1000.0;
    assert!(fonts::names().is_none(), "the font thread is slowed: no families yet");

    let opened = Instant::now();
    let (_, open) = off.show(&mut ed, |e| e.activate(Item::Font));
    let list = ed.font_list().expect("the list opens at once");
    assert!(list.loading && list.rows.is_empty(), "showing that it's reading fonts");
    let mut painted = list_opacity(&ed);
    let since = |at: Instant| at.duration_since(opened).as_secs_f64() * 1000.0;

    let (mut worst, mut wakes, mut ticks) = (open, 0, 0);
    let (mut opaque, mut names, mut faces) = (None, None, None);
    let mut due = ed.next_tick().map(|w| Instant::now() + w);
    let until = Instant::now() + Duration::from_secs(10);
    while faces.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
        let mut step = None;
        if fonts::probe::count(probe, WM_FONTS) > 0 {
            fonts::woken(probe);
            wakes += 1;
            step = Some(off.show(&mut ed, |e| e.fonts_changed()));
        } else if due.is_some_and(|d| Instant::now() >= d) {
            ticks += 1;
            step = Some(off.show(&mut ed, |e| e.tick()));
        }
        if let Some((damage, _)) = &step {
            let now = list_opacity(&ed);
            if repaints_list(&ed, damage) {
                painted = now;
            }
            assert!(!stale(painted, now), "tick {ticks}: the list shows {painted} but is at {now}");
        }
        let Some((_, ms)) = step else { continue };
        worst = worst.max(ms);
        // like the picker's timer: asking again never puts a pending tick off
        let next = ed.next_tick().map(|w| Instant::now() + w);
        due = match (due, next) {
            (Some(d), Some(n)) if d > Instant::now() => Some(d.min(n)),
            (_, n) => n,
        };
        if opaque.is_none() && painted == 1.0 {
            opaque = Some(Instant::now());
        }
        if names.is_none() && fonts::names().is_some() {
            names = Some(Instant::now());
        }
        if names.is_some() && !ed.fonts_pending() {
            faces = Some(Instant::now());
        }
    }
    let (opaque, names, faces) = (opaque.expect("never opaque"), names.expect("no families"), faces.expect("no faces"));
    assert!(opaque < names, "the list was painted fully opaque while the fonts were still being read");
    assert!(wakes >= 2, "the font thread woke the window: {wakes}");
    assert!(open < 50.0 && worst < 50.0, "the UI thread never waits: open {open:.1} ms, slowest step {worst:.1} ms");
    println!(
        "cold start, font thread slowed by 600 ms: editor up in {built:.0} ms; list open in {open:.1} ms (reading fonts…); \
         fully opaque after {:.0} ms with no input ({ticks} ticks); families after {:.0} ms and own faces after {:.0} ms \
         ({wakes} wake messages); slowest UI step {worst:.1} ms",
        since(opaque),
        since(names),
        since(faces)
    );
    fonts::probe::close(probe);
}
