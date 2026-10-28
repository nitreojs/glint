//! A settings window in the glint look, for any of our apps: its own title bar, a sidebar of
//! pages, the page in a scrolling panel, and a footer with reset, cancel / close, save and the
//! unsaved count; an update to take rises in at the bottom of the sidebar. Closing with unsaved
//! changes asks first. It follows the Windows light / dark app setting. The app supplies the
//! pages ([`Pages`]); [`run`] opens the window.
//!
//! Run it as a separate short-lived process (`app.exe --settings`): winit allows only one event
//! loop per process and the window can be opened many times.

use std::path::PathBuf;
use std::sync::OnceLock;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Event, Id, Key, Layout, Margin, Modifiers, Rect, RichText, Stroke, Ui, UiBuilder,
    pos2, vec2,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;

use crate::theme::{FONT_LICENSE, fonts, visuals};
use crate::updates::{self, Updater};
use crate::widgets::{
    self, BAR_H, CONTENT_X, FOOTER_H, Kind, Look, SIDEBAR_W, TitleAction, WINDOW, hint, mono, sans, semibold,
};
use crate::window::{self, frameless};

/// The bright panel's rounding where it meets the darker frame.
const PANEL_RADIUS: u8 = 8;
/// How long a newly picked page takes to fade in, and how far it drops from.
const PAGE_ENTER: f32 = 0.18;
const PAGE_DROP: f32 = 8.0;

/// What a settings window shows and edits.
pub trait Pages: 'static {
    type Page: Copy + PartialEq + 'static;
    /// The app's name, in the title bar and the save dialog.
    const APP: &'static str;

    /// The sidebar, top to bottom.
    fn pages(&self) -> &'static [Self::Page];
    fn name(&self, page: Self::Page) -> &'static str;
    /// A [`widgets::badge`] beside the page's name in the sidebar and its title ("beta").
    fn badge(&self, _page: Self::Page) -> Option<&'static str> {
        None
    }
    fn accent(&self) -> [u8; 3];
    /// Unsaved changes on `page`.
    fn changes(&self, page: Self::Page) -> usize;
    /// `None`: the page has no reset button; otherwise whether it differs from the defaults.
    fn resettable(&self, page: Self::Page) -> Option<bool>;
    fn reset(&mut self, page: Self::Page);
    fn show(&mut self, page: Self::Page, ui: &mut Ui, look: &Look);
    /// Shown over the page (a failed save, say).
    fn error(&self) -> Option<String>;
    /// Whether what's edited can be saved (valid, nothing half-done); the frame also needs changes.
    fn can_save(&self) -> bool;
    /// Writes the settings and tells the app; false keeps the window open (with an [`Pages::error`]).
    fn save(&mut self) -> bool;
    /// Back to what's saved.
    fn revert(&mut self);
    /// Called before switching pages, resetting or reverting: stop anything half-done.
    fn leave(&mut self) {}
    /// Called at the start of every frame.
    fn tick(&mut self) {}
    /// Something is being recorded (a hotkey): Esc belongs to it, not to closing.
    fn busy(&self) -> bool {
        false
    }
    /// Dialogs of the app's own; returns whether one is open.
    fn overlays(&mut self, _ctx: &egui::Context, _look: &Look) -> bool {
        false
    }
    /// The app's updates, for the update button at the bottom of the sidebar.
    fn updater(&mut self) -> Option<&mut Updater> {
        None
    }
}

static CTX: OnceLock<egui::Context> = OnceLock::new();

/// Repaints the settings window, from any thread.
pub fn repaint() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

fn saved_size(path: &PathBuf) -> [f32; 2] {
    let text = std::fs::read_to_string(path).ok();
    let read = text.as_deref().and_then(|t| {
        let nums: Vec<f32> = t.trim().trim_matches(['[', ']']).split(',').filter_map(|n| n.trim().parse().ok()).collect();
        (nums.len() == 2).then(|| [nums[0], nums[1]])
    });
    let [w, h] = read.unwrap_or(WINDOW);
    [w.clamp(WINDOW[0], 8000.0), h.clamp(WINDOW[1], 8000.0)]
}

