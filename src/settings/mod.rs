//! Settings window, run as `glint.exe --settings`: a separate short-lived process, because
//! winit allows only one event loop per process and the window can be opened many times.
//! It edits config.toml and asks the tray app to apply it.

mod widgets;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout, RichText, Stroke, TextStyle,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
    SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, SW_SHOWNORMAL, SetForegroundWindow, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};
use windows::core::{HSTRING, w};

use crate::config::{CodecName, Config, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, hotkey_string};
use crate::ipc::{self, SETTINGS_TITLE, WM_HOTKEY_PAUSE, WM_RELOAD};
use crate::tray::icon_pixels;
use widgets::{CONTROL_W, FAINT, Kind, MUTED, TEXT, TWINKLE_EVERY, TWINKLE_LEN, WARN};

pub const REPO_URL: &str = "https://github.com/nitreojs/glint";
const FPS_PRESETS: [u32; 6] = [0, 30, 60, 120, 144, 240];
/// Wordmark font (SIL Open Font License, see assets/fonts/OFL.txt).
const DISPLAY_FONT: &[u8] = include_bytes!("../../assets/fonts/DMSerifDisplay-Italic.ttf");
/// Seconds for the header and cards to cascade in when the window opens.
const INTRO_S: f64 = 0.9;

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
            .with_inner_size([520.0, 790.0])
            .with_resizable(false)
            .with_maximize_button(false)
            .with_icon(egui::IconData { rgba, width: size as u32, height: size as u32 }),
        centered: true,
        ..Default::default()
    };
    let mut refresh_rates: Vec<u32> =
        crate::monitors::enumerate().unwrap_or_default().iter().map(|o| o.native_fps()).collect();
    refresh_rates.sort_unstable_by(|a, b| b.cmp(a));
    refresh_rates.dedup();
    let app = SettingsApp {
        saved: cfg.clone(),
        cfg,
        error: load_error,
        listening: None,
        hotkey_note: None,
        refresh_rates,
        opened_at: None,
    };
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
    /// Distinct refresh rates of the connected monitors, for the "native" hint.
    refresh_rates: Vec<u32>,
    /// `egui` time of the first frame (drives the intro animation).
    opened_at: Option<f64>,
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
        0 => "native".into(),
        n => n.to_string(),
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
        _ => "best compression, more gpu load",
    }
}

fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
}

impl eframe::App for SettingsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_capture();
        let ctx = ui.ctx().clone();
        let time = ctx.input(|i| i.time);
        let since = time - *self.opened_at.get_or_insert(time);
        if self.listening.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        widgets::backdrop(ui.painter(), ui.max_rect());
        // Header, cards and footer cascade in, each a little after the previous one.
        let appear = |i: usize| ((since - 0.07 * i as f64) / 0.4).clamp(0.0, 1.0) as f32;

        egui::Panel::bottom("buttons")
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 22, right: 22, top: 8, bottom: 16 }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.multiply_opacity(widgets::ease(appear(4)));
                ui.horizontal(|ui| {
                    let not_default = self.cfg != Config::default();
                    if widgets::button(ui, "reset to defaults", Kind::Ghost, not_default).clicked() {
                        self.cfg = Config::default();
                        self.hotkey_note = None;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let dirty = self.cfg != self.saved;
                        if widgets::button(ui, "save", Kind::Primary, dirty && self.listening.is_none()).clicked() {
                            self.save(&ctx);
                        }
                        if widgets::button(ui, "cancel", Kind::Secondary, true).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(egui::Margin { left: 22, right: 22, top: 16, bottom: 0 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.contents(ui, time, &appear));
            });

        // Repaint only while something moves (hover transitions schedule their own frames).
        let phase = time % TWINKLE_EVERY;
        if since < INTRO_S || self.listening.is_some() || phase < TWINKLE_LEN {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_secs_f64(TWINKLE_EVERY - phase));
        }
    }
}

