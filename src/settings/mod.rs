//! Settings window, run as `glint.exe --settings`: a separate short-lived process, because
//! winit allows only one event loop per process and the window can be opened many times.
//! It edits config.toml and asks the tray app to apply it.
//!
//! The window draws its own title bar (see [`frameless`]) and follows the Windows light / dark
//! app setting.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Id, Key, Layout, Margin, Rect,
    RichText, Stroke, TextStyle, Ui, UiBuilder, pos2, vec2,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

use crate::app::ipc::{self, SETTINGS_TITLE, WM_RELOAD};
use crate::app::tray::icon_rgba;
use crate::config::{Config, file_name_problem};
use crate::platform::paths::app_dir;

mod hotkey_capture;
mod sections;
pub(crate) mod widgets;
mod window;

pub use hotkey_capture::{CAPTURE_ARG, capture_keys};
use hotkey_capture::{HotkeyField, Listener};
use sections::uploads::Uploaders;
use sections::{SECTIONS, Section};
use widgets::{
    BAR_H, CONTENT_X, FOOTER_H, Kind, Look, SIDEBAR_W, TitleAction, WINDOW, hint, medium, mono, sans, semibold,
};
pub(crate) use window::frameless;

/// IBM Plex Sans and Mono, unmodified (SIL Open Font License, see assets/fonts/OFL.txt).
const FONTS: [(&str, &[u8]); 5] = [
    ("sans", include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf")),
    ("sans-medium", include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf")),
    ("sans-semibold", include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf")),
    ("mono", include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")),
    ("mono-medium", include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf")),
];
const FONT_LICENSE: &str = include_str!("../../assets/fonts/OFL.txt");

/// Where the settings window's last size is kept, in points: `[width, height]` as JSON.
fn size_path() -> PathBuf {
    app_dir().join("settings-window.json")
}

fn saved_size() -> [f32; 2] {
    let read = std::fs::read_to_string(size_path()).ok().and_then(|t| serde_json::from_str::<[f32; 2]>(&t).ok());
    let [w, h] = read.unwrap_or(WINDOW);
    [w.clamp(WINDOW[0], 8000.0), h.clamp(WINDOW[1], 8000.0)]
}

pub fn run() {
    if let Some(h) = ipc::find_settings() {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let (cfg, load_error) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("couldn't read the settings, showing defaults: {e}"))),
    };
    let size = saved_size();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(SETTINGS_TITLE)
            .with_inner_size(size)
            .with_min_inner_size(WINDOW)
            .with_resizable(true)
            .with_decorations(false)
            .with_icon(egui::IconData { rgba: icon_rgba(64, cfg.accent_rgb()), width: 64, height: 64 }),
        centered: true,
        ..Default::default()
    };
    let app = SettingsApp {
        saved: cfg.clone(),
        accent_text: cfg.accent.clone(),
        cfg,
        error: load_error,
        listening: None,
        listen_for: HotkeyField::Record,
        hotkey_error: None,
        section: Section::General,
        dialog: None,
        keyboard: false,
        rounded: false,
        styled: None,
        closing: false,
        uploaders: Uploaders::load(),
        audio_devices: None,
        shown: None,
        pinned: false,
        size: (size, size),
    };
    let _ = eframe::run_native(
        SETTINGS_TITLE,
        options,
        Box::new(|cc| {
            fonts(&cc.egui_ctx);
            let _ = CTX.set(cc.egui_ctx.clone());
            let mut app = app;
            if let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) {
                app.rounded = frameless(HWND(h.hwnd.get() as _), true);
            }
            Ok(Box::new(app))
        }),
    );
}

#[derive(Clone, Copy, PartialEq)]
enum Dialog {
    /// Closing with unsaved changes.
    SaveChanges,
    Licences,
}

struct SettingsApp {
    /// What is on disk.
    saved: Config,
    /// What is being edited.
    cfg: Config,
    error: Option<String>,
    /// The keyboard hook's thread while a new hotkey is being recorded, and for which field.
    listening: Option<Listener>,
    listen_for: HotkeyField,
    /// Keys and reason of the last combo that can't be used; listening goes on.
    hotkey_error: Option<(Vec<String>, String)>,
    section: Section,
    dialog: Option<Dialog>,
    /// The last input was keyboard navigation, so focus rings show.
    keyboard: bool,
    /// Windows 11 rounds the window corners.
    rounded: bool,
    /// The theme and accent the egui visuals were set up for.
    styled: Option<(bool, [u8; 3])>,
    /// Closing was confirmed (or there was nothing to lose): let the close through.
    closing: bool,
    uploaders: Uploaders,
    audio_devices: Option<sections::audio::Devices>,
    shown: Option<Section>,
    pinned: bool,
    /// What's typed in the custom accent field; `cfg.accent` follows it while it's a colour.
    accent_text: String,
    /// The window's inner size in points when it opened, and now.
    size: ([f32; 2], [f32; 2]),
}

static CTX: OnceLock<egui::Context> = OnceLock::new();

fn repaint() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

impl SettingsApp {
    fn changes(&self) -> usize {
        SECTIONS.iter().map(|s| s.changes(&self.cfg, &self.saved)).sum()
    }

    fn can_save(&self) -> bool {
        self.changes() > 0 && self.listening.is_none() && file_name_problem(&self.cfg.file_name).is_none()
    }

    /// Writes config.toml and tells the tray app; returns whether it worked.
    fn save(&mut self) -> bool {
        let dir = self.cfg.output_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("can't use {}: {e}", dir.display()));
            return false;
        }
        match self.cfg.save() {
            Ok(()) => {
                ipc::post_to_main(WM_RELOAD, 0);
                self.saved = self.cfg.clone();
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Closes, or asks first when there are unsaved changes.
    fn request_close(&mut self, ctx: &egui::Context) {
        if self.changes() > 0 {
            self.dialog = Some(Dialog::SaveChanges);
        } else {
            self.close(ctx);
        }
    }

    fn go(&mut self, section: Section) {
        self.stop_listening();
        self.section = section;
    }
}

impl Drop for SettingsApp {
    fn drop(&mut self) {
        self.stop_listening();
        let (opened, now) = self.size;
        if now != opened {
            let _ = std::fs::write(size_path(), format!("[{}, {}]", now[0].round(), now[1].round()));
        }
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll_capture();
        let ctx = ui.ctx().clone();
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let accent = self.cfg.accent_rgb();
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
        let radius: u8 = if self.rounded { 8 } else { 0 };
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let had_overlay = self.dialog.is_some() || egui::Popup::is_any_open(&ctx);

        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.surf);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, "settings", active, radius) {
            TitleAction::Minimize => ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
            TitleAction::Close => self.request_close(&ctx),
            TitleAction::None => {}
        }

        // Sidebar: shares the bar's fill, so the two read as one L-shaped frame.
        let side = Rect::from_min_max(pos2(full.left(), bar.bottom()), pos2(full.left() + SIDEBAR_W, full.bottom()));
        ui.painter().rect_filled(side, CornerRadius { nw: 0, ne: 0, sw: radius, se: 0 }, p.side);
        let item = |i: f32| Rect::from_min_size(pos2(side.left() + 8.0, side.top() + 6.0 + i * 34.0), vec2(SIDEBAR_W - 16.0, 32.0));
        let current = SECTIONS.iter().position(|&s| s == self.section).unwrap_or(0);
        let pos = widgets::glide(ui, Id::unique("nav-selection"), current as f32, 0.18);
        ui.painter().rect_filled(item(pos), 5, p.sel);
        for (i, &s) in SECTIONS.iter().enumerate() {
            let near = (1.0 - (pos - i as f32).abs()).clamp(0.0, 1.0);
            let dirty = s.changes(&self.cfg, &self.saved) > 0;
            if widgets::nav_item(ui, &look, item(i as f32), i + 1, s.name(), (i == current, near), dirty).clicked() {
                self.go(s);
            }
        }

        let main = Rect::from_min_max(pos2(side.right(), side.top()), full.right_bottom());
        let body = Rect::from_min_max(
            pos2(main.left() + CONTENT_X, main.top()),
            pos2(main.right() - CONTENT_X, main.bottom() - FOOTER_H),
        );
        ui.painter().text(pos2(body.left(), body.top() + 18.0), Align2::LEFT_TOP, self.section.name(), semibold(20.0), p.t1);
        let rows = Rect::from_min_max(pos2(body.left(), body.top() + 56.0), pos2(main.right() - 10.0, body.bottom()));
        let gutter = (rows.right() - body.right()) as i8;
        let mut content = ui.new_child(UiBuilder::new().max_rect(rows).layout(Layout::top_down(Align::Min)));
        if self.shown != Some(self.section) {
            self.shown = Some(self.section);
            self.pinned = true;
        }
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
            .id_salt(self.section.name())
            .auto_shrink(false)
            .content_margin(Margin { right: gutter, ..Margin::ZERO });
        if self.pinned {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll.show(&mut content, |content| {
            content.spacing_mut().item_spacing = vec2(8.0, 0.0);
            if let Some(e) = &self.error {
                hint(content, e.clone(), p.warn);
                content.add_space(12.0);
            }
            match self.section {
                Section::General => self.general(content, &look),
                Section::Recording => self.recording(content, &look),
                Section::Audio => self.audio(content, &look),
                Section::Screenshots => self.screenshots(content, &look),
                Section::Uploads => self.uploads(content, &look),
                Section::Output => self.output(content, &look),
                Section::About => self.about(content, &look),
            }
        });
        self.footer(ui, &look, Rect::from_min_max(pos2(main.left(), body.bottom()), main.right_bottom()));
        self.dialogs(&ctx, &look);

        // Alt+F4 and the taskbar's "close window" come through here.
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing && self.changes() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::SaveChanges);
        }
        if esc && !had_overlay && self.listening.is_none() {
            self.request_close(&ctx);
        }
    }
}