/// Opens the window titled `title`, sized as it was last left (kept in `size_file`), until it's
/// closed. `icon` is 64×64 RGBA.
pub fn run<P: Pages>(title: &str, size_file: PathBuf, icon: Vec<u8>, pages: P) {
    let size = saved_size(&size_file);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(size)
            .with_min_inner_size(WINDOW)
            .with_resizable(true)
            .with_decorations(false)
            .with_transparent(window::transparent())
            .with_active(!window::quiet())
            .with_icon(egui::IconData { rgba: icon, width: 64, height: 64 }),
        centered: true,
        ..Default::default()
    };
    let mut frame = Frame::new(pages, (size, size));
    frame.size_file = Some(size_file);
    let _ = eframe::run_native(
        title,
        options,
        Box::new(move |cc| {
            fonts(&cc.egui_ctx);
            let _ = CTX.set(cc.egui_ctx.clone());
            if let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) {
                frameless(HWND(h.hwnd.get() as _), true);
            }
            Ok(Box::new(frame))
        }),
    );
}

/// The page (index into [`Pages::pages`]) a key picks, as the sidebar numbers them: 1–9, and 0 for
/// the tenth; numpad digits are the same keys. Ctrl+digit always picks one, a bare digit only while
/// no text field is `typing`; neither while something `held` takes keys (a key being recorded, a
/// dialog or popup).
fn digit_page(key: Key, mods: Modifiers, count: usize, typing: bool, held: bool) -> Option<usize> {
    const DIGITS: [Key; 10] =
        [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9, Key::Num0];
    let page = DIGITS.iter().position(|&d| d == key).filter(|&n| n < count)?;
    let ok = !held && ((mods.ctrl && !mods.alt && !mods.shift) || (mods.is_none() && !typing));
    ok.then_some(page)
}

/// The window around the pages.
pub struct Frame<P: Pages> {
    pub pages: P,
    page: Option<P::Page>,
    /// Closing with unsaved changes: the "save changes?" dialog is up.
    asking: bool,
    /// A dialog or popup was up last frame: digits are its, not the sidebar's.
    overlaid: bool,
    /// The last input was keyboard navigation, so focus rings show.
    keyboard: bool,
    /// The theme and accent the egui visuals were set up for.
    styled: Option<(bool, [u8; 3])>,
    /// Closing was confirmed (or there was nothing to lose): let the close through.
    closing: bool,
    shown: Option<P::Page>,
    /// Holds the page at the top until the user does something.
    pinned: bool,
    /// The window's inner size in points when it opened, and now.
    size: ([f32; 2], [f32; 2]),
    size_file: Option<PathBuf>,
}

impl<P: Pages> Frame<P> {
    pub fn new(pages: P, size: ([f32; 2], [f32; 2])) -> Frame<P> {
        Frame {
            pages,
            page: None,
            asking: false,
            overlaid: false,
            keyboard: false,
            styled: None,
            closing: false,
            shown: None,
            pinned: false,
            size,
            size_file: None,
        }
    }

    /// The page shown.
    pub fn page(&self) -> P::Page {
        self.page.unwrap_or(self.pages.pages()[0])
    }

    pub fn go(&mut self, page: P::Page) {
        self.pages.leave();
        self.page = Some(page);
    }

    /// The accent the visuals were last built for.
    pub fn styled_accent(&self) -> Option<[u8; 3]> {
        self.styled.map(|s| s.1)
    }

    /// The "save changes?" dialog is up.
    pub fn asking(&self) -> bool {
        self.asking
    }

    pub fn closing(&self) -> bool {
        self.closing
    }

    fn changes(&self) -> usize {
        self.pages.pages().iter().map(|&p| self.pages.changes(p)).sum()
    }

    fn can_save(&self) -> bool {
        self.changes() > 0 && self.pages.can_save()
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Closes, or asks first when there are unsaved changes.
    fn request_close(&mut self, ctx: &egui::Context) {
        if self.changes() > 0 {
            self.asking = true;
        } else {
            self.close(ctx);
        }
    }
}

impl<P: Pages> Drop for Frame<P> {
    fn drop(&mut self) {
        self.pages.leave();
        let (opened, now) = self.size;
        if let Some(file) = &self.size_file
            && now != opened
        {
            let _ = std::fs::write(file, format!("[{}, {}]", now[0].round(), now[1].round()));
        }
    }
}

impl<P: Pages> eframe::App for Frame<P> {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::window::closing();
    }

