//! Settings window, run as `glint.exe --settings`: a separate short-lived process, because
//! winit allows only one event loop per process and the window can be opened many times.
//! It edits config.toml and asks the tray app to apply it.
//!
//! The window draws its own title bar (egui-winit keeps the DWM shadow for undecorated
//! windows) and follows the Windows light / dark app setting.

mod widgets;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Id, Key, Layout, Rect, RichText,
    Stroke, TextStyle, Ui, UiBuilder, pos2, vec2,
};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Input::KeyboardAndMouse::{HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
use windows::Win32::UI::Shell::{
    FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
    SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SW_SHOWNORMAL, SetForegroundWindow,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};
use windows::core::{HSTRING, w};

use crate::config::{
    CodecName, Config, DEFAULT_FILE_NAME, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, file_name_problem, hotkey_string,
};
use crate::ipc::{self, SETTINGS_TITLE, WM_HOTKEY_PAUSE, WM_RELOAD};
use crate::tray::icon_pixels;
use crate::util::{date_time, from_wstr};
use widgets::{
    BAR_H, CONTENT_X, FOOTER_H, HotkeyState, Kind, Look, SIDEBAR_W, TitleAction, WINDOW, hint, medium, mono, row, sans,
    semibold,
};

pub const REPO_URL: &str = "https://github.com/nitreojs/glint";
const FPS_PRESETS: [u32; 6] = [0, 30, 60, 120, 144, 240];
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
enum Section {
    General,
    Recording,
    Output,
    About,
}

const SECTIONS: [Section; 4] = [Section::General, Section::Recording, Section::Output, Section::About];

impl Section {
    fn name(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Recording => "recording",
            Section::Output => "output",
            Section::About => "about",
        }
    }

    /// How many of this section's settings differ between `a` and `b`.
    fn changes(self, a: &Config, b: &Config) -> usize {
        let n = |same: bool| usize::from(!same);
        match self {
            Section::General => n(a.hotkey == b.hotkey) + n(a.launch_at_startup == b.launch_at_startup),
            Section::Recording => {
                n(a.fps == b.fps)
                    + n(a.codec == b.codec)
                    + n(a.quality == b.quality)
                    + n(a.effort == b.effort)
                    + n(a.draw_cursor == b.draw_cursor)
            }
            Section::Output => n(a.output_dir == b.output_dir) + n(a.file_name == b.file_name),
            Section::About => 0,
        }
    }

    /// Puts this section's settings back to their defaults.
    fn reset(self, cfg: &mut Config) {
        let d = Config::default();
        match self {
            Section::General => {
                cfg.hotkey = d.hotkey;
                cfg.launch_at_startup = d.launch_at_startup;
            }
            Section::Recording => {
                cfg.fps = d.fps;
                cfg.codec = d.codec;
                cfg.quality = d.quality;
                cfg.effort = d.effort;
                cfg.draw_cursor = d.draw_cursor;
            }
            Section::Output => {
                cfg.output_dir = d.output_dir;
                cfg.file_name = d.file_name;
            }
            Section::About => {}
        }
    }
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
    /// The keyboard hook's thread while a new hotkey is being recorded.
    listening: Option<Listener>,
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

// ---- hotkey recording -----------------------------------------------------------------------
//
// A low-level keyboard hook sees every key before global hotkeys fire (PrintScreen included,
// which never reaches a window as WM_KEYDOWN), so it can record combos other apps have taken.
// Key-downs are swallowed while recording; key-ups pass through so no key gets stuck.
//
// The hook lives in a windowless helper, `glint.exe --capture-keys`: installed in this process
// it was never called while the settings window had focus (Windows 10), though a hook in any
// other process saw the same keys. The helper reports over stdout and quits when its stdin
// closes, so it can't outlive the settings window.

pub const CAPTURE_ARG: &str = "--capture-keys";

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

fn repaint() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Helper side: one line per event, flushed at once.
fn report(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

unsafe extern "system" fn capture_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if code == HC_ACTION as i32 {
            let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
            let down = wp.0 == WM_KEYDOWN as usize || wp.0 == WM_SYSKEYDOWN as usize;
            let bit = modifier_bit(kb.vkCode);
            let held = HELD.load(Ordering::SeqCst);
            if bit != 0 {
                let now = if down { held | bit } else { held & !bit };
                if now != held {
                    HELD.store(now, Ordering::SeqCst);
                    report(&format!("held {now}"));
                }
            } else if down {
                report(&format!("key {held} {}", kb.vkCode));
            }
            if down {
                return LRESULT(1);
            }
        }
        CallNextHookEx(None, code, wp, lp)
    }
}

