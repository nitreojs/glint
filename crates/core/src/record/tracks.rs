use crate::config::audio::{AudioSource, MAX_TRACKS, SourceKind, app_name, track_name};

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub number: u32,
    pub name: String,
    pub auto: String,
    pub sources: Vec<usize>,
    pub covered: Vec<(usize, usize)>,
}

impl Track {
    pub fn note(&self, names: &[String]) -> Option<String> {
        let (&(_, desktop), _) = self.covered.split_first()?;
        let apps: Vec<&str> = self.covered.iter().map(|&(app, _)| names[app].as_str()).collect();
        let verb = if apps.len() == 1 { "is" } else { "are" };
        Some(format!("{} {verb} already in {}", apps.join(" and "), names[desktop]))
    }
}

// desktop loopback already holds every app it doesn't exclude, so adding the app again would play it twice.
fn holds(desktop: &AudioSource, app: &AudioSource) -> bool {
    desktop.kind == SourceKind::Desktop
        && app.kind == SourceKind::App
        && app_name(&desktop.exclude) != app_name(&app.app)
}

pub fn plan(names: &[String], sources: &[&AudioSource], custom: &[String]) -> Vec<Track> {
    let mut out: Vec<Track> = Vec::new();
    for n in 1..=MAX_TRACKS {
        let wanted: Vec<usize> = (0..sources.len()).filter(|&i| sources[i].tracks.contains(&n)).collect();
        let covered: Vec<(usize, usize)> = (wanted.iter())
            .filter_map(|&i| wanted.iter().find(|&&d| holds(sources[d], sources[i])).map(|&d| (i, d)))
            .collect();
        let kept: Vec<usize> = wanted.iter().copied().filter(|i| !covered.iter().any(|c| c.0 == *i)).collect();
        if kept.is_empty() || out.iter().any(|t| t.sources == kept) {
            continue;
        }
        let auto = match kept.as_slice() {
            [one] => names[*one].clone(),
            _ if wanted.len() == names.len() => "mix".into(),
            some => some.iter().map(|&i| names[i].as_str()).collect::<Vec<_>>().join(" + "),
        };
        let name = match track_name(custom, n) {
            "" => auto.clone(),
            own => own.to_string(),
        };
        out.push(Track { number: n, name, auto, sources: kept, covered });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(kind: SourceKind, tracks: &[u32], app: &str) -> AudioSource {
        let mut s = AudioSource::new(kind, 0);
        s.tracks = tracks.to_vec();
        match kind {
            SourceKind::App => s.app = app.into(),
            _ => s.exclude = app.into(),
        }
        s
    }

    fn run(sources: &[AudioSource], custom: &[&str]) -> Vec<Track> {
        let names: Vec<String> = sources.iter().map(|s| s.label(None)).collect();
        let refs: Vec<&AudioSource> = sources.iter().collect();
        let custom: Vec<String> = custom.iter().map(|s| s.to_string()).collect();
        plan(&names, &refs, &custom)
    }

    fn summary(tracks: &[Track]) -> Vec<(u32, &str, &[usize])> {
        tracks.iter().map(|t| (t.number, t.name.as_str(), t.sources.as_slice())).collect()
    }

    use SourceKind::{App, Desktop, Mic};

    #[test]
    fn default_layout_is_a_mix_then_one_track_per_source() {
        let s = [src(Desktop, &[1, 2], ""), src(App, &[1, 3], "discord.exe"), src(Mic, &[1, 4], "")];
        let t = run(&s, &[]);
        assert_eq!(
            summary(&t),
            [(1, "mix", &[0, 2][..]), (2, "desktop", &[0][..]), (3, "discord.exe", &[1][..]), (4, "mic", &[2][..])]
        );
        assert_eq!(t[0].covered, [(1, 0)]);
        let names: Vec<String> = s.iter().map(|s| s.label(None)).collect();
        assert_eq!(t[0].note(&names).unwrap(), "discord.exe is already in desktop");
        assert_eq!(t[1].note(&names), None);
    }

    #[test]
    fn apps_the_desktop_leaves_out_stay_and_mics_always_stay() {
        let s = [src(Desktop, &[1], "spotifast.exe"), src(App, &[1], "Spotifast.exe"), src(App, &[1], "game.exe"), src(Mic, &[1], "")];
        let t = run(&s, &[]);
        assert_eq!(summary(&t), [(1, "mix", &[0, 1, 3][..])]);
        let names: Vec<String> = s.iter().map(|s| s.label(None)).collect();
        assert_eq!(t[0].note(&names).unwrap(), "game.exe is already in desktop without spotifast.exe");
    }

    #[test]
    fn apps_without_a_desktop_on_the_same_track_are_kept() {
        let s = [src(Desktop, &[1], ""), src(App, &[2], "a.exe"), src(App, &[2], "b.exe")];
        let t = run(&s, &[]);
        assert_eq!(summary(&t), [(1, "desktop", &[0][..]), (2, "a.exe + b.exe", &[1, 2][..])]);
        assert!(t.iter().all(|t| t.covered.is_empty()));
    }

    #[test]
    fn a_track_that_turns_into_an_earlier_one_is_dropped() {
        let s = [src(Desktop, &[1, 2], ""), src(App, &[1], "a.exe")];
        assert_eq!(summary(&run(&s, &[])), [(1, "desktop", &[0][..])]);
    }

    #[test]
    fn empty_and_duplicate_tracks_are_skipped() {
        assert_eq!(summary(&run(&[src(Desktop, &[1, 2], "")], &[])), [(1, "desktop", &[0][..])]);
        let both = [src(Desktop, &[1, 5], ""), src(Mic, &[1, 5], "")];
        assert_eq!(summary(&run(&both, &[])), [(1, "mix", &[0, 1][..])]);
        let apart = [src(Desktop, &[3], ""), src(Mic, &[6], "")];
        assert_eq!(summary(&run(&apart, &[])), [(3, "desktop", &[0][..]), (6, "mic", &[1][..])]);
        assert!(run(&[src(Desktop, &[], ""), src(Mic, &[], "")], &[]).is_empty());
        assert!(run(&[], &[]).is_empty());
    }

    #[test]
    fn partial_mixes_name_their_sources() {
        let s = [src(Desktop, &[1, 2], ""), src(App, &[2], "game.exe"), src(Mic, &[1, 2, 3], "")];
        let t = run(&s, &[]);
        assert_eq!(summary(&t), [(1, "desktop + mic", &[0, 2][..]), (3, "mic", &[2][..])]);
    }

    #[test]
    fn custom_names_replace_the_auto_name_by_track_number() {
        let s = [src(Desktop, &[1, 3], ""), src(Mic, &[1, 4], "")];
        let t = run(&s, &["all", "unused", "  ", " voice "]);
        assert_eq!(summary(&t), [(1, "all", &[0, 1][..]), (3, "desktop", &[0][..]), (4, "voice", &[1][..])]);
        assert_eq!(t[0].auto, "mix");
        assert_eq!(t[2].auto, "mic");
    }
}