impl SettingsApp {
    fn contents(&mut self, ui: &mut egui::Ui, time: f64, appear: &dyn Fn(usize) -> f32) {
        let semibold = FontFamily::Name("semibold".into());
        ui.scope(|ui| {
            ui.multiply_opacity(widgets::ease(appear(0)));
            ui.horizontal(|ui| {
                widgets::title(ui, "glint", FontFamily::Name("display".into()), 46.0, time);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    ui.add_space(2.0);
                    if widgets::link(ui, "github ↗").clicked() {
                        open_url(REPO_URL);
                    }
                    ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).small().color(FAINT));
                });
            });
            ui.label(RichText::new("screen recording at your monitor's native refresh rate").color(MUTED));
            if let Some(e) = &self.error {
                ui.add_space(6.0);
                ui.label(RichText::new(e).color(WARN));
            }
        });
        ui.add_space(14.0);

        widgets::card(ui, "shortcut", semibold.clone(), appear(1), |ui| {
            let listening = self.listening.is_some();
            let hint = match &self.hotkey_note {
                Some((n, warn)) => (n.clone(), if *warn { WARN } else { MUTED }),
                None => ("starts region selection; press again to stop".into(), MUTED),
            };
            let mut clicked = false;
            widgets::row(ui, "hotkey", Some(hint), |ui| {
                clicked = widgets::hotkey_field(ui, &self.cfg.hotkey, listening, time).clicked();
            });
            if clicked {
                if listening { self.stop_listening() } else { self.start_listening() }
            }
        });

        widgets::card(ui, "recording", semibold.clone(), appear(2), |ui| {
            let mut presets = FPS_PRESETS.to_vec();
            if !presets.contains(&self.cfg.fps) {
                presets.push(self.cfg.fps);
                presets.sort_unstable();
            }
            let options: Vec<(u32, String)> = presets.iter().map(|&p| (p, fps_label(p))).collect();
            let hint = match (self.cfg.fps, self.refresh_rates.as_slice()) {
                (0, []) => "native follows the monitor's refresh rate".to_string(),
                (0, rates) => {
                    let rates: Vec<String> = rates.iter().map(|r| r.to_string()).collect();
                    format!("native follows the monitor: {} hz", rates.join(" · "))
                }
                (n, _) => format!("capped at {n} fps, never above the monitor's rate"),
            };
            widgets::row(ui, "frame rate", Some((hint, MUTED)), |ui| {
                widgets::segmented(ui, "fps", &mut self.cfg.fps, &options);
            });
            let codec_hint = match self.cfg.codec {
                CodecName::H264 => "plays everywhere",
                CodecName::Hevc => "~30% smaller at the same quality; telegram, discord, browsers, vlc",
            };
            widgets::row(ui, "codec", Some((codec_hint.into(), MUTED)), |ui| {
                let codecs = [(CodecName::H264, "h.264".to_string()), (CodecName::Hevc, "hevc".to_string())];
                widgets::segmented(ui, "codec", &mut self.cfg.codec, &codecs);
            });
            let q = self.cfg.quality;
            let hint = format!("{q} · {} · lower is sharper and bigger", quality_label(q));
            widgets::row(ui, "quality", Some((hint, MUTED)), |ui| {
                widgets::slider(ui, &mut self.cfg.quality, 12..=36);
            });
            let e = self.cfg.effort;
            widgets::row(ui, "encoder effort", Some((format!("{e} · {}", effort_label(e)), MUTED)), |ui| {
                widgets::slider(ui, &mut self.cfg.effort, 0..=100);
            });
            widgets::row(ui, "cursor", None, |ui| {
                widgets::toggle(ui, &mut self.cfg.draw_cursor, "show the mouse cursor");
            });
        });

        widgets::card(ui, "output", semibold, appear(3), |ui| {
            let default_dir = Config { output_dir: String::new(), ..self.cfg.clone() }.output_dir();
            widgets::row(ui, "folder", None, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.output_dir)
                        .hint_text(default_dir.display().to_string())
                        .desired_width(CONTROL_W)
                        .margin(egui::Margin::symmetric(10, 7)),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if widgets::button(ui, "browse…", Kind::Secondary, true).clicked()
                        && let Some(p) = pick_folder(&self.cfg.output_dir())
                    {
                        self.cfg.output_dir = p.display().to_string();
                    }
                    if widgets::button(ui, "open", Kind::Ghost, true).clicked() {
                        let dir = self.cfg.output_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                    }
                });
            });
        });
    }
}

fn style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallback = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let fonts_dir = Path::new(&windir).join("Fonts");
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-semibold", "seguisb.ttf")] {
        if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
        }
    }
    fonts.font_data.insert("display".into(), Arc::new(FontData::from_static(DISPLAY_FONT)));
    let family = |first: &str| {
        let mut list: Vec<String> = fonts.font_data.contains_key(first).then(|| first.to_string()).into_iter().collect();
        list.extend(fallback.iter().cloned());
        list
    };
    let (body, semibold, display) = (family("segoe"), family("segoe-semibold"), family("display"));
    fonts.families.insert(FontFamily::Proportional, body);
    fonts.families.insert(FontFamily::Name("semibold".into()), semibold);
    fonts.families.insert(FontFamily::Name("display".into()), display);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::from_rgb(16, 16, 22);
    v.window_fill = v.panel_fill;
    v.extreme_bg_color = Color32::from_black_alpha(70); // text fields
    v.selection.bg_fill = widgets::ACCENT.gamma_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0, widgets::ACCENT);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    let radius = CornerRadius::same(9);
    for (w, stroke) in [
        (&mut v.widgets.inactive, 0.08),
        (&mut v.widgets.hovered, 0.16),
        (&mut v.widgets.active, 0.2),
        (&mut v.widgets.open, 0.16),
    ] {
        w.bg_fill = Color32::from_white_alpha(12);
        w.weak_bg_fill = Color32::from_white_alpha(12);
        w.corner_radius = radius;
        w.bg_stroke = Stroke::new(1.0, Color32::from_white_alpha((stroke * 255.0) as u8));
        w.expansion = 0.0;
    }
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        s.interaction.selectable_labels = false;
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.interact_size.y = 28.0;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
    });
}