/// `glint.exe --capture-keys`: hooks the keyboard until stdin closes or the process is killed.
pub fn capture_keys() {
    std::thread::spawn(|| {
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]); // EOF (or no stdin at all)
        std::process::exit(0);
    });
    unsafe {
        let module = GetModuleHandleW(None).ok().map(HINSTANCE::from);
        let Ok(hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(capture_hook), module, 0) else { return };
        report("ready");
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        let _ = UnhookWindowsHookEx(hook);
    }
}

/// "Ctrl+Alt+PrintScreen" as keycap labels.
fn key_names(hotkey: &str) -> Vec<String> {
    hotkey.split('+').map(|k| k.trim().to_lowercase()).collect()
}

/// The modifiers held right now, as keycap labels.
fn held_names() -> Vec<String> {
    let held = HELD.load(Ordering::SeqCst);
    [(MOD_CONTROL, "ctrl"), (MOD_ALT, "alt"), (MOD_SHIFT, "shift"), (MOD_WIN, "win")]
        .into_iter()
        .filter(|(bit, _)| held & bit != 0)
        .map(|(_, name)| name.to_string())
        .collect()
}

/// The running capture helper; its reports land in [`HELD`] and [`CAPTURED`].
struct Listener {
    child: std::process::Child,
}

impl Listener {
    fn start() -> Result<Listener, String> {
        use std::io::BufRead;
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new(exe)
            .arg(CAPTURE_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut lines = std::io::BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
        // Keys pressed before the hook is in place would slip through, so wait for it.
        if !matches!(lines.next(), Some(Ok(l)) if l == "ready") {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the key capture helper didn't start".into());
        }
        std::thread::spawn(move || {
            for line in lines.map_while(Result::ok) {
                let mut words = line.split(' ');
                let kind = words.next();
                let nums: Vec<u32> = words.filter_map(|w| w.parse().ok()).collect();
                match (kind, nums.as_slice()) {
                    (Some("held"), &[mods]) => HELD.store(mods, Ordering::SeqCst),
                    (Some("key"), &[mods, vk]) => {
                        let mut c = CAPTURED.lock().unwrap();
                        if c.is_none() {
                            *c = Some((mods, vk));
                        }
                    }
                    _ => continue,
                }
                repaint();
            }
        });
        Ok(Listener { child })
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl SettingsApp {
    fn start_listening(&mut self) {
        if self.listening.is_some() {
            return;
        }
        HELD.store(0, Ordering::SeqCst);
        *CAPTURED.lock().unwrap() = None;
        self.hotkey_error = None;
        // The tray app lets go of its hotkey so pressing it records it instead of firing.
        ipc::post_to_main(WM_HOTKEY_PAUSE, 1);
        match Listener::start() {
            Ok(l) => self.listening = Some(l),
            Err(e) => {
                ipc::post_to_main(WM_HOTKEY_PAUSE, 0);
                self.error = Some(format!("can't record keys: {e}"));
            }
        }
    }

    fn stop_listening(&mut self) {
        self.hotkey_error = None;
        if let Some(l) = self.listening.take() {
            l.stop();
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
            return self.stop_listening();
        }
        // Anything that can't be used keeps listening, so the next combo retries.
        let Some(text) = hotkey_string(mods, vk) else {
            self.hotkey_error = Some((Vec::new(), "that key can't be part of a hotkey. try another.".into()));
            return;
        };
        let keys = key_names(&text);
        let standalone = (0x70..=0x87).contains(&vk) || matches!(vk, 0x13 | 0x2C | 0x91); // F1-F24, Pause, PrtSc, ScrLk
        if mods == 0 && !standalone {
            let msg = format!("{} alone would fire while you type. add ctrl, alt, shift or win.", keys[0]);
            self.hotkey_error = Some((keys, msg));
            return;
        }
        if !hotkey_available(mods, vk) {
            let msg = format!("{} is already used by another app. try a different shortcut.", keys.join(" + "));
            self.hotkey_error = Some((keys, msg));
            return;
        }
        self.stop_listening();
        self.cfg.hotkey = text;
    }

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

fn open_url(url: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
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

fn fps_label(fps: u32) -> String {
    match fps {
        0 => "native".into(),
        n => n.to_string(),
    }
}

fn quality_word(q: u32) -> &'static str {
    match q {
        0..=14 => "near lossless",
        15..=20 => "very sharp",
        21..=24 => "sharp",
        25..=29 => "balanced",
        _ => "small files",
    }
}

fn effort_word(e: u32) -> &'static str {
    match e {
        0..=24 => "fastest, least gpu load",
        25..=49 => "fast",
        50..=69 => "balanced",
        _ => "best compression, more gpu load",
    }
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
    fn general(&mut self, ui: &mut Ui, look: &Look) {
        let keys = key_names(&self.cfg.hotkey);
        let held = held_names();
        let state = match (&self.listening, &self.hotkey_error) {
            (Some(_), Some((keys, _))) => HotkeyState::Error { keys },
            (Some(_), None) => HotkeyState::Listening { held: &held },
            _ => HotkeyState::Idle,
        };
        let mut clicked = false;
        row(ui, look, "record hotkey", 7.0, |ui| {
            clicked = widgets::hotkey_field(ui, look, &keys, state).clicked();
            if let Some((_, msg)) = &self.hotkey_error {
                hint(ui, msg.clone(), look.p.warn);
            }
        });
        if clicked {
            if self.listening.is_some() { self.stop_listening() } else { self.start_listening() }
        }
        row(ui, look, "launch at startup", 1.0, |ui| {
            let text = if self.cfg.launch_at_startup { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.launch_at_startup, text);
            hint(ui, "glint starts in the tray when you sign in to windows", look.p.t3);
        });
    }

    fn recording(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        let mut presets = FPS_PRESETS.to_vec();
        if !presets.contains(&self.cfg.fps) {
            presets.push(self.cfg.fps); // a hand-edited value stays selectable
            presets.sort_unstable();
        }
        let labels: Vec<String> = presets.iter().map(|&f| fps_label(f)).collect();
        let fps: Vec<(u32, &str)> = presets.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "frame rate", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.fps, &fps, None);
            let text = match self.cfg.fps {
                0 => "native follows each monitor's refresh rate".to_string(),
                n => format!("{n} fps, or less on slower monitors"),
            };
            hint(ui, text, t3);
        });
        row(ui, look, "codec", 7.0, |ui| {
            let codecs = [(CodecName::H264, "h.264"), (CodecName::Hevc, "hevc")];
            widgets::segmented(ui, look, &mut self.cfg.codec, &codecs, Some(168.0));
            hint(ui, "h.264 — plays everywhere · hevc — ~30 % smaller files", t3);
        });
        row(ui, look, "quality", 0.0, |ui| {
            let word = quality_word(self.cfg.quality);
            widgets::slider(ui, look, &mut self.cfg.quality, 12..=36, word, "12 ← sharper, bigger", "smaller, softer → 36");
        });
        row(ui, look, "encoder effort", 0.0, |ui| {
            let word = effort_word(self.cfg.effort);
            widgets::slider(ui, look, &mut self.cfg.effort, 0..=100, word, "0 ← faster", "smaller files → 100");
        });
        row(ui, look, "mouse cursor", 1.0, |ui| {
            let text = if self.cfg.draw_cursor { "shown in captures" } else { "hidden" };
            widgets::toggle(ui, look, &mut self.cfg.draw_cursor, text);
        });
    }

