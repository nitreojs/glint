use serde::Deserialize;

use super::version::Version;

/// One entry of GitHub's `GET /repos/{owner}/{repo}/releases`.
#[derive(Deserialize, Debug)]
pub struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// A newer release with everything an update needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub version: Version,
    /// `v0.2.0`
    pub tag: String,
    pub exe: Asset,
    pub sig: Asset,
    pub sums: Asset,
}

pub const SUMS: &str = "SHA256SUMS";

/// The exe a release publishes, as named by the release workflow.
pub fn asset_name(tag: &str) -> String {
    format!("glint-{tag}-windows-x64.exe")
}

pub fn parse(json: &str) -> Result<Vec<Release>, String> {
    serde_json::from_str(json).map_err(|e| format!("github sent a release list glint couldn't read: {e}"))
}

/// The `rel="next"` page of a paginated GitHub list, from its `Link` header.
pub fn next_page(link: Option<&str>) -> Option<String> {
    link?.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        let next = params.split(';').any(|p| p.trim().replace(' ', "") == "rel=\"next\"");
        let url = url.trim().strip_prefix('<')?.strip_suffix('>')?;
        (next && url.starts_with("https://")).then(|| url.to_string())
    })
}

/// The newest release above `current` with a signed exe; pre-releases only on the beta channel.
pub fn pick(releases: &[Release], current: &Version, beta: bool) -> Option<Candidate> {
    releases
        .iter()
        .filter(|r| !r.draft && r.tag_name.starts_with('v'))
        .filter_map(|r| {
            let version = Version::parse(&r.tag_name)?;
            if version <= *current || (!beta && (r.prerelease || version.is_pre())) {
                return None;
            }
            let asset = |name: &str| r.assets.iter().find(|a| a.name == name).cloned();
            let exe_name = asset_name(&r.tag_name);
            Some(Candidate {
                version,
                tag: r.tag_name.clone(),
                sig: asset(&format!("{exe_name}.minisig"))?,
                sums: asset(SUMS)?,
                exe: asset(&exe_name)?,
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED: &str = include_str!("fixtures/releases.json");
    const WITH_AFTERGLOW: &str = include_str!("fixtures/releases-with-afterglow.json");

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn github_release_lists_parse() {
        let releases = parse(RECORDED).unwrap();
        let tags: Vec<_> = releases.iter().map(|r| (r.tag_name.as_str(), r.draft, r.prerelease, r.assets.len())).collect();
        assert_eq!(
            tags,
            [
                ("v0.4.0", true, false, 3),
                ("v0.3.0-beta.1", false, true, 3),
                ("v0.2.1", false, false, 2),
                ("v0.2.0", false, false, 3),
                ("v0.1.0", false, false, 3),
            ]
        );
        assert_eq!(releases[3].assets[0].size, 10503168);
        assert!(parse("{\"message\":\"API rate limit exceeded\"}").unwrap_err().starts_with("github sent"));
    }

    #[test]
    fn stable_takes_the_newest_signed_release() {
        let releases = parse(RECORDED).unwrap();
        let c = pick(&releases, &v("0.1.0"), false).unwrap();
        // v0.4.0 is a draft, v0.3.0-beta.1 a pre-release, v0.2.1 has no signature
        assert_eq!((c.tag.as_str(), c.version.to_string().as_str()), ("v0.2.0", "0.2.0"));
        assert_eq!(crate::update::notes_url(&c.tag), "https://github.com/nitreojs/glint/releases/tag/v0.2.0");
        assert_eq!(c.exe.name, "glint-v0.2.0-windows-x64.exe");
        assert_eq!(
            c.exe.browser_download_url,
            "https://github.com/nitreojs/glint/releases/download/v0.2.0/glint-v0.2.0-windows-x64.exe"
        );
        assert_eq!(c.sig.name, "glint-v0.2.0-windows-x64.exe.minisig");
        assert_eq!(c.sums.name, "SHA256SUMS");
        assert_eq!(pick(&releases, &v("0.2.0"), false), None);
        assert_eq!(pick(&releases, &v("0.3.0"), false), None);
    }

    #[test]
    fn beta_also_takes_pre_releases() {
        let releases = parse(RECORDED).unwrap();
        assert_eq!(pick(&releases, &v("0.1.0"), true).unwrap().tag, "v0.3.0-beta.1");
        assert_eq!(pick(&releases, &v("0.3.0-alpha.2"), true).unwrap().tag, "v0.3.0-beta.1");
        assert_eq!(pick(&releases, &v("0.3.0-beta.1"), true), None);
        // a beta user leaving the channel still gets the next stable release above them
        assert_eq!(pick(&releases, &v("0.2.0-beta.3"), false).unwrap().tag, "v0.2.0");
    }

    #[test]
    fn pages_follow_the_link_header() {
        let page2 = "https://api.github.com/repositories/1/releases?per_page=100&page=2";
        let first = format!(r#"<{page2}>; rel="next", <https://api.github.com/repositories/1/releases?per_page=100&page=4>; rel="last""#);
        assert_eq!(next_page(Some(&first)).as_deref(), Some(page2));
        let middle = format!(r#"<https://x/?page=1>; rel="prev", <{page2}>; rel = "next""#);
        assert_eq!(next_page(Some(&middle)).as_deref(), Some(page2));
        let last = r#"<https://x/?page=3>; rel="prev", <https://x/?page=1>; rel="first""#;
        assert_eq!(next_page(Some(last)), None);
        assert_eq!(next_page(None), None);
        assert_eq!(next_page(Some("garbage")), None);
        assert_eq!(next_page(Some(r#"<http://plain/?page=2>; rel="next""#)), None);
        // releases from several pages are picked from together
        let mut all = parse(WITH_AFTERGLOW).unwrap();
        all.extend(parse(RECORDED).unwrap());
        assert_eq!(pick(&all, &v("0.1.1"), false).unwrap().tag, "v0.2.0");
    }

    #[test]
    fn other_apps_releases_are_skipped() {
        let releases = parse(WITH_AFTERGLOW).unwrap();
        let tags: Vec<_> = releases.iter().map(|r| (r.tag_name.as_str(), r.prerelease, r.assets.len())).collect();
        assert_eq!(
            tags,
            [("afterglow-v0.2.0-beta.1", true, 3), ("afterglow-v0.1.0", false, 3), ("v0.1.2", false, 3), ("v0.1.1", false, 3)]
        );
        assert_eq!(releases[1].assets[0].name, "afterglow-v0.1.0-windows-x64.exe");
        for beta in [false, true] {
            let c = pick(&releases, &v("0.1.1"), beta).unwrap();
            assert_eq!((c.tag.as_str(), c.exe.name.as_str()), ("v0.1.2", "glint-v0.1.2-windows-x64.exe"));
            assert_eq!(pick(&releases, &v("0.1.2"), beta), None);
        }
    }
}