impl SettingsApp {
    fn footer(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let ctx = ui.ctx().clone();
        ui.painter().hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, look.p.line));
        let inner = Rect::from_min_max(pos2(rect.left() + 20.0, rect.top() + 1.0), pos2(rect.right() - 14.0, rect.bottom()));
        let mut ui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)));
        ui.spacing_mut().item_spacing = vec2(8.0, 0.0);
        if self.section != Section::About {
            let label = format!("reset {}", self.section.name());
            let differs = self.section.changes(&self.cfg, &Config::default()) > 0;
            if widgets::button(&mut ui, look, &label, Kind::Text, 30.0, differs).clicked() {
                self.stop_listening();
                self.section.reset(&mut self.cfg);
            }
        }
        let changes = self.changes();
        let can_save = self.can_save();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if widgets::button(ui, look, "save", Kind::Primary, 30.0, can_save).clicked() {
                self.save();
            }
            // With changes this discards them; without, there's nothing to lose.
            let label = if changes > 0 { "cancel" } else { "close" };
            if widgets::button(ui, look, label, Kind::Secondary, 30.0, true).clicked() {
                if changes > 0 {
                    self.stop_listening();
                    self.cfg = self.saved.clone();
                } else {
                    self.close(&ctx);
                }
            }
            if changes > 0 {
                ui.add_space(6.0);
                let s = if changes == 1 { "" } else { "s" };
                ui.label(RichText::new(format!("{changes} unsaved change{s}")).font(sans(12.0)).color(look.p.t3));
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, look: &Look) {
        let Some(dialog) = self.dialog else { return };
        let p = look.p;
        let frame = egui::Frame::new()
            .fill(p.surf)
            .stroke(Stroke::new(1.0, p.line2))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(20))
            .shadow(egui::epaint::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(80) });
        let modal = egui::Modal::new(Id::unique("dialog")).frame(frame).backdrop_color(Color32::from_black_alpha(90));
        let resp = modal.show(ctx, |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            let title = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).font(semibold(15.0)).color(p.t1));
            let body = |ui: &mut Ui, text: &str| ui.label(RichText::new(text).font(sans(13.0)).color(p.t2));
            match dialog {
                Dialog::SaveChanges => {
                    ui.set_width(340.0);
                    title(ui, "save changes?");
                    let n = self.changes();
                    body(ui, &format!("you have {n} unsaved change{} in glint's settings.", if n == 1 { "" } else { "s" }));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if widgets::button(ui, look, "keep editing", Kind::Text, 30.0, true).clicked() {
                            self.dialog = None;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let valid = file_name_problem(&self.cfg.file_name).is_none();
                            if widgets::button(ui, look, "save", Kind::Primary, 30.0, valid).clicked() && self.save() {
                                self.close(ctx);
                            }
                            if widgets::button(ui, look, "discard", Kind::Secondary, 30.0, true).clicked() {
                                self.close(ctx);
                            }
                        });
                    });
                }
                Dialog::Licences => {
                    ui.set_width(460.0);
                    title(ui, "open-source licences");
                    body(ui, "ibm plex sans and ibm plex mono, © 2017 ibm corp., under the sil open font license 1.1:");
                    egui::ScrollArea::vertical().max_height(250.0).show(ui, |ui| {
                        ui.label(RichText::new(FONT_LICENSE).font(mono(11.0)).color(p.t3));
                    });
                    body(
                        ui,
                        "rust crates (egui, eframe, winit, windows-rs, serde, toml, gif, color_quant and their dependencies), \
                         by their authors, under permissive licences, mostly mit or apache-2.0.",
                    );
                    ui.add_space(4.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::button(ui, look, "close", Kind::Secondary, 30.0, true).clicked() {
                            self.dialog = None;
                        }
                    });
                }
            }
        });
        if resp.should_close() && self.dialog == Some(dialog) {
            self.dialog = None;
        }
    }
}

