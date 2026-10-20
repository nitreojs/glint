//! Settings window, run as `glint.exe --settings`: a separate short-lived process, because
//! winit allows only one event loop per process and the window can be opened many times.
//! It edits config.toml and asks the tray app to apply it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke, TextStyle,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, SetForegroundWindow, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};
use windows::core::HSTRING;

use crate::config::{CodecName, Config, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, hotkey_string};
use crate::ipc::{self, SETTINGS_TITLE, WM_HOTKEY_PAUSE, WM_RELOAD};
use crate::tray::icon_pixels;

const ACCENT: Color32 = Color32::from_rgb(0, 145, 255);
const WARN: Color32 = Color32::from_rgb(255, 176, 64);
const MUTED: Color32 = Color32::from_rgb(140, 140, 150);
const FPS_PRESETS: [u32; 8] = [0, 24, 30, 60, 120, 144, 165, 240];

pub fn run() {
    if let Some(h) = ipc::find_settings() {
        unsafe {
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let (cfg, load_error) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(e)),
    };
    let size = 64;
    let rgba = icon_pixels(size, [235, 45, 45])
        .into_iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8])
        .collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(SETTINGS_TITLE)
            .with_inner_size([520.0, 640.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(egui::IconData { rgba, width: size as u32, height: size as u32 }),
        centered: true,
        ..Default::default()
    };
    let app = SettingsApp { saved: cfg.clone(), cfg, error: load_error, listening: None, hotkey_note: None };
    let _ = eframe::run_native(
        SETTINGS_TITLE,
        options,
        Box::new(|cc| {
            style(&cc.egui_ctx);
            let _ = CTX.set(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    );
}

struct SettingsApp {
    /// What is on disk.
    saved: Config,
    /// What is being edited.
    cfg: Config,
    error: Option<String>,
    /// Keyboard hook while a new hotkey is being recorded.
    listening: Option<HHOOK>,
    /// Outcome of the last hotkey recording (text, is a warning).
    hotkey_note: Option<(String, bool)>,
}

// ---- hotkey recording -----------------------------------------------------------------------
//
// A low-level keyboard hook sees every key before global hotkeys fire (PrintScreen included,
// which never reaches a window as WM_KEYDOWN), so it can record combos other apps have taken.
// Key-downs are swallowed while recording; key-ups pass through so no key gets stuck.

static CTX: OnceLock<egui::Context> = OnceLock::new();
/// Modifier keys currently held, as MOD_* bits.
static HELD: AtomicU32 = AtomicU32::new(0);
/// (modifiers, virtual key) of the first non-modifier key pressed while recording.
static CAPTURED: Mutex<Option<(u32, u32)>> = Mutex::new(None);

fn modifier_bit(vk: u32) -> u32 {
    match vk {
        0x10 | 0xA0 | 0xA1 => MOD_SHIFT,
        0x11 | 0xA2 | 0xA3 => MOD_CONTROL,
        0x12 | 0xA4 | 0xA5 => MOD_ALT,
        0x5B | 0x5C => MOD_WIN,
        _ => 0,
    }
}

unsafe extern "system" fn keyboard_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let down = wp.0 == WM_KEYDOWN as usize || wp.0 == WM_SYSKEYDOWN as usize;
            let bit = modifier_bit(kb.vkCode);
            if bit != 0 {
                if down {
                    HELD.fetch_or(bit, Ordering::SeqCst);
                } else {
                    HELD.fetch_and(!bit, Ordering::SeqCst);
                }
            } else if down {
                let mut c = CAPTURED.lock().unwrap();
                if c.is_none() {
                    *c = Some((HELD.load(Ordering::SeqCst), kb.vkCode));
                    if let Some(ctx) = CTX.get() {
                        ctx.request_repaint();
                    }
                }
            }
            if down {
                return LRESULT(1);
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

impl SettingsApp {
    fn start_listening(&mut self) {
        if self.listening.is_some() {
            return;
        }
        HELD.store(0, Ordering::SeqCst);
        *CAPTURED.lock().unwrap() = None;
        // The tray app lets go of its hotkey so pressing it records it instead of firing.
        ipc::post_to_main(WM_HOTKEY_PAUSE, 1);
        match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), None, 0) } {
            Ok(h) => {
                self.listening = Some(h);
                self.hotkey_note = None;
            }
            Err(e) => {
                ipc::post_to_main(WM_HOTKEY_PAUSE, 0);
                self.hotkey_note = Some((format!("Can't record keys: {e}"), true));
            }
        }
    }

    fn stop_listening(&mut self) {
        if let Some(h) = self.listening.take() {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
            ipc::post_to_main(WM_HOTKEY_PAUSE, 0);
        }
    }

    fn poll_capture(&mut self) {
        if self.listening.is_none() {
            return;
        }
        let Some((mods, vk)) = CAPTURED.lock().unwrap().take() else {
            return;
        };
        // Esc / Backspace on their own cancel.
        if mods == 0 && (vk == 0x1B || vk == 0x08) {
            self.stop_listening();
            return;
        }
        let Some(text) = hotkey_string(mods, vk) else {
            self.hotkey_note = Some((format!("That key (code {vk:#04x}) can't be used, try another"), true));
            return; // keep listening
        };
        self.stop_listening();
        self.hotkey_note =
            (!hotkey_available(mods, vk)).then(|| (format!("{text} is already used by another app (ShareX?)"), true));
        self.cfg.hotkey = text;
    }

    fn save(&mut self, ctx: &egui::Context) {
        let dir = self.cfg.output_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("Can't use {}: {e}", dir.display()));
            return;
        }
        match self.cfg.save() {
            Ok(()) => {
                ipc::post_to_main(WM_RELOAD, 0);
                self.saved = self.cfg.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Err(e) => self.error = Some(e),
        }
    }
}

