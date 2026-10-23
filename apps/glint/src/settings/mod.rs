//! Settings window, run as `glint.exe --settings` (see [`glint_ui::settings`] for the window):
//! it edits config.toml and asks the tray app to apply it.

use eframe::egui::{self, Ui};
use glint_core::config::Config;
use glint_core::icon::icon_rgba;
use glint_core::naming::pattern_problem;
use glint_core::paths::app_dir;
use glint_ui::capture::Recorder;
use glint_ui::settings::{self as frame, Pages};
use glint_ui::widgets::Look;
use windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow;

use crate::app::ipc::{self, SETTINGS_TITLE, WM_RELOAD};
use crate::platform::paths::{output_dir, remember_previous_output_dir};

mod hotkey_capture;
mod sections;

pub use hotkey_capture::{CAPTURE_ARG, capture_keys};
use hotkey_capture::{HotkeyField, release_tray_hotkeys};
use sections::audio::SourceRows;
use sections::output::Mover;
use sections::updates::Updater;
use sections::uploads::Uploaders;
use sections::{SECTIONS, Section};

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
    let icon = icon_rgba(64, cfg.accent_rgb());
    let app = SettingsApp::new(cfg, load_error, Updater::load());
    frame::run(SETTINGS_TITLE, app_dir().join("settings-window.json"), icon, app);
}

struct SettingsApp {
    /// What is on disk.
    saved: Config,
    /// What is being edited.
    cfg: Config,
    error: Option<String>,
    /// Records a new hotkey, for `listen_for`.
    keys: Recorder,
    listen_for: HotkeyField,
    /// The licences dialog is open.
    licences: bool,
    uploaders: Uploaders,
    updater: Updater,
    audio_devices: Option<sections::audio::Devices>,
    source_rows: SourceRows,
    mover: Mover,
    /// What's typed in the custom accent field; `cfg.accent` follows it while it's a colour.
    accent_text: String,
}

fn repaint() {
    frame::repaint();
}

impl SettingsApp {
    fn new(cfg: Config, error: Option<String>, updater: Updater) -> SettingsApp {
        SettingsApp {
            saved: cfg.clone(),
            accent_text: cfg.accent.clone(),
            cfg,
            error,
            keys: Recorder::new(release_tray_hotkeys),
            listen_for: HotkeyField::Record,
            licences: false,
            uploaders: Uploaders::load(),
            updater,
            audio_devices: None,
            source_rows: SourceRows::default(),
            mover: Mover::default(),
        }
    }
}

impl Pages for SettingsApp {
    type Page = Section;
    const APP: &'static str = "glint";