pub(crate) fn fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, bytes) in FONTS {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    // Segoe UI covers what Plex doesn't (other scripts in folder names, for instance).
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let segoe = std::fs::read(Path::new(&windir).join("Fonts").join("segoeui.ttf")).ok();
    let mut fallback: Vec<String> = Vec::new();
    if let Some(bytes) = segoe {
        fonts.font_data.insert("segoe".into(), Arc::new(FontData::from_owned(bytes)));
        fallback.push("segoe".into());
    }
    fallback.extend(defaults);
    let families = [
        (FontFamily::Proportional, "sans"),
        (widgets::MEDIUM.clone(), "sans-medium"),
        (widgets::SEMIBOLD.clone(), "sans-semibold"),
        (FontFamily::Monospace, "mono"),
        (widgets::MONO_MEDIUM.clone(), "mono-medium"),
    ];
    for (family, first) in families {
        let chain = std::iter::once(first.to_string()).chain(fallback.iter().cloned()).collect();
        fonts.families.insert(family, chain);
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        s.interaction.selectable_labels = false;
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.scroll = egui::style::ScrollStyle {
            bar_width: 6.0,
            floating_width: 6.0,
            handle_min_length: 24.0,
            foreground_color: false,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            dormant_handle_opacity: 1.0,
            active_handle_opacity: 1.0,
            interact_handle_opacity: 1.0,
            ..egui::style::ScrollStyle::floating()
        };
        s.text_styles.insert(TextStyle::Body, sans(13.0));
        s.text_styles.insert(TextStyle::Button, medium(13.0));
        s.text_styles.insert(TextStyle::Small, sans(12.0));
        s.text_styles.insert(TextStyle::Monospace, mono(12.0));
    });
}

