//! Settings window, run as `glint.exe --settings`: a separate short-lived process, because
//! winit allows only one event loop per process and the window can be opened many times.
//! It edits config.toml and asks the tray app to apply it.
//!
//! The window draws its own title bar (egui-winit keeps the DWM shadow for undecorated
//! windows) and follows the Windows light / dark app setting.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Id, Key, Layout, Rect, RichText,
    Stroke, TextStyle, Ui, UiBuilder, pos2, vec2,
};
use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;
use windows::core::w;

use crate::app::ipc::{self, SETTINGS_TITLE, WM_RELOAD};
use crate::app::tray::icon_pixels;
use crate::config::{Config, file_name_problem};
use crate::platform::wide::from_wstr;

mod hotkey_capture;
mod sections;
mod widgets;

pub use hotkey_capture::{CAPTURE_ARG, capture_keys};
use hotkey_capture::{HotkeyField, Listener};
use sections::{SECTIONS, Section};
use widgets::{
    BAR_H, CONTENT_X, FOOTER_H, Kind, Look, SIDEBAR_W, TitleAction, WINDOW, hint, medium, mono, sans, semibold,
};

/// IBM Plex Sans and Mono, unmodified (SIL Open Font License, see assets/fonts/OFL.txt).
const FONTS: [(&str, &[u8]); 5] = [
    ("sans", include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf")),
    ("sans-medium", include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf")),
    ("sans-semibold", include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf")),
    ("mono", include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf")),
    ("mono-medium", include_bytes!("../../assets/fonts/IBMPlexMono-Medium.ttf")),
];
const FONT_LICENSE: &str = include_str!("../../assets/fonts/OFL.txt");

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
    let size = 64;
    let rgba = icon_pixels(size, true)
        .into_iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8])
        .collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(SETTINGS_TITLE)
            .with_inner_size(WINDOW)
            .with_resizable(false)
            .with_decorations(false)
            .with_icon(egui::IconData { rgba, width: size as u32, height: size as u32 }),
        centered: true,
        ..Default::default()
    };
    let app = SettingsApp {
        saved: cfg.clone(),
        cfg,
        error: load_error,
        listening: None,
        listen_for: HotkeyField::Record,
        hotkey_error: None,
        section: Section::General,
        dialog: None,
        keyboard: false,
        rounded: None,
        light: None,
        closing: false,
    };
    let _ = eframe::run_native(
        SETTINGS_TITLE,
        options,
        Box::new(|cc| {
            fonts(&cc.egui_ctx);
            let _ = CTX.set(cc.egui_ctx.clone());
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
    /// Windows 11 rounds the window corners; None until the window exists.
    rounded: Option<bool>,
    /// The theme the egui visuals were set up for.
    light: Option<bool>,
    /// Closing was confirmed (or there was nothing to lose): let the close through.
    closing: bool,
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
    }
}

fn windows_build() -> u32 {
    let mut buf = [0u16; 16];
    let mut bytes = size_of_val(&buf) as u32;
    let key = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    unsafe {
        let _ = RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key,
            w!("CurrentBuildNumber"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        );
    }
    from_wstr(&buf).parse().unwrap_or(0)
}

/// Asks Windows 11 to round the window; returns whether it's rounded (never on Windows 10),
/// or None while the window doesn't exist yet.
fn round_corners() -> Option<bool> {
    let hwnd = ipc::find_settings()?;
    if windows_build() < 22000 {
        return Some(false);
    }
    let pref = DWMWCP_ROUND;
    let set = unsafe {
        DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as _, size_of_val(&pref) as u32)
    };
    Some(set.is_ok())
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.poll_capture();
        let ctx = ui.ctx().clone();
        if self.rounded.is_none() {
            self.rounded = round_corners();
        }
        let light = ctx.system_theme() == Some(egui::Theme::Light);
        let p = if light { widgets::LIGHT } else { widgets::DARK };
        if self.light != Some(light) {
            self.light = Some(light);
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
        let look = Look { p, keyboard: self.keyboard };
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        let active = widgets::anim(ui, Id::unique("window-active"), focused, 0.15);
        let radius: u8 = if self.rounded == Some(true) { 8 } else { 0 };
        let esc = ctx.input(|i| i.key_pressed(Key::Escape));
        let had_dialog = self.dialog.is_some();

        let full = ui.max_rect();
        ui.painter().rect_filled(full, radius, p.surf);
        let bar = Rect::from_min_size(full.min, vec2(full.width(), BAR_H));
        match widgets::title_bar(ui, &look, bar, active, radius) {
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
        let rows = Rect::from_min_max(pos2(body.left(), body.top() + 56.0), body.right_bottom());
        let mut content = ui.new_child(UiBuilder::new().max_rect(rows).layout(Layout::top_down(Align::Min)));
        content.spacing_mut().item_spacing = vec2(8.0, 0.0);
        if let Some(e) = &self.error {
            hint(&mut content, e.clone(), p.warn);
            content.add_space(12.0);
        }
        match self.section {
            Section::General => self.general(&mut content, &look),
            Section::Recording => self.recording(&mut content, &look),
            Section::Screenshots => self.screenshots(&mut content, &look),
            Section::Output => self.output(&mut content, &look),
            Section::About => self.about(&mut content, &look),
        }
        self.footer(ui, &look, Rect::from_min_max(pos2(main.left(), body.bottom()), main.right_bottom()));
        self.dialogs(&ctx, &look);

        // Alt+F4 and the taskbar's "close window" come through here.
        if ctx.input(|i| i.viewport().close_requested()) && !self.closing && self.changes() > 0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::SaveChanges);
        }
        if esc && !had_dialog && self.listening.is_none() {
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
                        "rust crates (egui, eframe, winit, windows-rs, serde, toml and their dependencies), \
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

fn fonts(ctx: &egui::Context) {
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
        s.text_styles.insert(TextStyle::Body, sans(13.0));
        s.text_styles.insert(TextStyle::Button, medium(13.0));
        s.text_styles.insert(TextStyle::Small, sans(12.0));
        s.text_styles.insert(TextStyle::Monospace, mono(12.0));
    });
}

/// egui's own widgets (text fields, the licence scroll area) in the palette's colours.
fn visuals(ctx: &egui::Context, p: &widgets::Palette, light: bool) {
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
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.set_visuals(v);
}
