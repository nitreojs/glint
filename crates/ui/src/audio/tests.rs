use super::*;
use crate::theme::fonts;
use crate::widgets::palette;
use eframe::egui::{Event, PointerButton, RawInput};

fn app(exe: &str, audio: bool) -> App {
    App { exe: exe.into(), path: None, title: String::new(), audio, via: Vec::new(), copies: 1 }
}

fn devices() -> Devices {
    let mut d = Devices::empty();
    d.apps = vec![app("Spotify.exe", true), app("chrome.exe", false), app("Deadlock.exe", false), app("Discord.exe", true)];
    d.looks.insert("chrome.exe".into(), AppLook { name: "Google Chrome".into(), icon: None });
    d.mics = vec![
        Microphone { id: "a".into(), name: "Microphone (Shure MV7)".into(), label: "Shure MV7".into() },
        Microphone { id: "b".into(), name: "Headset (Arctis)".into(), label: "Arctis".into() },
    ];
    d.default_mic = Some("a".into());
    d
}

fn src(kind: SourceKind, tracks: &[u32], app: &str) -> AudioSource {
    AudioSource { tracks: tracks.to_vec(), app: app.into(), ..AudioSource::new(kind, 0) }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| n.to_string()).collect()
}

fn listed(c: &[Choice]) -> Vec<(&str, &str)> {
    c.iter().map(|c| (c.name.as_str(), c.note.as_str())).collect()
}

#[test]
fn the_picker_lists_what_isnt_recorded_yet_by_name_with_sound_first() {
    let d = devices();
    let none = choices(&d, &[], Target::Add, "");
    assert_eq!(
        listed(&none),
        [
            ("desktop audio", ""),
            ("Shure MV7", "default"),
            ("Arctis", ""),
            ("Discord", ""),
            ("Spotify", ""),
            ("Deadlock", ""),
            ("Google Chrome", "chrome.exe"),
        ]
    );
    assert!(none[3].audio && !none[5].audio);

    let some = [src(SourceKind::Desktop, &[1], ""), src(SourceKind::Mic, &[1], ""), src(SourceKind::App, &[1], "discord.EXE")];
    assert_eq!(listed(&choices(&d, &some, Target::Add, "")), [("Arctis", ""), ("Spotify", ""), ("Deadlock", ""), ("Google Chrome", "chrome.exe")]);
    assert_eq!(listed(&choices(&d, &some, Target::Add, "goo")), [("Google Chrome", "chrome.exe"), ("goo.exe", "not running")]);
    assert_eq!(listed(&choices(&d, &some, Target::Add, " Chrome.exe")), [("Google Chrome", "chrome.exe")], "a running exe isn't offered twice");
    assert!(choices(&d, &some, Target::Add, "discord").is_empty(), "already recorded");
    assert_eq!(listed(&choices(&d, &some, Target::Add, "mic")), [("Arctis", ""), ("mic.exe", "not running")]);
    // leaving an app out of desktop audio: only apps, even ones recorded on their own
    assert_eq!(listed(&choices(&d, &some, Target::Exclude(0), "disc")), [("Discord", ""), ("disc.exe", "not running")]);

    let picked = source_of(&none[6].pick);
    assert_eq!((picked.kind, picked.app.as_str(), picked.name.as_str()), (SourceKind::App, "chrome.exe", "Google Chrome"));
    let plain = source_of(&none[4].pick);
    assert_eq!((plain.app.as_str(), plain.name.as_str()), ("Spotify.exe", ""), "a name that says no more than the exe isn't kept");
    assert_eq!(source_of(&none[1].pick).device, "", "the default microphone follows Windows' default");
    assert_eq!(source_of(&none[2].pick).device, "b");
}

struct Harness {
    ctx: Context,
    editor: AudioEditor,
    sources: Vec<AudioSource>,
    names: Vec<String>,
    time: f64,
}