    /// Past the window's painted corners is the desktop.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.pages.tick();
        let ctx = ui.ctx().clone();
        window::shots(&ctx);
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let accent = self.pages.accent();
        let p = widgets::palette(light, accent);
        if self.styled != Some((light, accent)) {
            self.styled = Some((light, accent));
            visuals(&ctx, &p, light);
        }
        ctx.input(|i| {
            if i.pointer.any_pressed() {
                self.keyboard = false;
            } else if [Key::Tab, Key::ArrowLeft, Key::ArrowRight, Key::ArrowUp, Key::ArrowDown]
                .into_iter()
                .any(|k| i.key_pressed(k))
            {
                self.keyboard = true;
            }
        });
        // A digit picks a page as clicking it would; checked before the page shows, which may take
        // the key itself (a toolbar chip being bound).
        let held = self.pages.busy() || self.overlaid || self.asking || egui::Popup::is_any_open(&ctx);
        let typing = ctx.text_edit_focused();
        let count = self.pages.pages().len();
        let picked = ctx.input(|i| {
            i.events.iter().find_map(|e| match *e {
                Event::Key { key, modifiers, pressed: true, repeat: false, .. } => digit_page(key, modifiers, count, typing, held),
                _ => None,
            })
        });
        if let Some(n) = picked
            && self.pages.pages()[n] != self.page()
        {
            self.go(self.pages.pages()[n]);
        }
        let viewport = ctx.input(|i| (i.viewport().inner_rect, i.viewport().maximized, i.viewport().minimized));
        if let (Some(r), Some(false) | None, Some(false) | None) = viewport
            && r.width() >= WINDOW[0] - 1.0
            && r.height() >= WINDOW[1] - 1.0
        {
            self.size.1 = [r.width(), r.height()];
        }
        let look = Look { p, keyboard: self.keyboard };
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let active = widgets::anim(ui, Id::unique("window-active"), focused, 0.15);
        let radius = window::radius(&ctx);
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let mut had_overlay = self.asking || egui::Popup::is_any_open(&ctx);

        // The bar and the sidebar share this fill: one darker L-shaped frame around the panel.
        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.side);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, P::APP, "settings", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => self.request_close(&ctx),
            TitleAction::None => {}
        }

        let pages = self.pages.pages();
        let page = self.page();
        let side = Rect::from_min_max(pos2(full.left(), bar.bottom()), pos2(full.left() + SIDEBAR_W, full.bottom()));
        let item = |i: f32| Rect::from_min_size(pos2(side.left() + 8.0, side.top() + 6.0 + i * 34.0), vec2(SIDEBAR_W - 16.0, 32.0));
        let current = pages.iter().position(|&s| s == page).unwrap_or(0);
        let pos = widgets::glide(ui, Id::unique("nav-selection"), current as f32, 0.18);
        ui.painter().rect_filled(item(pos), 5, p.sel);
        for (i, &s) in pages.iter().enumerate() {
            let near = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
            let dirty = self.pages.changes(s) > 0;
            let name = self.pages.name(s);
            if widgets::nav_item(ui, &look, item(i as f32), i + 1, name, self.pages.badge(s), (i == current, near), dirty).clicked() {
                self.go(s);
            }
        }
        if let Some(updater) = self.pages.updater() {
            updates::sidebar_button(ui, &look, side, updater);
        }
        let page = self.page();

        let main = Rect::from_min_max(pos2(side.right(), side.top()), full.right_bottom());
        // Rounded where it meets the frame; its last corner is the window's own.
        let corners = CornerRadius { nw: PANEL_RADIUS, ne: PANEL_RADIUS, sw: PANEL_RADIUS, se: radius };
        ui.painter().rect_filled(main, corners, p.surf);
        let body = Rect::from_min_max(
            pos2(main.left() + CONTENT_X, main.top()),
            pos2(main.right() - CONTENT_X, main.bottom() - FOOTER_H),
        );
        if self.shown != Some(page) {
            // a newly picked page drops in; the first one is simply there
            if self.shown.is_some() {
                widgets::snap(&ctx, Id::unique("section-enter"), 0.0);
            }
            self.shown = Some(page);
            self.pinned = true;
        }
        let enter = widgets::glide(ui, Id::unique("section-enter"), 1.0, PAGE_ENTER);
        let lift = (1.0 - enter) * PAGE_DROP;
        let mut heading = ui.painter().clone();
        heading.multiply_opacity(enter);
        let name = self.pages.name(page);
        let title = heading.text(pos2(body.left(), body.top() + 18.0 + lift), Align2::LEFT_TOP, name, semibold(20.0), p.t1);
        if let Some(b) = self.pages.badge(page) {
            widgets::paint_badge(&heading, &look, pos2(title.right() + 8.0, title.center().y), b);
        }
        let rows = Rect::from_min_max(pos2(body.left(), body.top() + 56.0), pos2(main.right() - 10.0, body.bottom()));
        let gutter = (rows.right() - body.right()) as i8;
        let mut content = ui.new_child(UiBuilder::new().max_rect(rows).layout(Layout::top_down(Align::Min)));
        // observed on the user's machine: a fresh window opened scrolled to the bottom with no input
        let touched = ctx.input(|i| {
            i.pointer.any_pressed()
                || i.pointer.delta() != egui::Vec2::ZERO
                || i.events.iter().any(|e| {
                    matches!(e, egui::Event::Key { pressed: true, .. } | egui::Event::AccessKitActionRequest(_))
                })
        });
        self.pinned &= !touched;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(name)
            .auto_shrink(false)
            .content_margin(Margin { right: gutter, ..Margin::ZERO });
        if self.pinned {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(&mut content, |content| {
            content.spacing_mut().item_spacing = vec2(8.0, 0.0);
            content.multiply_opacity(enter);
            content.add_space(lift);
            widgets::reveal_with(content, Id::unique("save-error"), self.pages.error(), |ui, e| {
                hint(ui, e, p.warn);
                ui.add_space(12.0);
            });
            self.pages.show(page, content, &look);
        });
        self.footer(ui, &look, Rect::from_min_max(pos2(main.left(), body.bottom()), main.right_bottom()));
        window::border(ui, full, p.line2);
        had_overlay |= self.pages.overlays(&ctx, &look);
        self.ask(&ctx, &look);
        self.overlaid = had_overlay;

        // Alt+F4 and the taskbar's "close window" come through here.
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing && self.changes() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.asking = true;
        }
        if esc && !had_overlay && !self.pages.busy() {
            self.request_close(&ctx);
        }
    }
}

