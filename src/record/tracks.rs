use crate::config::audio::MAX_TRACKS;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Track {
    pub name: String,
    pub sources: Vec<usize>,
}

pub(crate) fn plan(names: &[String], routes: &[&[u32]]) -> Vec<Track> {
    let mut out: Vec<Track> = Vec::new();
    for n in 1..=MAX_TRACKS {
        let sources: Vec<usize> = (0..routes.len()).filter(|&i| routes[i].contains(&n)).collect();
        if sources.is_empty() || out.iter().any(|t| t.sources == sources) {
            continue;
        }
        let name = match sources.as_slice() {
            [one] => names[*one].clone(),
            all if all.len() == names.len() => "mix".into(),
            some => some.iter().map(|&i| names[i].as_str()).collect::<Vec<_>>().join(" + "),
        };
        out.push(Track { name, sources });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    fn summary(tracks: &[Track]) -> Vec<(&str, &[usize])> {
        tracks.iter().map(|t| (t.name.as_str(), t.sources.as_slice())).collect()
    }

    #[test]
    fn default_layout_is_a_mix_then_one_track_per_source() {
        let n = names(&["desktop", "discord.exe", "mic: Shure MV7"]);
        let t = plan(&n, &[&[1, 2], &[1, 3], &[1, 4]]);
        assert_eq!(
            summary(&t),
            [("mix", &[0, 1, 2][..]), ("desktop", &[0][..]), ("discord.exe", &[1][..]), ("mic: Shure MV7", &[2][..])]
        );
    }

    #[test]
    fn empty_and_duplicate_tracks_are_skipped() {
        let n = names(&["desktop", "mic"]);
        assert_eq!(summary(&plan(&n[..1], &[&[1, 2]])), [("desktop", &[0][..])]);
        assert_eq!(summary(&plan(&n, &[&[1, 5], &[1, 5]])), [("mix", &[0, 1][..])]);
        assert_eq!(summary(&plan(&n, &[&[3], &[6]])), [("desktop", &[0][..]), ("mic", &[1][..])]);
        assert!(plan(&n, &[&[], &[]]).is_empty());
        assert!(plan(&[], &[]).is_empty());
    }

    #[test]
    fn partial_mixes_name_their_sources() {
        let n = names(&["desktop", "game.exe", "mic"]);
        let t = plan(&n, &[&[1, 2], &[2], &[1, 2, 3]]);
        assert_eq!(summary(&t), [("desktop + mic", &[0, 2][..]), ("mix", &[0, 1, 2][..]), ("mic", &[2][..])]);
    }
}