/// egui's own widgets (text fields, scroll bars) in the palette's colours.
pub(crate) fn visuals(ctx: &egui::Context, p: &widgets::Palette, light: bool) {
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = p.surf;
    v.window_fill = p.surf;
    v.extreme_bg_color = p.field;
    v.selection.bg_fill = p.acc.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, p.t1);
    v.text_cursor.stroke = Stroke::new(1.5, p.t1);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.fg_stroke.color = p.t1;
    }
    // scroll handles are painted in these fills
    for (w, fill) in [(&mut v.widgets.inactive, p.line2), (&mut v.widgets.hovered, p.line3), (&mut v.widgets.active, p.t3)] {
        w.bg_fill = fill;
        w.corner_radius = CornerRadius::same(3);
    }
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.set_visuals(v);
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App;

    const TOP: f32 = 118.0;

    struct Harness {
        ctx: egui::Context,
        app: SettingsApp,
        frame: eframe::Frame,
        time: f64,
        screen: egui::Vec2,
        tree: Vec<(egui::accesskit::NodeId, egui::accesskit::Node)>,
    }

    impl Harness {
        fn new() -> Harness {
            let ctx = egui::Context::default();
            fonts(&ctx);
            let cfg = Config::default();
            let app = SettingsApp {
                saved: cfg.clone(),
                cfg,
                error: None,
                listening: None,
                listen_for: HotkeyField::Record,
                hotkey_error: None,
                section: Section::General,
                dialog: None,
                keyboard: false,
                rounded: false,
                styled: None,
                closing: false,
                uploaders: Uploaders::load(),
                audio_devices: None,
                shown: None,
                pinned: false,
                accent_text: String::new(),
                size: (WINDOW, WINDOW),
            };
            let screen = vec2(WINDOW[0], WINDOW[1]);
            Harness { ctx, app, frame: eframe::Frame::_new_kittest(), time: 0.0, screen, tree: Vec::new() }
        }

        fn frame(&mut self, events: Vec<egui::Event>, label: &str) -> Option<f32> {
            self.time += 0.016;
            let input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), self.screen)),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (app, frame) = (&mut self.app, &mut self.frame);
            let mut out = self.ctx.run_ui(input, |ui| app.ui(ui, frame));
            self.tree = out.platform_output.accesskit_update.take().map(|u| u.nodes).unwrap_or_default();
            out.shapes.iter().find_map(|c| match &c.shape {
                egui::epaint::Shape::Text(t) if t.galley.text() == label => Some(t.pos.y),
                _ => None,
            })
        }

        fn settle(&mut self) {
            for _ in 0..12 {
                self.frame(Vec::new(), "");
            }
        }

        fn click(&mut self, pos: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(pos), button(pos, true)], "");
            self.frame(vec![button(pos, false)], "");
            self.settle();
        }

        fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
            self.frame(vec![egui::Event::PointerMoved(from), button(from, true)], "");
            for t in [0.25, 0.5, 0.75, 1.0] {
                self.frame(vec![egui::Event::PointerMoved(from.lerp(to, t))], "");
            }
            self.frame(vec![button(to, false)], "");
            self.settle();
        }

        fn control(&mut self, label: &str) -> Rect {
            widgets::ROWS.with_borrow_mut(Vec::clear);
            self.frame(Vec::new(), "");
            widgets::ROWS.take().into_iter().find(|(l, ..)| l == label).map(|(.., c)| c).expect(label)
        }
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    fn wheel() -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -2000.0),
            modifiers: Default::default(),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        }
    }

    #[test]
    fn sections_open_at_the_top() {
        let mut h = Harness::new();
        assert_eq!(h.frame(Vec::new(), "record hotkey"), Some(TOP));
        h.frame(vec![egui::Event::PointerMoved(pos2(400.0, 300.0))], "record hotkey");
        for _ in 0..20 {
            assert_eq!(h.frame(vec![wheel()], "record hotkey"), Some(TOP));
        }

        h.frame(vec![egui::Event::PointerMoved(pos2(420.0, 320.0))], "record hotkey");
        for _ in 0..20 {
            h.frame(vec![wheel()], "record hotkey");
        }
        assert!(h.frame(Vec::new(), "record hotkey").is_none_or(|y| y < TOP));

        h.app.go(Section::Recording);
        assert_eq!(h.frame(Vec::new(), "frame rate"), Some(TOP));
        h.app.go(Section::General);
        assert_eq!(h.frame(Vec::new(), "record hotkey"), Some(TOP));
    }

    #[test]
    fn row_labels_clear_their_controls() {
        use crate::config::audio::{AudioSource, SourceKind};
        for width in [WINDOW[0], 760.0, 1200.0] {
            let mut h = Harness::new();
            h.screen = vec2(width, WINDOW[1]);
            h.app.cfg.upload_destination = "litterbox".into();
            h.app.cfg.audio_sources =
                [SourceKind::Desktop, SourceKind::App, SourceKind::Mic].iter().enumerate().map(|(i, &k)| AudioSource::new(k, i)).collect();
            for section in SECTIONS {
                h.app.go(section);
                for _ in 0..40 {
                    h.frame(Vec::new(), "");
                }
                widgets::ROWS.with_borrow_mut(Vec::clear);
                h.frame(Vec::new(), "");
                let rows = widgets::ROWS.take();
                assert!(!rows.is_empty(), "{} has no rows", section.name());
                for (label, l, c) in rows {
                    let at = format!("{} › {label} at {width}: label {l:?}, control {c:?}", section.name());
                    assert!(l.right() < c.left(), "{at}");
                    assert!(c.right() <= width - CONTENT_X + 0.5, "{at}");
                }
            }
        }
    }

    #[test]
    fn accent_presets_keep_white_button_text() {
        for light in [false, true] {
            for rgb in sections::general::ACCENTS {
                assert_eq!(widgets::palette(light, rgb).on_acc, Color32::WHITE, "{rgb:?} light {light}");
            }
        }
    }

    #[test]
    fn custom_accent_picker_drags_live_and_esc_only_closes_it() {
        use crate::config::color::rgb_to_hsv;
        use sections::general::{ACCENTS, accent_popup_id};
        let mut h = Harness::new();
        h.frame(vec![egui::Event::PointerMoved(pos2(400.0, 300.0))], "");
        h.frame(vec![egui::Event::PointerMoved(pos2(420.0, 320.0))], "");
        for _ in 0..20 {
            h.frame(vec![wheel()], "");
        }
        let swatches = h.control("accent");
        let custom = pos2(swatches.left() + 8.0 * 30.0 + 12.0, swatches.top() + 12.0);
        h.click(custom);
        let popup = h.ctx.memory(|m| m.area_rect(accent_popup_id())).expect("the picker opens");
        assert!(Rect::from_min_size(pos2(0.0, 0.0), h.screen).contains_rect(popup), "{popup:?}");
        assert!(!popup.expand(4.0).contains(custom), "{popup:?}");

        let square = Rect::from_min_size(popup.min + vec2(13.0, 13.0), vec2(232.0, 148.0));
        h.click(square.lerp_inside(vec2(0.75, 0.25)));
        let [hue, s, v] = rgb_to_hsv(h.app.cfg.accent_rgb());
        let red = rgb_to_hsv(ACCENTS[0])[0];
        assert!((s - 0.75).abs() < 0.02 && (v - 0.75).abs() < 0.02 && (hue - red).abs() < 0.01, "{hue} {s} {v}");
        assert_eq!(h.app.cfg.accent_custom.as_deref(), Some(h.app.cfg.accent.as_str()));

        let strip = square.bottom() + 10.0 + 6.0;
        h.drag(pos2(square.left() + 10.0, strip), pos2(square.center().x, strip));
        let picked = h.app.cfg.accent_rgb();
        assert!((rgb_to_hsv(picked)[0] - 0.5).abs() < 0.01, "{picked:?}");
        assert_eq!(h.app.styled.map(|s| s.1), Some(picked));

        let esc = egui::Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        h.frame(vec![esc], "");
        h.settle();
        assert!(!egui::Popup::is_id_open(&h.ctx, accent_popup_id()));
        assert!(h.app.dialog.is_none() && !h.app.closing);
        assert_eq!(h.app.cfg.accent_rgb(), picked);

        h.click(pos2(swatches.left() + 12.0, swatches.top() + 12.0));
        assert_eq!((h.app.cfg.accent_rgb(), h.app.cfg.accent_custom_rgb()), (ACCENTS[0], Some(picked)));
        h.click(custom);
        assert_eq!(h.app.cfg.accent_rgb(), picked);
        assert!(egui::Popup::is_id_open(&h.ctx, accent_popup_id()));
        h.click(pos2(600.0, 70.0));
        assert!(!egui::Popup::is_id_open(&h.ctx, accent_popup_id()));
        assert_eq!(h.app.cfg.accent_rgb(), picked);
    }

    #[test]
    fn every_section_reports_its_controls_to_accessibility_tools() {
        use egui::accesskit::{Node, Role, Toggled};
        use sections::general::ACCENTS;
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.app.cfg.upload_destination = "litterbox".into();
        let named = |tree: &[(egui::accesskit::NodeId, Node)], role: Role, name: &str| {
            tree.iter().filter(|(_, n)| n.role() == role && n.label() == Some(name)).map(|(_, n)| n.clone()).collect::<Vec<_>>()
        };
        let mut counts = Vec::new();
        for section in SECTIONS {
            h.app.go(section);
            h.settle();
            let tree = std::mem::take(&mut h.tree);
            for (_, n) in &tree {
                let input = matches!(
                    n.role(),
                    Role::Button | Role::CheckBox | Role::RadioButton | Role::Slider | Role::ComboBox | Role::TextInput | Role::Tab | Role::Link
                );
                let name = n.label().or(n.placeholder()).unwrap_or("");
                assert!(!input || !name.trim().is_empty(), "{}: unnamed {:?} {n:?}", section.name(), n.role());
            }
            for name in ["minimize window", "close window", "save"] {
                assert_eq!(named(&tree, Role::Button, name).len(), 1, "{} › {name}", section.name());
            }
            let tabs = named(&tree, Role::Tab, section.name());
            assert!(tabs.len() == 1 && tabs[0].toggled() == Some(Toggled::True), "{}", section.name());
            let groups = tree.iter().filter(|(_, n)| n.role() == Role::Group && n.bounds().is_some()).count();
            assert!(groups >= 2, "{} has {groups} rows", section.name());
            counts.push(format!("{} {} nodes, {groups} rows", section.name(), tree.len()));

            if section == Section::General {
                let hotkey = named(&tree, Role::Button, "record hotkey");
                assert_eq!(hotkey.len(), 1);
                assert_eq!(hotkey[0].value(), Some("ctrl+alt+printscreen"));
                let labels = tree.iter().filter(|(_, n)| n.role() == Role::Label && n.value() == Some("record hotkey"));
                assert_eq!(labels.count(), 1);
                let startup = named(&tree, Role::CheckBox, "launch at startup");
                assert!(startup.len() == 1 && startup[0].toggled() == Some(Toggled::False), "{startup:?}");
                assert_eq!(startup[0].description(), Some("off"));
                let notifications = named(&tree, Role::RadioGroup, "notifications");
                assert_eq!(notifications.len(), 1);
                let options: Vec<_> = notifications[0]
                    .children()
                    .iter()
                    .filter_map(|id| tree.iter().find(|(i, _)| i == id))
                    .map(|(_, n)| (n.role(), n.label().unwrap_or("").to_owned(), n.toggled()))
                    .collect();
                assert_eq!(
                    options,
                    [
                        (Role::RadioButton, "all".to_owned(), Some(Toggled::True)),
                        (Role::RadioButton, "errors only".to_owned(), Some(Toggled::False)),
                    ]
                );
                let swatches = tree.iter().filter(|(_, n)| n.role() == Role::RadioButton && n.label().is_some_and(|l| l.starts_with('#')));
                assert_eq!(swatches.count(), ACCENTS.len());
                assert_eq!(named(&tree, Role::RadioButton, "custom colour").len(), 1);
            }
            if section == Section::Recording {
                let quality = named(&tree, Role::Slider, "quality");
                assert!(quality.len() == 1 && quality[0].numeric_value() == Some(h.app.cfg.quality as f64), "{quality:?}");
            }
        }
        eprintln!("{}", counts.join("\n"));
    }
}