impl<P: Pages> Frame<P> {
    fn footer(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let ctx = ui.ctx().clone();
        ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, look.p.line));
        let inner = Rect::from_min_max(pos2(rect.left() + 20.0, rect.top() + 1.0), pos2(rect.right() - 14.0, rect.bottom()));
        let mut ui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        let page = self.page();
        if let Some(differs) = self.pages.resettable(page) {
            let label = format!("reset {}", self.pages.name(page));
            if widgets::button(&mut ui, look, &label, Kind::Text, 30.0, differs).clicked() {
                self.pages.leave();
                self.pages.reset(page);
            }
        }
        let changes = self.changes();
        let can_save = self.can_save();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, look, "save", Kind::Primary, 30.0, can_save).clicked() {
                self.pages.save();
            }
            // With changes this discards them; without, there's nothing to lose.
            let label = if changes > 0 { "cancel" } else { "close" };
            if widgets::button(ui, look, label, Kind::Secondary, 30.0, true).clicked() {
                if changes > 0 {
                    self.pages.leave();
                    self.pages.revert();
                } else {
                    self.close(&ctx);
                }
            }
            // fades out still saying how many there were
            let count = Id::unique("unsaved-count");
            if changes > 0 {
                ui.data_mut(|d| d.insert_temp(count, changes));
            }
            let shown = widgets::anim(ui, Id::unique("unsaved-label"), changes > 0, 0.15);
            if shown > 0.0 {
                let n = ui.data(|d| d.get_temp::<usize>(count)).unwrap_or(changes);
                ui.add_space(6.0);
                let s = if n == 1 { "" } else { "s" };
                let color = look.p.t3.gamma_multiply(widgets::ease(shown));
                ui.label(RichText::new(format!("{n} unsaved change{s}")).font(sans(12.0)).color(color));
            }
        });
    }

    /// The "save changes?" dialog, while closing with unsaved changes.
    fn ask(&mut self, ctx: &egui::Context, look: &Look) {
        if !self.asking {
            return;
        }
        let p = look.p;
        let resp = dialog(ctx, look).show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.set_width(340.0);
            ui.label(RichText::new("save changes?").font(semibold(15.0)).color(p.t1));
            let n = self.changes();
            let text = format!("you have {n} unsaved change{} in {}'s settings.", if n == 1 { "" } else { "s" }, P::APP);
            ui.label(RichText::new(text).font(sans(13.0)).color(p.t2));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if widgets::button(ui, look, "keep editing", Kind::Text, 30.0, true).clicked() {
                    self.asking = false;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let valid = self.pages.can_save();
                    if widgets::button(ui, look, "save", Kind::Primary, 30.0, valid).clicked() && self.pages.save() {
                        self.close(ctx);
                    }
                    if widgets::button(ui, look, "discard", Kind::Secondary, 30.0, true).clicked() {
                        self.close(ctx);
                    }
                });
            });
        });
        if resp.should_close() {
            self.asking = false;
        }
    }
}