    fn pages(&self) -> &'static [Section] {
        &SECTIONS
    }

    fn name(&self, page: Section) -> &'static str {
        page.name()
    }

    fn accent(&self) -> [u8; 3] {
        self.cfg.accent_rgb()
    }

    fn changes(&self, page: Section) -> usize {
        page.changes(&self.cfg, &self.saved)
    }

    fn resettable(&self, page: Section) -> Option<bool> {
        (page != Section::About).then(|| page.changes(&self.cfg, &Config::default()) > 0)
    }

    fn reset(&mut self, page: Section) {
        page.reset(&mut self.cfg);
    }

    fn show(&mut self, page: Section, ui: &mut Ui, look: &Look) {
        match page {
            Section::General => self.general(ui, look),
            Section::Recording => self.recording(ui, look),
            Section::Audio => self.audio(ui, look),
            Section::Screenshots => self.screenshots(ui, look),
            Section::Uploads => self.uploads(ui, look),
            Section::Output => self.output(ui, look),
            Section::Updates => self.updates(ui, look),
            Section::About => self.about(ui, look),
        }
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }

    fn can_save(&self) -> bool {
        !self.keys.listening() && pattern_problem(&self.cfg.file_name).is_none()
    }

    /// Writes config.toml and tells the tray app; returns whether it worked.
    fn save(&mut self) -> bool {
        let dir = output_dir(&self.cfg);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error = Some(format!("can't use {}: {e}", dir.display()));
            return false;
        }
        match self.cfg.save() {
            Ok(()) => {
                let before = output_dir(&self.saved);
                if before != dir {
                    remember_previous_output_dir(&before);
                }
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

    fn revert(&mut self) {
        self.cfg = self.saved.clone();
    }

    fn leave(&mut self) {
        self.stop_listening();
    }

    fn tick(&mut self) {
        self.poll_capture();
    }

    fn busy(&self) -> bool {
        self.keys.listening()
    }

    fn overlays(&mut self, ctx: &egui::Context, look: &Look) -> bool {
        const CRATES: &str = "egui, eframe, winit, windows-rs, serde, toml, gif, color_quant, minisign-verify";
        if self.licences {
            self.licences = frame::licences(ctx, look, CRATES);
        }
        self.licences
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App;
    use eframe::egui::{Color32, Key, Rect, pos2, vec2};
    use glint_ui::settings::Frame;
    use glint_ui::theme::fonts;
    use glint_ui::widgets::{self, CONTENT_X, WINDOW};

    const TOP: f32 = 118.0;

    struct Harness {
        ctx: egui::Context,
        app: Frame<SettingsApp>,
        frame: eframe::Frame,
        time: f64,
        screen: egui::Vec2,
        tree: Vec<(egui::accesskit::NodeId, egui::accesskit::Node)>,
    }

    impl Harness {
        fn new() -> Harness {
            let ctx = egui::Context::default();
            fonts(&ctx);
            let updater = Updater::at(std::env::temp_dir().join("glint-tests-no-update-status.json"));
            let app = Frame::new(SettingsApp::new(Config::default(), None, updater), (WINDOW, WINDOW));
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

        // a picked section drops in, then rests at the top
        h.app.go(Section::Recording);
        h.settle();
        assert_eq!(h.frame(Vec::new(), "frame rate"), Some(TOP));
        h.app.go(Section::General);
        h.settle();
        assert_eq!(h.frame(Vec::new(), "record hotkey"), Some(TOP));
    }

    #[test]
    fn row_labels_clear_their_controls() {
        use glint_core::config::audio::{AudioSource, SourceKind};
        for width in [WINDOW[0], 760.0, 1200.0] {
            let mut h = Harness::new();
            h.screen = vec2(width, WINDOW[1]);
            h.app.pages.cfg.upload_destination = "litterbox".into();
            h.app.pages.cfg.audio_sources =
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
        use glint_core::config::color::rgb_to_hsv;
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
        let [hue, s, v] = rgb_to_hsv(h.app.pages.cfg.accent_rgb());
        let red = rgb_to_hsv(ACCENTS[0])[0];
        assert!((s - 0.75).abs() < 0.02 && (v - 0.75).abs() < 0.02 && (hue - red).abs() < 0.01, "{hue} {s} {v}");
        assert_eq!(h.app.pages.cfg.accent_custom.as_deref(), Some(h.app.pages.cfg.accent.as_str()));

        let strip = square.bottom() + 10.0 + 6.0;
        h.drag(pos2(square.left() + 10.0, strip), pos2(square.center().x, strip));
        let picked = h.app.pages.cfg.accent_rgb();
        assert!((rgb_to_hsv(picked)[0] - 0.5).abs() < 0.01, "{picked:?}");
        assert_eq!(h.app.styled_accent(), Some(picked));

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
        assert!(!h.app.asking() && !h.app.closing());
        assert_eq!(h.app.pages.cfg.accent_rgb(), picked);

        h.click(pos2(swatches.left() + 12.0, swatches.top() + 12.0));
        assert_eq!((h.app.pages.cfg.accent_rgb(), h.app.pages.cfg.accent_custom_rgb()), (ACCENTS[0], Some(picked)));
        h.click(custom);
        assert_eq!(h.app.pages.cfg.accent_rgb(), picked);
        assert!(egui::Popup::is_id_open(&h.ctx, accent_popup_id()));
        h.click(pos2(600.0, 70.0));
        assert!(!egui::Popup::is_id_open(&h.ctx, accent_popup_id()));
        assert_eq!(h.app.pages.cfg.accent_rgb(), picked);
    }

    #[test]
    fn every_section_reports_its_controls_to_accessibility_tools() {
        use egui::accesskit::{Node, Role, Toggled};
        use sections::general::ACCENTS;
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.app.pages.cfg.upload_destination = "litterbox".into();
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
                assert!(quality.len() == 1 && quality[0].numeric_value() == Some(h.app.pages.cfg.quality as f64), "{quality:?}");
            }
        }
        eprintln!("{}", counts.join("\n"));
    }

    #[test]
    fn output_section_names_files_after_windows_and_moves_old_captures() {
        use egui::accesskit::Role;
        use sections::output::Leftover;
        use sections::output::tests::temp;
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.screen = vec2(WINDOW[0], 1400.0);
        let root = temp("harness");
        let (old, now) = (root.join("old"), root.join("now"));
        std::fs::create_dir_all(old.join("Telegram")).unwrap();
        std::fs::write(old.join("Telegram").join("a.png"), b"x").unwrap();
        h.app.pages.saved.output_dir = now.display().to_string();
        h.app.pages.cfg.output_dir = h.app.pages.saved.output_dir.clone();
        h.app.pages.mover.scanned = Some(now.clone());
        h.app.pages.mover.leftovers = vec![Leftover { from: old.clone(), files: vec![old.join("Telegram").join("a.png")] }];
        h.app.go(Section::Output);
        h.settle();

        let toggle = |h: &mut Harness, label: &str| {
            let c = h.control(label);
            h.click(pos2(c.left() + 18.0, c.top() + 10.0));
        };
        toggle(&mut h, "name files after the window");
        assert_eq!(h.app.pages.cfg.file_name, "{app}_{date}_{time}");
        toggle(&mut h, "sort into folders by app");
        assert_eq!(h.app.pages.cfg.file_name, "{app}/{app}_{date}_{time}");
        toggle(&mut h, "name files after the window");
        assert_eq!(h.app.pages.cfg.file_name, "{app}/glint_{date}_{time}");
        assert_eq!(Section::Output.changes(&h.app.pages.cfg, &h.app.pages.saved), 1);
        Section::Output.reset(&mut h.app.pages.cfg);
        assert_eq!((h.app.pages.cfg.file_name.as_str(), h.app.pages.cfg.output_dir.as_str()), ("glint_{date}_{time}", ""));
        h.app.pages.cfg.output_dir = h.app.pages.saved.output_dir.clone();

        let shows = |tree: &[(egui::accesskit::NodeId, egui::accesskit::Node)], text: &str| {
            tree.iter().any(|(_, n)| n.role() == Role::Label && (n.value() == Some(text) || n.label() == Some(text)))
        };
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        let text = format!("1 capture is still in {}", old.display());
        assert!(shows(&tree, &text), "{text}");
        let button = tree
            .iter()
            .find(|(_, n)| n.role() == Role::Button && n.label() == Some("move my files here"))
            .and_then(|(_, n)| n.bounds())
            .expect("the move button");
        h.click(pos2(((button.x0 + button.x1) / 2.0) as f32, ((button.y0 + button.y1) / 2.0) as f32));
        for _ in 0..200 {
            if !h.app.pages.mover.leftovers.iter().any(|l| l.from == old) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            h.frame(Vec::new(), "");
        }
        h.settle();
        assert!(now.join("Telegram").join("a.png").is_file() && !old.join("Telegram").join("a.png").exists());
        let tree = std::mem::take(&mut h.tree);
        assert!(shows(&tree, "moved 1 file"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn updates_section_shows_the_tray_apps_status() {
        use glint_core::config::UpdateMode;
        use crate::update::{State, Status};
        use egui::accesskit::{Node, Role, Toggled};
        fn find(tree: &[(egui::accesskit::NodeId, Node)], role: Role, text: &str) -> Option<Node> {
            let named = |n: &Node| n.label() == Some(text) || n.value() == Some(text);
            tree.iter().find(|(_, n)| n.role() == role && named(n)).map(|(_, n)| n.clone())
        }
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.app.go(Section::Updates);
        let ready = State::Ready { version: "0.2.0".into() };
        h.app.pages.updater.status = Status { checked: Some("2026-10-21 14:32".into()), state: ready };
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        assert!(find(&tree, Role::Label, concat!("v", env!("CARGO_PKG_VERSION"))).is_some());
        let check = find(&tree, Role::Button, "check now").expect("check now");
        assert!(!check.is_disabled());
        assert!(find(&tree, Role::Label, "0.2.0 ready ·").is_some() && find(&tree, Role::Link, "restart").is_some());
        assert!(find(&tree, Role::Label, "last checked 2026-10-21 14:32").is_some());
        assert!(find(&tree, Role::Link, "what's new in 0.2.0 ↗").is_some());
        let modes = find(&tree, Role::RadioGroup, "updates").expect("the mode picker");
        let options: Vec<_> = modes
            .children()
            .iter()
            .filter_map(|id| tree.iter().find(|(i, _)| i == id))
            .map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.toggled()))
            .collect();
        let on = |name: &str, yes: bool| (name.to_owned(), Some(if yes { Toggled::True } else { Toggled::False }));
        assert_eq!(options, [on("auto", true), on("notify only", false), on("off", false)]);
        let beta = find(&tree, Role::CheckBox, "beta updates").expect("the beta toggle");
        assert_eq!(beta.toggled(), Some(Toggled::False));

        let picker = h.control("updates");
        h.click(pos2(picker.left() + 150.0, picker.top() + 16.0));
        assert_eq!(h.app.pages.cfg.update_mode, UpdateMode::Notify);
        assert_eq!(Section::Updates.changes(&h.app.pages.cfg, &h.app.pages.saved), 1);
        Section::Updates.reset(&mut h.app.pages.cfg);
        assert_eq!(h.app.pages.cfg, h.app.pages.saved);

        h.app.pages.updater.status.state = State::Failed { error: "github answered 500 for the release list".into() };
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        assert!(find(&tree, Role::Label, "github answered 500 for the release list").is_some());
        assert!(find(&tree, Role::Link, "restart").is_none());
        assert!(find(&tree, Role::Link, "github.com/nitreojs/glint/releases ↗").is_some());

        h.app.pages.updater.status = Status { checked: None, state: State::Checking };
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        assert!(find(&tree, Role::Button, "check now").expect("check now").is_disabled());
        assert!(find(&tree, Role::Label, "checking…").is_some() && find(&tree, Role::Label, "not checked yet").is_some());
    }

    #[test]
    fn click_highlights_reveal_their_mode_and_count_as_changes() {
        use egui::accesskit::{Node, Role, Toggled};
        use glint_core::config::{ClickMode, Keystrokes};
        use glint_core::record::keys::Position;
        fn find(tree: &[(egui::accesskit::NodeId, Node)], role: Role, text: &str) -> Option<Node> {
            let named = |n: &Node| n.label() == Some(text) || n.value() == Some(text);
            tree.iter().find(|(_, n)| n.role() == role && named(n)).map(|(_, n)| n.clone())
        }
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.screen = vec2(WINDOW[0], 1400.0);
        h.app.go(Section::Recording);
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        assert!(find(&tree, Role::Heading, "overlays").is_some());
        let toggle = find(&tree, Role::CheckBox, "click highlights").expect("the toggle");
        assert_eq!((toggle.toggled(), toggle.description()), (Some(Toggled::False), Some("off")));
        assert!(find(&tree, Role::RadioGroup, "clicks").is_none());
        assert!(find(&tree, Role::RadioGroup, "keystrokes").is_some());
        assert!(find(&tree, Role::RadioGroup, "keystroke position").is_none());
        assert!(find(&tree, Role::RadioGroup, "keystrokes mode").is_none());

        let c = h.control("click highlights");
        h.click(pos2(c.left() + 18.0, c.top() + 10.0));
        assert!(h.app.pages.cfg.click_highlights);
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        let toggle = find(&tree, Role::CheckBox, "click highlights").expect("the toggle");
        assert_eq!((toggle.toggled(), toggle.description()), (Some(Toggled::True), Some("a ripple on every click")));
        let modes = find(&tree, Role::RadioGroup, "clicks").expect("the mode picker");
        let options: Vec<_> = modes
            .children()
            .iter()
            .filter_map(|id| tree.iter().find(|(i, _)| i == id))
            .map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.toggled()))
            .collect();
        assert_eq!(options, [("editable".to_owned(), Some(Toggled::True)), ("burn in".to_owned(), Some(Toggled::False))]);
        assert!(find(&tree, Role::Label, "editable clicks can be moved or removed in the trimmer").is_some());

        let picker = h.control("clicks");
        h.click(pos2(picker.left() + 126.0, picker.top() + 16.0));
        assert_eq!(h.app.pages.cfg.clicks, ClickMode::BurnIn);

        let keys = h.control("keystrokes");
        h.click(pos2(keys.left() + 167.0, keys.top() + 16.0));
        assert_eq!(h.app.pages.cfg.keystrokes, Keystrokes::All);
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        let levels = find(&tree, Role::RadioGroup, "keystrokes").expect("the level picker");
        let options: Vec<_> = levels
            .children()
            .iter()
            .filter_map(|id| tree.iter().find(|(i, _)| i == id))
            .map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.toggled()))
            .collect();
        assert_eq!(
            options,
            [
                ("off".to_owned(), Some(Toggled::False)),
                ("shortcuts".to_owned(), Some(Toggled::False)),
                ("all keys".to_owned(), Some(Toggled::True)),
            ]
        );
        assert!(find(&tree, Role::Label, "all keys skips password fields").is_some());
        assert!(find(&tree, Role::RadioGroup, "keystroke position").is_some());
        let modes = find(&tree, Role::RadioGroup, "keystrokes mode").expect("the mode picker");
        let options: Vec<_> = modes
            .children()
            .iter()
            .filter_map(|id| tree.iter().find(|(i, _)| i == id))
            .map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.toggled()))
            .collect();
        assert_eq!(options, [("editable".to_owned(), Some(Toggled::True)), ("burn in".to_owned(), Some(Toggled::False))]);
        assert!(find(&tree, Role::Label, "editable keystrokes can be moved or removed in the trimmer").is_some());

        let keys = h.control("keystrokes");
        h.click(pos2(keys.left() + 100.0, keys.top() + 16.0));
        assert_eq!(h.app.pages.cfg.keystrokes, Keystrokes::Shortcuts);
        let place = h.control("keystroke position");
        h.click(pos2(place.left() + 33.0, place.top() + 16.0));
        assert_eq!(h.app.pages.cfg.keystroke_position, Position::Left);
        let mode = h.control("keystrokes mode");
        h.click(pos2(mode.left() + 126.0, mode.top() + 16.0));
        assert_eq!(h.app.pages.cfg.keystrokes_mode, ClickMode::BurnIn);
        assert_eq!(Section::Recording.changes(&h.app.pages.cfg, &h.app.pages.saved), 5);
        Section::Recording.reset(&mut h.app.pages.cfg);
        assert_eq!(h.app.pages.cfg, h.app.pages.saved);
    }
}

