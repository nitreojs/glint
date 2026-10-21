use std::cmp::Ordering;
use std::fmt;

/// A semantic version (`1.2.3`, `1.2.3-beta.1`); build metadata is accepted and ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    core: [u64; 3],
    pre: Vec<Ident>,
}

/// Numeric identifiers sort below alphanumeric ones, as semver requires.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Text(String),
}

fn number(s: &str) -> Option<u64> {
    let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if digits && (s == "0" || !s.starts_with('0')) { s.parse().ok() } else { None }
}

impl Version {
    /// Parses `1.2.3`, `v1.2.3`, `1.2.3-rc.1+build`.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(v, _)| v);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut parts = core.split('.').map(number);
        let core = [parts.next()??, parts.next()??, parts.next()??];
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| match number(id) {
                    Some(n) => Some(Ident::Num(n)),
                    None if id.bytes().all(|b| b.is_ascii_digit()) => None,
                    None if id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') => Some(Ident::Text(id.into())),
                    None => None,
                })
                .collect::<Option<_>>()?,
        };
        Some(Version { core, pre })
    }

    /// The version this build was made from.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml has a semver version")
    }

    pub(crate) fn is_pre(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> Ordering {
        self.core.cmp(&other.core).then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => self.pre.cmp(&other.pre),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let [major, minor, patch] = self.core;
        write!(f, "{major}.{minor}.{patch}")?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Text(t) => f.write_str(t)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s}"))
    }

    #[test]
    fn versions_parse_and_print_back() {
        for s in ["0.1.0", "1.20.300", "0.2.0-beta.1", "1.0.0-alpha-2.x.7", "1.0.0-0"] {
            assert_eq!(v(s).to_string(), s);
        }
        assert_eq!(v("v0.2.0").to_string(), "0.2.0");
        assert_eq!(v("1.2.3+win.5"), v("1.2.3"));
        assert!(v("0.2.0-rc.1").is_pre() && !v("0.2.0").is_pre());
        for bad in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.x", "1.2.3-", "1.2.3-a..b", "1.2.3-01", "1.2.3-é", "vv1.2.3", " 1.2.3"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn precedence_follows_semver() {
        // the chain from semver.org, section 11
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "1.0.1",
            "1.1.0",
            "2.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        assert!(v("0.10.0") > v("0.9.9"));
        assert!(v("0.2.0-beta.1") > v("0.1.0"));
        assert!(v("0.2.0-beta.1") < v("0.2.0"));
        assert_eq!(v("v1.0.0").cmp(&v("1.0.0+other")), Ordering::Equal);
    }
}