/// The open-source licences dialog; `crates` names the Rust crates the app uses. False once it's
/// closed.
pub fn licences(ctx: &egui::Context, look: &Look, crates: &str) -> bool {
    let p = look.p;
    let mut open = true;
    let resp = dialog(ctx, look).show(ctx, |ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        ui.set_width(460.0);
        let body = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).font(sans(13.0)).color(p.t2));
        ui.label(RichText::new("open-source licences").font(semibold(15.0)).color(p.t1));
        body(ui, "ibm plex sans and ibm plex mono, © 2017 ibm corp., under the sil open font license 1.1:");
        egui::ScrollArea::vertical().max_height(250.0).show(ui, |ui| {
            ui.label(RichText::new(FONT_LICENSE).font(mono(11.0)).color(p.t3));
        });
        body(ui, &format!("rust crates ({crates} and their dependencies), by their authors, under permissive licences, mostly mit or apache-2.0."));
        ui.add_space(4.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, look, "close", Kind::Secondary, 30.0, true).clicked() {
                open = false;
            }
        });
    });
    open && !resp.should_close()
}

/// A modal in the window's look: the app's own dialogs use it too.
pub fn dialog(ctx: &egui::Context, look: &Look) -> egui::Modal {
    let p = look.p;
    let frame = egui::Frame::new()
        .fill(p.surf)
        .stroke(Stroke::new(1.0, p.line2))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(20))
        .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(80) });
    let modal = egui::Modal::new(Id::unique("dialog")).frame(frame);
    window::backdrop(ctx, modal, Color32::from_black_alpha(90))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_pick_pages_unless_a_field_or_a_recording_has_the_keys() {
        // what egui-winit reports for ctrl on Windows
        let (none, ctrl) = (Modifiers::NONE, Modifiers::CTRL | Modifiers::COMMAND);
        let pick = |key, mods, typing, held| digit_page(key, mods, 10, typing, held);
        assert_eq!(pick(Key::Num1, none, false, false), Some(0));
        assert_eq!(pick(Key::Num3, none, false, false), Some(2));
        assert_eq!(pick(Key::Num9, none, false, false), Some(8));
        assert_eq!(pick(Key::Num0, none, false, false), Some(9), "0 is the tenth");
        assert_eq!(pick(Key::Num5, ctrl, false, false), Some(4));
        // typing a number into a field
        assert_eq!(pick(Key::Num3, none, true, false), None);
        assert_eq!(pick(Key::Num5, ctrl, true, false), Some(4), "ctrl+digit works from a field");
        // a key being recorded, a dialog or a popup
        assert_eq!(pick(Key::Num3, none, false, true), None);
        assert_eq!(pick(Key::Num5, ctrl, false, true), None);
        // other chords are left alone: shift+3 is #, ctrl+alt is AltGr
        for mods in [Modifiers::SHIFT, Modifiers::ALT, ctrl | Modifiers::SHIFT, ctrl | Modifiers::ALT] {
            assert_eq!(pick(Key::Num3, mods, false, false), None, "{mods:?}");
        }
        assert_eq!(pick(Key::A, none, false, false), None);
        // only pages that are there
        assert_eq!(digit_page(Key::Num0, none, 9, false, false), None);
        assert_eq!(digit_page(Key::Num7, none, 6, false, false), None);
        assert_eq!(digit_page(Key::Num6, none, 6, false, false), Some(5));
    }
}