impl Harness {
    fn new(sources: Vec<AudioSource>, names: Vec<String>) -> Harness {
        let ctx = Context::default();
        fonts(&ctx);
        let editor = AudioEditor { devices: Some(devices()), ..AudioEditor::default() };
        let mut h = Harness { ctx, editor, sources, names, time: 0.0 };
        h.settle();
        h
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 1.0 / 60.0;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(620.0, 1200.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let look = Look { p: palette(false, [0xE5, 0x40, 0x2F]), keyboard: false };
        let (editor, sources, names) = (&mut self.editor, &mut self.sources, &mut self.names);
        let _ = self.ctx.run_ui(input, |ui| editor.show(ui, &look, sources, names));
    }

    fn settle(&mut self) {
        for _ in 0..40 {
            self.frame(Vec::new());
        }
    }

    /// The middle of row `i`'s name.
    fn header(&self, i: usize) -> Pos2 {
        let rows = &self.editor.rows;
        pos2(148.0 + 120.0, rows.top + rows.tops[&rows.keys[i]] + HEADER_H / 2.0)
    }

    fn drag(&mut self, from: Pos2, to: Pos2) {
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        self.frame(vec![Event::PointerMoved(from), button(from, true)]);
        for t in 1..=8 {
            self.frame(vec![Event::PointerMoved(from.lerp(to, t as f32 / 8.0))]);
        }
        self.frame(vec![button(to, false)]);
        self.settle();
    }

    fn labels(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.label(None)).collect()
    }
}

#[test]
fn dragging_a_row_up_takes_its_track_and_name_along() {
    let sources = vec![src(SourceKind::App, &[1], "spotify.exe"), src(SourceKind::App, &[2], "deadlock.exe"), src(SourceKind::App, &[3], "Discord.exe")];
    let mut h = Harness::new(sources, names(&["music", "games", "chat", "", "", ""]));
    let tops: Vec<f32> = (0..3).map(|i| h.header(i).y).collect();
    assert!(tops[0] < tops[1] && tops[1] < tops[2], "{tops:?}");

    let (from, to) = (h.header(2), h.header(0) - vec2(0.0, HEADER_H / 2.0 + 4.0));
    h.drag(from, to);
    assert_eq!(h.labels(), ["Discord", "spotify", "deadlock"]);
    assert_eq!(h.sources.iter().map(|s| s.tracks.clone()).collect::<Vec<_>>(), [vec![1], vec![2], vec![3]]);
    assert_eq!(h.names, names(&["chat", "music", "games", "", "", ""]));
    let settled: Vec<f32> = (0..3).map(|i| h.header(i).y).collect();
    assert_eq!(settled, tops, "the rows end up where rows go");

    // a short drag that doesn't pass the next row's middle leaves the order alone
    let at = h.header(0);
    h.drag(at, at + vec2(0.0, 10.0));
    assert_eq!(h.labels(), ["Discord", "spotify", "deadlock"]);
}

#[test]
fn a_click_opens_a_row_and_the_picker_adds_a_finished_one() {
    let sources = vec![src(SourceKind::Desktop, &[1, 2], ""), src(SourceKind::Mic, &[1, 3], "")];
    let mut h = Harness::new(sources, names(&["", "", "", "old", "", ""]));
    let closed = h.header(1).y;
    let at = h.header(0);
    h.drag(at, at);
    assert!(h.editor.rows.open.contains(&h.editor.rows.keys[0]));
    assert!(h.header(1).y > closed + 60.0, "the opened row pushes the next one down");

    h.editor.pickers.open = Some(Picker { target: Target::Add, focus: true, ..Picker::default() });
    h.settle();
    assert!(h.editor.busy());
    h.frame(vec![Event::Text("dead".into())]);
    let enter = Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    h.frame(vec![enter]);
    h.settle();
    assert!(!h.editor.busy());
    assert_eq!(h.labels(), ["desktop", "mic", "Deadlock"]);
    assert_eq!(h.sources[2].tracks, [1, 4], "the mix and a track of its own");
    assert_eq!(h.names[3], "", "the track it got starts unnamed");

    h.editor.pickers.open = Some(Picker { target: Target::Add, focus: true, ..Picker::default() });
    h.settle();
    let esc = Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    h.frame(vec![esc]);
    assert!(h.editor.pickers.open.is_none() && h.editor.busy(), "esc closed the picker, and was the picker's");
    h.frame(Vec::new());
    assert!(!h.editor.busy());
}