    fn output(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let default_dir = Config { output_dir: String::new(), ..Config::default() }.output_dir();
        row(ui, look, "folder", 7.0, |ui| {
            widgets::text_field(ui, look, &mut self.cfg.output_dir, &default_dir.display().to_string(), false);
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if widgets::button(ui, look, "browse…", Kind::Secondary, 28.0, true).clicked()
                    && let Some(dir) = pick_folder(&self.cfg.output_dir())
                {
                    self.cfg.output_dir = dir.display().to_string();
                }
                if widgets::button(ui, look, "open folder", Kind::Secondary, 28.0, true).clicked() {
                    let dir = self.cfg.output_dir();
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                }
            });
            ui.add_space(2.0);
            hint(ui, "recordings are saved here. leave empty for the default.", p.t3);
        });
        let problem = file_name_problem(&self.cfg.file_name);
        row(ui, look, "file names", 7.0, |ui| {
            widgets::text_field(ui, look, &mut self.cfg.file_name, DEFAULT_FILE_NAME, problem.is_some());
            let (date, time) = date_time();
            let example = format!("{}.mp4", self.cfg.file_stem(&date, &time));
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(RichText::new("→").font(mono(12.0)).color(p.t3));
                ui.add(egui::Label::new(RichText::new(example).font(mono(12.0)).color(p.t2)).truncate());
            });
            match problem {
                Some(msg) => hint(ui, msg, p.warn),
                None => hint(ui, "{date} and {time} are filled in when the file is saved", p.t3),
            }
        });
    }

    fn about(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        row(ui, look, "version", 0.0, |ui| {
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            ui.label(RichText::new(version).font(widgets::mono_medium(13.0)).color(p.t1));
        });
        row(ui, look, "source code", 0.0, |ui| {
            if widgets::link(ui, look, "github.com/nitreojs/glint ↗").clicked() {
                open_url(REPO_URL);
            }
        });
        row(ui, look, "licences", 0.0, |ui| {
            if widgets::link(ui, look, "open-source licences").clicked() {
                self.dialog = Some(Dialog::Licences);
            }
        });
        row(ui, look, "all settings", 5.0, |ui| {
            let any = self.cfg != Config::default();
            if widgets::button(ui, look, "reset everything to defaults", Kind::Secondary, 28.0, any).clicked() {
                self.cfg = Config::default();
            }
            hint(ui, "every section at once. nothing changes until you save.", p.t3);
        });
    }

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
