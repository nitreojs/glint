use serde::Deserialize;

pub const MAX_TRACKS: u32 = 6;
pub const MAX_VOLUME: u32 = 200;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Desktop,
    App,
    Mic,
}

impl SourceKind {
    pub fn name(self) -> &'static str {
        match self {
            SourceKind::Desktop => "desktop",
            SourceKind::App => "app",
            SourceKind::Mic => "mic",
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AudioSource {
    pub kind: SourceKind,
    pub enabled: bool,
    pub volume: u32,
    pub tracks: Vec<u32>,
    pub app: String,
    pub exclude: String,
    pub device: String,
    /// What the app (an app source's, or the one a desktop source leaves out) is called, as its
    /// exe describes itself; empty names it after the exe.
    pub name: String,
}

impl Default for AudioSource {
    fn default() -> Self {
        AudioSource { tracks: vec![1], ..AudioSource::new(SourceKind::Desktop, 0) }
    }
}

impl AudioSource {
    pub fn new(kind: SourceKind, nth: usize) -> AudioSource {
        let own = nth as u32 + 2;
        let tracks = if own <= MAX_TRACKS { vec![1, own] } else { vec![1] };
        AudioSource {
            kind,
            enabled: true,
            volume: 100,
            tracks,
            app: String::new(),
            exclude: String::new(),
            device: String::new(),
            name: String::new(),
        }
    }

    pub fn label(&self, device_name: Option<&str>) -> String {
        match self.kind {
            SourceKind::Desktop if app_name(&self.exclude).is_empty() => "desktop".into(),
            SourceKind::Desktop => format!("desktop without {}", self.app_label()),
            SourceKind::App => self.app_label(),
            SourceKind::Mic => match device_name {
                Some(name) => format!("mic: {name}"),
                None => "mic".into(),
            },
        }
    }

    /// The app's own name, or its exe's without ".exe": "Discord", "deadlock".
    pub fn app_label(&self) -> String {
        let exe = if self.kind == SourceKind::Desktop { &self.exclude } else { &self.app };
        match self.name.trim() {
            "" => exe_stem(exe).to_string(),
            name => name.to_string(),
        }
    }

    pub fn records(&self) -> bool {
        let unnamed = self.kind == SourceKind::App && app_name(&self.app).is_empty();
        !self.tracks.is_empty() && !unnamed
    }

    pub(crate) fn problem(&self) -> Option<String> {
        if self.volume > MAX_VOLUME {
            return Some(format!("volume must be 0-{MAX_VOLUME}, got {}", self.volume));
        }
        if let Some(t) = self.tracks.iter().find(|t| !(1..=MAX_TRACKS).contains(*t)) {
            return Some(format!("tracks must be 1-{MAX_TRACKS}, got {t}"));
        }
        None
    }

    fn render(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let tracks: Vec<String> = self.tracks.iter().map(u32::to_string).collect();
        let mut line = format!(
            "{{ kind = \"{}\", enabled = {}, volume = {}, tracks = [{}]",
            self.kind.name(),
            self.enabled,
            self.volume,
            tracks.join(", ")
        );
        match self.kind {
            SourceKind::Desktop if !self.exclude.is_empty() => line += &format!(", exclude = {}", q(&self.exclude)),
            SourceKind::App => line += &format!(", app = {}", q(&self.app)),
            SourceKind::Mic if !self.device.is_empty() => line += &format!(", device = {}", q(&self.device)),
            _ => {}
        }
        if self.kind != SourceKind::Mic && !self.name.is_empty() {
            line += &format!(", name = {}", q(&self.name));
        }
        line + " }"
    }
}

pub fn app_name(exe: &str) -> String {
    exe.trim().to_lowercase()
}

/// `exe` without its folder and ".exe".
pub fn exe_stem(exe: &str) -> &str {
    let file = exe.trim().rsplit(['\\', '/']).next().unwrap_or("");
    match file.len().checked_sub(4) {
        Some(cut) if file.is_char_boundary(cut) && file[cut..].eq_ignore_ascii_case(".exe") => &file[..cut],
        _ => file,
    }
}

pub(crate) fn default_sources() -> Vec<AudioSource> {
    let desktop = AudioSource { enabled: false, ..AudioSource::new(SourceKind::Desktop, 0) };
    let mic = AudioSource { enabled: false, ..AudioSource::new(SourceKind::Mic, 1) };
    vec![desktop, mic]
}

pub(crate) fn render_sources(sources: &[AudioSource]) -> String {
    if sources.is_empty() {
        return "audio_sources = []".into();
    }
    let lines: Vec<String> = sources.iter().map(|s| format!("    {},\n", s.render())).collect();
    format!("audio_sources = [\n{}]", lines.concat())
}

pub fn track_name(names: &[String], n: u32) -> &str {
    names.get(n as usize - 1).map_or("", |s| s.trim())
}

/// Sets track `n`'s name; an empty one only clears what's there, so the list grows only for names.
fn set_track_name(names: &mut Vec<String>, n: u32, name: &str) {
    let i = n as usize - 1;
    if name.is_empty() {
        if let Some(old) = names.get_mut(i) {
            old.clear();
        }
        return;
    }
    if names.len() <= i {
        names.resize(i + 1, String::new());
    }
    names[i] = name.to_string();
}

fn on_track(sources: &[AudioSource], n: u32) -> usize {
    sources.iter().filter(|s| s.tracks.contains(&n)).count()
}

/// The tracks source `i` has to itself, in order.
fn own_tracks(sources: &[AudioSource], i: usize) -> Vec<u32> {
    let mut own: Vec<u32> = sources[i].tracks.iter().copied().filter(|&n| on_track(sources, n) == 1).collect();
    own.sort_unstable();
    own.dedup();
    own
}

/// Adds `s` after the others, on the tracks a newcomer belongs on: track 1 while it holds
/// everything, any other track all of them share, and a track of its own when the others have
/// one (or there are no others yet). That track starts without a name.
pub fn add_source(sources: &mut Vec<AudioSource>, names: &mut Vec<String>, mut s: AudioSource) {
    let mut tracks: Vec<u32> = match sources.is_empty() {
        true => vec![1],
        false => (1..=MAX_TRACKS)
            .filter(|&n| sources.iter().all(|s| s.tracks.contains(&n)) && (n == 1 || sources.len() > 1))
            .collect(),
    };
    let wants_own = sources.is_empty() || (0..sources.len()).any(|i| !own_tracks(sources, i).is_empty());
    let taken = |n: u32| tracks.contains(&n) || on_track(sources, n) > 0;
    let last = (1..=MAX_TRACKS).filter(|&n| taken(n)).max().unwrap_or(0);
    let free = (last + 1..=MAX_TRACKS).chain(1..=last).find(|&n| !taken(n));
    if let Some(n) = free.filter(|_| wants_own) {
        set_track_name(names, n, "");
        tracks.push(n);
    }
    tracks.sort_unstable();
    s.tracks = tracks;
    sources.push(s);
}

/// Removes source `i`; tracks nobody's on any more lose their names.
pub fn remove_source(sources: &mut Vec<AudioSource>, names: &mut Vec<String>, i: usize) {
    let gone = sources.remove(i);
    for &n in &gone.tracks {
        if on_track(sources, n) == 0 {
            set_track_name(names, n, "");
        }
    }
}

/// Moves source `from` to `to`. Tracks a source has to itself keep the list's order, so they're
/// renumbered and carry their names along; shared tracks stay as they are.
pub fn move_source(sources: &mut Vec<AudioSource>, names: &mut Vec<String>, from: usize, to: usize) {
    if from == to || from >= sources.len() || to >= sources.len() {
        return;
    }
    let mut own: Vec<Vec<u32>> = (0..sources.len()).map(|i| own_tracks(sources, i)).collect();
    let mut pool: Vec<u32> = own.concat();
    pool.sort_unstable();
    let mut carried: Vec<Vec<String>> =
        own.iter().map(|o| o.iter().map(|&n| track_name(names, n).to_string()).collect()).collect();
    let moved = sources.remove(from);
    sources.insert(to, moved);
    let moved = own.remove(from);
    own.insert(to, moved);
    let moved = carried.remove(from);
    carried.insert(to, moved);
    let mut pool = pool.into_iter();
    for ((s, old), labels) in sources.iter_mut().zip(&own).zip(&carried) {
        let new: Vec<u32> = pool.by_ref().take(old.len()).collect();
        s.tracks.retain(|n| !old.contains(n));
        s.tracks.extend(&new);
        s.tracks.sort_unstable();
        for (&n, name) in new.iter().zip(labels) {
            set_track_name(names, n, name);
        }
    }
}

/// Puts source `i` on track `n` or takes it off; a track that starts or ends this way has no name.
pub fn set_track(sources: &mut [AudioSource], names: &mut Vec<String>, i: usize, n: u32, on: bool) {
    if on_track(sources, n) == 0 {
        set_track_name(names, n, "");
    }
    let t = &mut sources[i].tracks;
    t.retain(|&k| k != n);
    if on {
        t.push(n);
        t.sort_unstable();
    }
    if on_track(sources, n) == 0 {
        set_track_name(names, n, "");
    }
}

pub(crate) fn render_track_names(names: &[String]) -> String {
    let all: Vec<String> = (1..=MAX_TRACKS).map(|n| toml::Value::String(track_name(names, n).into()).to_string()).collect();
    format!("audio_track_names = [{}]", all.join(", "))
}

const LEGACY: [&str; 2] = ["audio_desktop", "audio_mic"];

pub(crate) fn is_legacy(table: &toml::Table) -> bool {
    LEGACY.iter().any(|k| table.contains_key(*k))
}

pub(crate) fn migrate(table: &mut toml::Table) {
    let on = |v: Option<toml::Value>| v.and_then(|v| v.as_bool()).unwrap_or(false);
    let desktop = on(table.remove(LEGACY[0]));
    let mic = on(table.remove(LEGACY[1]));
    if table.contains_key("audio_sources") {
        return;
    }
    let mut sources = default_sources();
    sources[0].enabled = desktop;
    sources[1].enabled = mic;
    let text = render_sources(&sources);
    if let Ok(mut parsed) = toml::from_str::<toml::Table>(&text) {
        table.extend(std::mem::take(&mut parsed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_render_and_parse_back() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct Wrap {
            audio_sources: Vec<AudioSource>,
        }
        let sources = vec![
            AudioSource { exclude: "game \"x\".exe".into(), ..AudioSource::new(SourceKind::Desktop, 0) },
            AudioSource { app: "discord.exe".into(), name: "Discord".into(), volume: 150, ..AudioSource::new(SourceKind::App, 1) },
            AudioSource { device: "{0.0.1.00000000}.{abc}".into(), enabled: false, ..AudioSource::new(SourceKind::Mic, 5) },
        ];
        let back: Wrap = toml::from_str(&render_sources(&sources)).unwrap();
        assert_eq!(back.audio_sources, sources);
        assert_eq!(sources[2].tracks, [1]);
        let empty: Wrap = toml::from_str(&render_sources(&[])).unwrap();
        assert!(empty.audio_sources.is_empty());
    }

    #[test]
    fn track_names_render_and_parse_back() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct Wrap {
            audio_track_names: Vec<String>,
        }
        let names: Vec<String> = ["all", "", "game \"x\"", "", "", "voice"].iter().map(|s| s.to_string()).collect();
        let back: Wrap = toml::from_str(&render_track_names(&names)).unwrap();
        assert_eq!(back.audio_track_names, names);
        let short: Wrap = toml::from_str(&render_track_names(&[" all ".into()])).unwrap();
        assert_eq!(short.audio_track_names, ["all", "", "", "", "", ""]);
        assert_eq!(track_name(&[], 6), "");
        assert_eq!(track_name(&names, 3), "game \"x\"");
    }

    #[test]
    fn legacy_keys_become_sources() {
        let mut t: toml::Table = toml::from_str("audio_desktop = true\naudio_mic = false\nfps = 60").unwrap();
        assert!(is_legacy(&t));
        migrate(&mut t);
        assert!(!is_legacy(&t));
        let sources: Vec<AudioSource> = t["audio_sources"].clone().try_into().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!((sources[0].kind, sources[0].enabled, &sources[0].tracks), (SourceKind::Desktop, true, &vec![1, 2]));
        assert_eq!((sources[1].kind, sources[1].enabled, &sources[1].tracks), (SourceKind::Mic, false, &vec![1, 3]));
        assert_eq!(t["fps"].as_integer(), Some(60));

        let mut t: toml::Table =
            toml::from_str("audio_mic = true\naudio_sources = [{ kind = \"app\", app = \"a.exe\" }]").unwrap();
        migrate(&mut t);
        let sources: Vec<AudioSource> = t["audio_sources"].clone().try_into().unwrap();
        assert_eq!((sources.len(), sources[0].kind, sources[0].tracks.as_slice()), (1, SourceKind::App, &[1][..]));
    }

    #[test]
    fn problems_are_reported() {
        let ok = AudioSource::new(SourceKind::Desktop, 0);
        assert_eq!(ok.problem(), None);
        assert!(AudioSource { volume: 201, ..ok.clone() }.problem().is_some());
        assert!(AudioSource { tracks: vec![7], ..ok.clone() }.problem().is_some());
        assert!(!AudioSource { kind: SourceKind::App, ..ok.clone() }.records());
        assert!(!AudioSource { tracks: vec![], ..ok.clone() }.records());
        assert!(ok.records());
        let app = AudioSource { kind: SourceKind::App, app: " Discord.EXE".into(), ..ok };
        assert_eq!(app.label(None), "Discord");
        assert_eq!(AudioSource { name: "Discord Canary".into(), ..app }.label(None), "Discord Canary");
        let desktop = AudioSource { exclude: "Game.exe".into(), ..AudioSource::new(SourceKind::Desktop, 0) };
        assert_eq!(desktop.label(None), "desktop without Game");
        assert_eq!(AudioSource { name: "The Game".into(), ..desktop }.label(None), "desktop without The Game");
        assert_eq!(AudioSource::new(SourceKind::Mic, 0).label(Some("Shure MV7")), "mic: Shure MV7");
        assert_eq!((exe_stem(r"C:\Games\x.y.EXE"), exe_stem("spotify"), exe_stem(".exe")), ("x.y", "spotify", ""));
    }

    use SourceKind::{App, Desktop, Mic};

    fn src(kind: SourceKind, tracks: &[u32], app: &str) -> AudioSource {
        AudioSource { tracks: tracks.to_vec(), app: app.into(), ..AudioSource::new(kind, 0) }
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn layout(sources: &[AudioSource]) -> Vec<(String, Vec<u32>)> {
        sources.iter().map(|s| (s.label(None), s.tracks.clone())).collect()
    }

    fn of(pairs: &[(&str, &[u32])]) -> Vec<(String, Vec<u32>)> {
        pairs.iter().map(|(l, t)| (l.to_string(), t.to_vec())).collect()
    }

    #[test]
    fn a_new_source_joins_the_mix_and_gets_a_free_track_of_its_own() {
        let (mut s, mut n) = (Vec::new(), names(&[]));
        add_source(&mut s, &mut n, src(Desktop, &[], ""));
        add_source(&mut s, &mut n, src(Mic, &[], ""));
        assert_eq!(layout(&s), of(&[("desktop", &[1, 2]), ("mic", &[1, 3])]), "the defaults' layout");

        // the old bug: a newcomer's track came from its position, so after a removal it landed on
        // a track another source already had, under that track's old name
        let mut s = vec![src(Desktop, &[1, 2], ""), src(App, &[1, 3], "a.exe"), src(Mic, &[1, 4], "")];
        let mut n = names(&["all", "speakers", "a", "voice", "stale", ""]);
        remove_source(&mut s, &mut n, 0);
        assert_eq!(n, names(&["all", "", "a", "voice", "stale", ""]), "track 2 is gone, so is its name");
        add_source(&mut s, &mut n, src(App, &[], "b.exe"));
        assert_eq!(layout(&s), of(&[("a", &[1, 3]), ("mic", &[1, 4]), ("b", &[1, 5])]));
        assert_eq!(n[4], "", "a track a newcomer gets starts without a name");
        add_source(&mut s, &mut n, src(App, &[], "c.exe"));
        add_source(&mut s, &mut n, src(App, &[], "d.exe"));
        add_source(&mut s, &mut n, src(App, &[], "e.exe"));
        assert_eq!(s[3..].iter().map(|s| s.tracks.clone()).collect::<Vec<_>>(), [vec![1, 6], vec![1, 2], vec![1]]);
    }

    #[test]
    fn a_new_source_follows_the_layout_it_joins() {
        // one source alone: it gets mixed with the newcomer on track 1, the newcomer gets its own
        let (mut s, mut n) = (vec![src(Desktop, &[1], "")], names(&[]));
        add_source(&mut s, &mut n, src(Mic, &[], ""));
        assert_eq!(layout(&s), of(&[("desktop", &[1]), ("mic", &[1, 2])]));
        // everything on one track: so is the newcomer
        let mut s = vec![src(Desktop, &[1], ""), src(Mic, &[1], "")];
        add_source(&mut s, &mut n, src(App, &[], "a.exe"));
        assert_eq!(s[2].tracks, [1]);
        // a track each, no mix: the newcomer gets a track of its own and nothing else
        let mut s = vec![src(App, &[1], "spotify.exe"), src(App, &[2], "game.exe"), src(App, &[3], "discord.exe")];
        let mut n = names(&["music", "games", "chat", "old"]);
        add_source(&mut s, &mut n, src(Mic, &[], ""));
        assert_eq!(s[3].tracks, [4]);
        assert_eq!(n, names(&["music", "games", "chat", ""]));
        // a mix of two of three isn't everyone's: a newcomer stays out of it
        let mut s = vec![src(Desktop, &[1, 2], ""), src(Mic, &[1, 3], ""), src(App, &[4], "x.exe")];
        add_source(&mut s, &mut n, src(App, &[], "y.exe"));
        assert_eq!(s[3].tracks, [5]);
    }

    #[test]
    fn reordering_keeps_each_sources_tracks_and_names_with_it() {
        let mut s = vec![src(Desktop, &[1, 2], ""), src(App, &[1, 3], "game.exe"), src(Mic, &[1, 4], "")];
        let mut n = names(&["all", "speakers", "game", "voice", "", ""]);
        let before = (layout(&s), n.clone());
        move_source(&mut s, &mut n, 2, 0);
        assert_eq!(layout(&s), of(&[("mic", &[1, 2]), ("desktop", &[1, 3]), ("game", &[1, 4])]));
        assert_eq!(n, names(&["all", "voice", "speakers", "game", "", ""]), "names follow their sources, the mix stays");
        move_source(&mut s, &mut n, 0, 2);
        assert_eq!((layout(&s), n.clone()), before, "moving back undoes it");

        // the user's layout: a track each, named by hand; dragging Discord up takes "chat" along
        let mut s = vec![src(App, &[1], "spotify.exe"), src(App, &[2], "deadlock.exe"), src(App, &[3], "Discord.exe")];
        let mut n = names(&["music", "games", "chat", "", "", ""]);
        move_source(&mut s, &mut n, 2, 0);
        assert_eq!(layout(&s), of(&[("Discord", &[1]), ("spotify", &[2]), ("deadlock", &[3])]));
        assert_eq!(n, names(&["chat", "music", "games", "", "", ""]));

        // tracks nobody has alone don't move; a source with none of its own keeps none
        let mut s = vec![src(Desktop, &[1, 5], ""), src(Mic, &[1], ""), src(App, &[1, 2, 5], "a.exe")];
        let mut n = names(&["all", "a only", "", "", "desk + a", ""]);
        move_source(&mut s, &mut n, 2, 0);
        assert_eq!(layout(&s), of(&[("a", &[1, 2, 5]), ("desktop", &[1, 5]), ("mic", &[1])]));
        assert_eq!(n, names(&["all", "a only", "", "", "desk + a", ""]));
        move_source(&mut s, &mut n, 0, 9);
        assert_eq!(s[0].label(None), "a", "out of range: nothing moves");
    }

    #[test]
    fn tracks_that_start_or_end_lose_their_old_names() {
        let mut s = vec![src(Desktop, &[1, 2], ""), src(Mic, &[1, 3], "")];
        let mut n = names(&["all", "speakers", "voice", "stale"]);
        set_track(&mut s, &mut n, 1, 4, true);
        assert_eq!((s[1].tracks.clone(), n[3].as_str()), (vec![1, 3, 4], ""));
        set_track(&mut s, &mut n, 0, 3, true);
        assert_eq!(n[2], "voice", "joining a track keeps its name");
        set_track(&mut s, &mut n, 1, 3, false);
        set_track(&mut s, &mut n, 0, 3, false);
        assert_eq!((s[0].tracks.clone(), s[1].tracks.clone(), n[2].as_str()), (vec![1, 2], vec![1, 4], ""));
        let mut short = names(&[]);
        set_track(&mut s, &mut short, 0, 6, true);
        assert!(short.is_empty(), "clearing never grows the list");
    }
}