impl Drop for SettingsApp {
    fn drop(&mut self) {
        self.stop_listening();
    }
}

/// Whether nobody else holds this combo (the tray app has released its own meanwhile).
fn hotkey_available(mods: u32, vk: u32) -> bool {
    const PROBE_ID: i32 = 0x6C69;
    unsafe {
        let ok = RegisterHotKey(None, PROBE_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk).is_ok();
        if ok {
            let _ = UnregisterHotKey(None, PROBE_ID);
        }
        ok
    }
}

fn pick_folder(start: &Path) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let opts = dialog.GetOptions().ok()?;
        dialog.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start.as_os_str()), None) {
            let _ = dialog.SetFolder(&item);
        }
        let parent: Option<HWND> = ipc::find_settings();
        dialog.Show(parent).ok()?; // Err when cancelled
        let path = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as _));
        s.map(PathBuf::from)
    }
}

fn fps_label(fps: u32) -> String {
    match fps {
        0 => "Native (monitor refresh rate)".into(),
        n => format!("{n} fps"),
    }
}

fn quality_label(q: u32) -> &'static str {
    match q {
        0..=17 => "near-lossless, big files",
        18..=20 => "very sharp",
        21..=24 => "balanced",
        25..=28 => "small files",
        _ => "smallest, visibly soft",
    }
}

fn effort_label(e: u32) -> &'static str {
    match e {
        0..=33 => "fastest",
        34..=66 => "balanced",
        _ => "best compression, more GPU load",
    }
}

/// A two-column row: label on the left, control on the right, optional hint below.
fn row(ui: &mut egui::Ui, label: &str, hint: Option<&str>, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(128.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(128.0);
            ui.label(label);
        });
        ui.vertical(|ui| {
            control(ui);
            if let Some(h) = hint {
                ui.label(RichText::new(h).small().color(MUTED));
            }
        });
    });
    ui.add_space(6.0);
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).text_style(TextStyle::Name("section".into())).color(Color32::WHITE));
    ui.add_space(4.0);
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_capture();
        let ctx = ui.ctx().clone();
        if self.listening.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::Panel::bottom("buttons")
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(20, 14)))
            .show_separator_line(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Reset to defaults").clicked() {
                        self.cfg = Config::default();
                        self.hotkey_note = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let dirty = self.cfg != self.saved;
                        let save = egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(if dirty {
                            ACCENT
                        } else {
                            ui.visuals().widgets.inactive.weak_bg_fill
                        });
                        if ui.add_enabled(dirty && self.listening.is_none(), save).clicked() {
                            self.save(&ctx);
                        }
                        if ui.button("Cancel").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(20, 16)))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.contents(ui));
            });

        if self.listening.is_some() {
            ctx.request_repaint(); // keep polling the hook
        }
    }
}

