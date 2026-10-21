use serde::Deserialize;

pub(crate) const MAX_TRACKS: u32 = 6;
pub(crate) const MAX_VOLUME: u32 = 200;

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum SourceKind {
    Desktop,
    App,
    Mic,
}

impl SourceKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            SourceKind::Desktop => "desktop",
            SourceKind::App => "app",
            SourceKind::Mic => "mic",
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AudioSource {
    pub kind: SourceKind,
    pub enabled: bool,
    pub volume: u32,
    pub tracks: Vec<u32>,
    pub app: String,
    pub exclude: String,
    pub device: String,
}

impl Default for AudioSource {
    fn default() -> Self {
        AudioSource { tracks: vec![1], ..AudioSource::new(SourceKind::Desktop, 0) }
    }
}

impl AudioSource {
    pub(crate) fn new(kind: SourceKind, nth: usize) -> AudioSource {
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
        }
    }

    pub(crate) fn label(&self, device_name: Option<&str>) -> String {
        match self.kind {
            SourceKind::Desktop => match app_name(&self.exclude) {
                exe if exe.is_empty() => "desktop".into(),
                exe => format!("desktop without {exe}"),
            },
            SourceKind::App => app_name(&self.app),
            SourceKind::Mic => match device_name {
                Some(name) => format!("mic: {name}"),
                None => "mic".into(),
            },
        }
    }

    pub(crate) fn records(&self) -> bool {
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
        line + " }"
    }
}

pub(crate) fn app_name(exe: &str) -> String {
    exe.trim().to_lowercase()
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

pub(crate) fn track_name(names: &[String], n: u32) -> &str {
    names.get(n as usize - 1).map_or("", |s| s.trim())
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
            AudioSource { app: "discord.exe".into(), volume: 150, ..AudioSource::new(SourceKind::App, 1) },
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
        assert_eq!(AudioSource { kind: SourceKind::App, app: " Discord.EXE".into(), ..ok }.label(None), "discord.exe");
        let desktop = AudioSource { exclude: "Game.exe".into(), ..AudioSource::new(SourceKind::Desktop, 0) };
        assert_eq!(desktop.label(None), "desktop without game.exe");
    }
}
