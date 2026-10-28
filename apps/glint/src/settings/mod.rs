//! Settings window, run as `glint.exe --settings` (see [`glint_ui::settings`] for the window):
//! it edits config.toml and asks the tray app to apply it.

use eframe::egui::{self, Ui};
use glint_core::config::Config;
use glint_core::icon::icon_rgba;
use glint_core::naming::pattern_problem;
use glint_core::paths::app_dir;
use glint_ui::audio::AudioEditor;
use glint_ui::capture::Recorder;
use glint_ui::settings::{self as frame, Pages};
use glint_ui::updates::{self, Updater};
use glint_ui::widgets::Look;

use crate::app::ipc::{self, MAIN_CLASS, SETTINGS_TITLE, WM_RELOAD};
use crate::app::updates::BUILD;
use crate::platform::paths::{output_dir, remember_previous_output_dir};

mod hotkey_capture;
mod sections;

pub use hotkey_capture::{CAPTURE_ARG, capture_keys};
use hotkey_capture::{HotkeyField, release_tray_hotkeys};
use sections::output::Mover;
use sections::toolbar::ToolbarRows;
use sections::uploads::Uploaders;
use sections::{SECTIONS, Section};

pub fn run() {
    if let Some(h) = ipc::find_settings() {
        return glint_ui::window::bring_forward(h);
    }
    let (cfg, load_error) = match Config::load() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("couldn't read the settings, showing defaults: {e}"))),
    };
    let icon = icon_rgba(64, cfg.accent_rgb());
    let app = SettingsApp::new(cfg, load_error, Updater::new(BUILD, MAIN_CLASS));
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
    /// The audio page's sources, devices and running apps.
    audio: AudioEditor,
    mover: Mover,
    /// What's typed in the custom accent field; `cfg.accent` follows it while it's a colour.
    accent_text: String,
    /// The editor toolbar page's list.
    toolbar: ToolbarRows,
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
            audio: AudioEditor::default(),
            mover: Mover::default(),
            toolbar: ToolbarRows::default(),
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
            Section::Overlays => self.overlays_page(ui, look),
            Section::Audio => self.audio(ui, look),
            Section::Screenshots => self.screenshots(ui, look),
            Section::Toolbar => self.toolbar_page(ui, look),
            Section::Uploads => self.uploads(ui, look),
            Section::Output => self.output(ui, look),
            Section::Updates => {
                updates::section(ui, look, &mut self.updater, &mut self.cfg.update_mode, &mut self.cfg.update_beta)
            }
            Section::About => self.about(ui, look),
        }
    }

    fn error(&self) -> Option<String> {
        self.error.clone()
    }

    fn can_save(&self) -> bool {
        !self.keys.listening() && !self.toolbar.listening() && pattern_problem(&self.cfg.file_name).is_none()
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
        self.toolbar.stop();
        self.audio.leave();
    }

    fn tick(&mut self) {
        self.poll_capture();
        self.toolbar.tick();
    }

    fn busy(&self) -> bool {
        self.keys.listening() || self.toolbar.busy() || self.audio.busy()
    }

    fn overlays(&mut self, ctx: &egui::Context, look: &Look) -> bool {
        const CRATES: &str = "egui, eframe, winit, windows-rs, serde, toml, gif, color_quant, minisign-verify";
        if self.licences {
            self.licences = frame::licences(ctx, look, CRATES);
        }
        self.licences
    }

    fn updater(&mut self) -> Option<&mut Updater> {
        Some(&mut self.updater)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App;
    use eframe::egui::{Color32, Key, Rect, pos2, vec2};
    use glint_ui::settings::Frame;
    use glint_ui::theme::fonts;
    use glint_ui::widgets::{self, CONTENT_X, FOOTER_H, WINDOW};

    /// Where the first row's label sits: below the title, the row's padding and `label_dy` 7.
    const TOP: f32 = 114.0;

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
            let updater = Updater::at(BUILD, MAIN_CLASS, std::env::temp_dir().join("glint-tests-no-update-status.json"));
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
        // short enough that general scrolls
        h.screen = vec2(WINDOW[0], 420.0);
        let top = h.frame(Vec::new(), "record").expect("the record hotkey");
        h.frame(vec![egui::Event::PointerMoved(pos2(400.0, 300.0))], "record");
        for _ in 0..20 {
            assert_eq!(h.frame(vec![wheel()], "record"), Some(top));
        }

        h.frame(vec![egui::Event::PointerMoved(pos2(420.0, 320.0))], "record");
        for _ in 0..20 {
            h.frame(vec![wheel()], "record");
        }
        assert!(h.frame(Vec::new(), "record").is_none_or(|y| y < top));

        // a picked section drops in, then rests at the top
        h.app.go(Section::Recording);
        h.settle();
        assert_eq!(h.frame(Vec::new(), "frame rate"), Some(TOP));
        h.app.go(Section::General);
        h.settle();
        assert_eq!(h.frame(Vec::new(), "record"), Some(top));
    }

    #[test]
    fn recording_fits_the_window_by_default() {
        let mut h = Harness::new();
        h.app.go(Section::Recording);
        h.settle();
        let last = h.control("when it stops");
        assert!(last.bottom() + 9.0 <= WINDOW[1] - FOOTER_H, "{last:?}");
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
        // the quality row's sliders are in its fine-tune inset
        h.ctx.data_mut(|d| d.insert_temp(egui::Id::unique(("fine-tune", "quality")), true));
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
                let hotkeys: Vec<_> = tree.iter().filter(|(_, n)| n.role() == Role::Button && n.value().is_some()).collect();
                assert_eq!(hotkeys.len(), 5, "{hotkeys:?}");
                let hotkey = named(&tree, Role::Button, "record");
                assert_eq!(hotkey.len(), 1);
                assert_eq!(hotkey[0].value(), Some("ctrl+alt+printscreen"));
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
                let presets = named(&tree, Role::RadioGroup, "quality");
                assert_eq!(presets.len(), 1);
                let options: Vec<_> = presets[0]
                    .children()
                    .iter()
                    .filter_map(|id| tree.iter().find(|(i, _)| i == id))
                    .map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.toggled()))
                    .collect();
                let on = |name: &str, yes: bool| (name.to_owned(), Some(if yes { Toggled::True } else { Toggled::False }));
                assert_eq!(options, [on("smaller", false), on("balanced", true), on("sharper", false)]);
                let quality = named(&tree, Role::Slider, "quality");
                assert!(quality.len() == 1 && quality[0].numeric_value() == Some(h.app.pages.cfg.quality as f64), "{quality:?}");
                let effort = named(&tree, Role::Slider, "encoder effort");
                assert!(effort.len() == 1 && effort[0].numeric_value() == Some(h.app.pages.cfg.effort as f64), "{effort:?}");
                assert_eq!(named(&tree, Role::Link, "hide details").len(), 1);
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
        toggle(&mut h, "app in the name");
        assert_eq!(h.app.pages.cfg.file_name, "{app}_{date}_{time}");
        toggle(&mut h, "folder per app");
        assert_eq!(h.app.pages.cfg.file_name, "{app}/{app}_{date}_{time}");
        toggle(&mut h, "app in the name");
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
        use glint_core::update::status::{State, Status};
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
        let version = concat!("v", env!("CARGO_PKG_VERSION"));
        let check = find(&tree, Role::Button, "check now").expect("check now");
        assert!(!check.is_disabled());
        assert!(find(&tree, Role::Label, "0.2.0 ready ·").is_some() && find(&tree, Role::Link, "restart").is_some());
        assert!(find(&tree, Role::Label, &format!("{version} · last checked 2026-10-21 14:32")).is_some());
        assert!(find(&tree, Role::Link, "what's new in 0.2.0 ↗").is_some());
        // the sidebar offers the restart too
        assert!(!find(&tree, Role::Button, "restart to update").expect("the sidebar's update button").is_disabled());
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
        assert!(find(&tree, Role::Label, "checking…").is_some());
        assert!(find(&tree, Role::Button, "restart to update").is_none(), "nothing to take: the button is gone");
        assert!(find(&tree, Role::Label, &format!("{version} · not checked yet")).is_some());
    }

    fn find(tree: &[(egui::accesskit::NodeId, egui::accesskit::Node)], role: egui::accesskit::Role, text: &str) -> Option<egui::accesskit::Node> {
        let named = |n: &egui::accesskit::Node| n.label() == Some(text) || n.value() == Some(text);
        tree.iter().find(|(_, n)| n.role() == role && named(n)).map(|(_, n)| n.clone())
    }

    /// A radio group's options as (label, selected).
    fn options(tree: &[(egui::accesskit::NodeId, egui::accesskit::Node)], group: &str) -> Vec<(String, bool)> {
        use egui::accesskit::{Role, Toggled};
        let group = find(tree, Role::RadioGroup, group).expect(group);
        group
            .children()
            .iter()
            .filter_map(|id| tree.iter().find(|(i, _)| i == id))
            .map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.toggled() == Some(Toggled::True)))
            .collect()
    }

    fn picks(items: &[(&str, bool)]) -> Vec<(String, bool)> {
        items.iter().map(|&(l, on)| (l.to_owned(), on)).collect()
    }

    fn key(key: Key) -> egui::Event {
        egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }
    }

    #[test]
    fn toolbar_rows_drag_into_order_hide_and_take_new_keys() {
        use egui::accesskit::Role;
        use glint_core::config::toolbar::{Keys, Slot, ToolKeys, default_slots};
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.screen = vec2(WINDOW[0], 1000.0);
        h.app.go(Section::Toolbar);
        h.settle();
        let at = |h: &mut Harness, role: Role, name: &str| {
            h.frame(Vec::new(), "");
            let b = find(&h.tree, role, name).and_then(|n| n.bounds()).unwrap_or_else(|| panic!("{name}"));
            pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32)
        };
        let tools = |h: &Harness| h.app.pages.cfg.toolbar.clone();
        let (t, s) = (Slot::Tool, Slot::Separator);

        // pen dragged below arrow
        let (pen, arrow) = (at(&mut h, Role::ListItem, "pen"), at(&mut h, Role::ListItem, "arrow"));
        h.drag(pen - vec2(80.0, 0.0), arrow + vec2(-80.0, 12.0));
        assert_eq!(tools(&h)[..4], [t(1), t(2), t(0), t(3)]);
        assert_eq!(Section::Toolbar.changes(&h.app.pages.cfg, &h.app.pages.saved), 1);

        // pixelate hidden: it's left out, its row stays where it was
        let pixelate = at(&mut h, Role::CheckBox, "show pixelate");
        h.click(pixelate);
        assert!(!tools(&h).contains(&t(5)));
        let mut rows: Vec<(f64, String)> = h
            .tree
            .iter()
            .filter(|(_, n)| n.role() == Role::ListItem)
            .filter_map(|(_, n)| Some((n.bounds()?.y0, n.label()?.to_owned())))
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(rows[5].1, "pixelate", "{rows:?}");

        // pen's letter: Q is refused, E is eraser's, so they swap; Esc cancels without closing
        let letter = at(&mut h, Role::Button, "pen letter");
        h.click(letter);
        assert!(h.app.pages.busy());
        h.frame(vec![key(Key::Q)], "");
        assert!(h.app.pages.toolbar.listening(), "still waiting after Q");
        h.frame(vec![key(Key::E)], "");
        h.settle();
        let keys = |h: &Harness, t: usize| h.app.pages.cfg.toolbar_keys.0[t];
        assert_eq!((keys(&h, 0).letter, keys(&h, 6).letter), (Some(b'E'), Some(b'P')));
        assert!(find(&h.tree, Role::Label, "smart eraser has p now").is_some());
        let digit = at(&mut h, Role::Button, "grab digit");
        h.click(digit);
        h.frame(vec![key(Key::Escape)], "");
        h.settle();
        assert!(!h.app.pages.busy() && !h.app.closing() && !h.app.asking(), "esc only cancels");
        assert_eq!(keys(&h, 10), Keys { digit: None, letter: Some(b'G') });
        h.click(digit);
        h.frame(vec![key(Key::Num6)], "");
        h.settle();
        assert_eq!((keys(&h, 10).digit, keys(&h, 5).digit), (Some(b'6'), None), "pixelate's 6 moved to grab");

        // a separator comes in at the top, then goes down between arrow and pen
        let add = at(&mut h, Role::Link, "add separator");
        h.click(add);
        h.settle();
        let (sep, pen) = (at(&mut h, Role::ListItem, "separator"), at(&mut h, Role::ListItem, "pen"));
        h.drag(sep - vec2(80.0, 0.0), pen - vec2(80.0, 14.0));
        assert_eq!(tools(&h)[..5], [t(1), t(2), s, t(0), t(3)]);

        Section::Toolbar.reset(&mut h.app.pages.cfg);
        h.settle();
        assert_eq!((tools(&h), h.app.pages.cfg.toolbar_keys), (default_slots(), ToolKeys::default()));
        assert_eq!(h.tree.iter().filter(|(_, n)| n.role() == Role::ListItem).count(), 12, "the separator went with the reset");
    }

    #[test]
    fn digits_switch_sections_but_not_while_typing_or_binding() {
        use egui::Modifiers;
        use egui::accesskit::Role;
        // a key pressed, what it types, and let go (or egui takes the next press for a repeat)
        let tap = |key, modifiers, text: &str| {
            let key_event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            let typed = (!text.is_empty()).then(|| egui::Event::Text(text.into()));
            [Some(key_event(true)), typed, Some(key_event(false))].into_iter().flatten().collect::<Vec<_>>()
        };
        let (none, ctrl) = (Modifiers::NONE, Modifiers::CTRL | Modifiers::COMMAND);
        let mut h = Harness::new();
        let at = |h: &Harness| h.app.page().name();
        h.ctx.enable_accesskit();
        h.screen = vec2(WINDOW[0], 1000.0);
        h.settle();
        h.frame(tap(Key::Num3, none, "3"), "");
        h.settle();
        assert_eq!(at(&h), "overlays");
        h.frame(tap(Key::Num5, ctrl, ""), "");
        assert_eq!(at(&h), "screenshots");
        h.frame(tap(Key::Num0, none, "0"), "");
        assert_eq!(at(&h), "about", "0 is the tenth");
        h.frame(tap(Key::Num8, none, "8"), "");
        h.settle();
        assert_eq!(at(&h), "output");

        // a digit typed into the folder field stays there; ctrl+digit still switches, keeping it
        let folder = h.control("folder");
        h.click(pos2(folder.left() + 40.0, folder.top() + 16.0));
        assert!(h.ctx.text_edit_focused());
        h.frame(tap(Key::Num3, none, "3"), "");
        h.settle();
        assert_eq!((at(&h), h.app.pages.cfg.output_dir.as_str()), ("output", "3"));
        h.frame(tap(Key::Num6, ctrl, ""), "");
        h.settle();
        assert_eq!(at(&h), "toolbar");
        assert_eq!((h.app.pages.cfg.output_dir.as_str(), Section::Output.changes(&h.app.pages.cfg, &h.app.pages.saved)), ("3", 1));

        // binding a tool's digit takes the digit, bare or with ctrl
        let b = find(&h.tree, Role::Button, "grab digit").and_then(|n| n.bounds()).expect("grab digit");
        let digit = pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
        h.click(digit);
        assert!(h.app.pages.toolbar.listening());
        h.frame(tap(Key::Num2, ctrl, ""), "");
        h.frame(tap(Key::Num2, none, "2"), "");
        h.settle();
        assert_eq!((at(&h), h.app.pages.cfg.toolbar_keys.0[10].digit), ("toolbar", Some(b'2')));
        h.frame(tap(Key::Num2, none, "2"), "");
        assert_eq!(at(&h), "recording", "bound, digits switch again");
    }

    #[test]
    fn overlays_show_only_the_rows_that_apply_and_count_as_changes() {
        use egui::accesskit::{Role, Toggled};
        use glint_core::config::{ClickMode, Keystrokes};
        use glint_core::record::keys::Position;
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.app.go(Section::Overlays);
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        let toggle = find(&tree, Role::CheckBox, "click highlights").expect("the toggle");
        assert_eq!((toggle.toggled(), toggle.description()), (Some(Toggled::False), Some("off")));
        assert_eq!(options(&tree, "keystrokes"), picks(&[("off", true), ("shortcuts", false), ("all keys", false)]));
        assert!(find(&tree, Role::Label, "no keys are shown").is_some());
        assert!(find(&tree, Role::RadioGroup, "position").is_none());
        assert!(find(&tree, Role::RadioGroup, "in the video").is_none());
        assert!(find(&tree, Role::Label, "more options appear when click highlights or keystrokes are on").is_some());

        let c = h.control("click highlights");
        h.click(pos2(c.left() + 18.0, c.top() + 10.0));
        assert!(h.app.pages.cfg.click_highlights);
        let tree = std::mem::take(&mut h.tree);
        let toggle = find(&tree, Role::CheckBox, "click highlights").expect("the toggle");
        assert_eq!((toggle.toggled(), toggle.description()), (Some(Toggled::True), Some("a ripple on every click")));
        assert_eq!(options(&tree, "in the video"), picks(&[("editable", true), ("burn in", false)]));
        assert!(find(&tree, Role::Label, "clicks can be moved or removed in the trimmer").is_some());
        assert!(find(&tree, Role::RadioGroup, "position").is_none());
        assert!(find(&tree, Role::Label, "more options appear when click highlights or keystrokes are on").is_none());

        // one choice sets both overlays' modes
        let picker = h.control("in the video");
        h.click(pos2(picker.left() + 126.0, picker.top() + 16.0));
        assert_eq!((h.app.pages.cfg.clicks, h.app.pages.cfg.keystrokes_mode), (ClickMode::BurnIn, ClickMode::BurnIn));

        let keys = h.control("keystrokes");
        h.click(pos2(keys.left() + 200.0, keys.top() + 16.0));
        assert_eq!(h.app.pages.cfg.keystrokes, Keystrokes::All);
        let tree = std::mem::take(&mut h.tree);
        assert!(find(&tree, Role::Label, "everything you type, except password fields").is_some());
        assert_eq!(options(&tree, "position"), picks(&[("left", false), ("centre", true), ("right", false)]));
        assert!(find(&tree, Role::Label, "drawn into the video; can't be changed later").is_some());

        let place = h.control("position");
        h.click(pos2(place.left() + 33.0, place.top() + 16.0));
        assert_eq!(h.app.pages.cfg.keystroke_position, Position::Left);
        assert_eq!(Section::Overlays.changes(&h.app.pages.cfg, &h.app.pages.saved), 4);
        assert_eq!(Section::Recording.changes(&h.app.pages.cfg, &h.app.pages.saved), 0);

        // a config whose modes differ says so
        h.app.pages.cfg.keystrokes_mode = ClickMode::Editable;
        h.settle();
        let tree = std::mem::take(&mut h.tree);
        assert!(find(&tree, Role::Label, "clicks burnt in, keys editable").is_some());

        Section::Overlays.reset(&mut h.app.pages.cfg);
        assert_eq!(h.app.pages.cfg, h.app.pages.saved);
    }

    #[test]
    fn quality_presets_set_both_values_and_fine_tuning_goes_custom() {
        use egui::accesskit::Role;
        let mut h = Harness::new();
        h.ctx.enable_accesskit();
        h.app.go(Section::Recording);
        h.settle();
        let presets = h.control("quality");
        h.click(pos2(presets.right() - 40.0, presets.top() + 16.0));
        assert_eq!((h.app.pages.cfg.quality, h.app.pages.cfg.effort), (20, 100));
        let tree = std::mem::take(&mut h.tree);
        assert_eq!(options(&tree, "quality"), picks(&[("smaller", false), ("balanced", false), ("sharper", true)]));
        assert!(find(&tree, Role::Label, "very sharp, larger files, more gpu load").is_some());
        assert!(find(&tree, Role::Slider, "quality").is_none());

        let link = find(&tree, Role::Link, "fine-tune").and_then(|n| n.bounds()).expect("the fine-tune link");
        h.click(pos2(((link.x0 + link.x1) / 2.0) as f32, ((link.y0 + link.y1) / 2.0) as f32));
        let tree = std::mem::take(&mut h.tree);
        let slider = find(&tree, Role::Slider, "quality").and_then(|n| n.bounds()).expect("the quality slider");
        assert!(find(&tree, Role::Link, "hide details").is_some());
        // the track's left end is quality 12
        h.click(pos2(slider.x0 as f32 + 1.0, ((slider.y0 + slider.y1) / 2.0) as f32));
        assert_eq!((h.app.pages.cfg.quality, h.app.pages.cfg.effort), (12, 100));
        let tree = std::mem::take(&mut h.tree);
        let custom = [("smaller", false), ("balanced", false), ("sharper", false), ("custom", true)];
        assert_eq!(options(&tree, "quality"), picks(&custom));
        assert!(find(&tree, Role::Label, "quality 12, effort 100").is_some());
        assert_eq!(Section::Recording.changes(&h.app.pages.cfg, &h.app.pages.saved), 2);

        let presets = h.control("quality");
        h.click(pos2(presets.left() + 40.0, presets.top() + 16.0));
        assert_eq!((h.app.pages.cfg.quality, h.app.pages.cfg.effort), (28, 50));
        Section::Recording.reset(&mut h.app.pages.cfg);
        assert_eq!(h.app.pages.cfg, h.app.pages.saved);
    }
}