impl SettingsApp {
    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("glint").text_style(TextStyle::Heading));
        ui.label(RichText::new("Screen recording at your monitor's native refresh rate").color(MUTED));
        if let Some(e) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(WARN));
        }
        ui.add_space(10.0);

        section(ui, "Shortcut");
        let listening = self.listening.is_some();
        let mut toggle = false;
        let note = self.hotkey_note.clone();
        row(ui, "Hotkey", None, |ui| {
            let text =
                if listening { "Press a shortcut…  (Esc to cancel)".to_string() } else { self.cfg.hotkey.clone() };
            let button = egui::Button::new(RichText::new(text).monospace()).min_size(egui::vec2(280.0, 28.0));
            let button = if listening { button.stroke(Stroke::new(1.5, ACCENT)) } else { button };
            toggle = ui.add(button).on_hover_text("Click, then press the new shortcut").clicked();
            match &note {
                Some((n, warn)) => {
                    ui.label(RichText::new(n).small().color(if *warn { WARN } else { MUTED }));
                }
                None => {
                    ui.label(RichText::new("Starts region selection; press again to stop").small().color(MUTED));
                }
            }
        });
        if toggle {
            if listening { self.stop_listening() } else { self.start_listening() }
        }

        section(ui, "Recording");
        row(ui, "Frame rate", None, |ui| {
            let mut presets = FPS_PRESETS.to_vec();
            if !presets.contains(&self.cfg.fps) {
                presets.push(self.cfg.fps);
                presets.sort();
            }
            egui::ComboBox::from_id_salt("fps").width(280.0).selected_text(fps_label(self.cfg.fps)).show_ui(ui, |ui| {
                for p in presets {
                    ui.selectable_value(&mut self.cfg.fps, p, fps_label(p));
                }
            });
        });
        let codec_hint = match self.cfg.codec {
            CodecName::H264 => "Plays everywhere",
            CodecName::Hevc => "About 30% smaller at the same quality; Telegram, Discord, browsers, VLC",
        };
        row(ui, "Codec", Some(codec_hint), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.cfg.codec, CodecName::H264, "  H.264  ");
                ui.selectable_value(&mut self.cfg.codec, CodecName::Hevc, "  HEVC  ");
            });
        });
        let q = self.cfg.quality;
        row(ui, "Quality", Some(&format!("{q}: {}  ·  lower is sharper and bigger", quality_label(q))), |ui| {
            ui.add(egui::Slider::new(&mut self.cfg.quality, 12..=36).show_value(false));
        });
        let e = self.cfg.effort;
        row(ui, "Encoder effort", Some(&format!("{e}: {}", effort_label(e))), |ui| {
            ui.add(egui::Slider::new(&mut self.cfg.effort, 0..=100).show_value(false));
        });
        row(ui, "Cursor", None, |ui| {
            ui.checkbox(&mut self.cfg.draw_cursor, "Show the mouse cursor in recordings");
        });

        section(ui, "Output");
        let default_dir = Config { output_dir: String::new(), ..self.cfg.clone() }.output_dir();
        row(ui, "Folder", None, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.output_dir)
                        .hint_text(default_dir.display().to_string())
                        .desired_width(196.0),
                );
                if ui.button("Browse…").clicked()
                    && let Some(p) = pick_folder(&self.cfg.output_dir())
                {
                    self.cfg.output_dir = p.display().to_string();
                }
                if ui.button("Open").clicked() {
                    let dir = self.cfg.output_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                }
            });
        });
    }
}

fn style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let fonts_dir = Path::new(&windir).join("Fonts");
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-semibold", "seguisb.ttf")] {
        if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
        }
    }
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if fonts.font_data.contains_key("segoe") {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    let semibold = FontFamily::Name("semibold".into());
    let mut sb = vec![];
    if fonts.font_data.contains_key("segoe-semibold") {
        sb.push("segoe-semibold".to_string());
    }
    sb.extend(fallback);
    fonts.families.insert(semibold.clone(), sb);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    let panel = Color32::from_rgb(22, 22, 27);
    v.panel_fill = panel;
    v.window_fill = panel;
    v.extreme_bg_color = Color32::from_rgb(13, 13, 17);
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = ACCENT;
    let radius = CornerRadius::same(6);
    for (w, fill) in [
        (&mut v.widgets.inactive, Color32::from_rgb(40, 40, 47)),
        (&mut v.widgets.hovered, Color32::from_rgb(52, 52, 61)),
        (&mut v.widgets.active, Color32::from_rgb(62, 62, 72)),
        (&mut v.widgets.open, Color32::from_rgb(52, 52, 61)),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.corner_radius = radius;
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.noninteractive.corner_radius = radius;
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.slider_width = 280.0;
        s.spacing.interact_size.y = 26.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Heading, FontId::new(24.0, semibold.clone()));
        s.text_styles.insert(TextStyle::Name("section".into()), FontId::new(15.0, semibold.clone()));
    });
}
