//! Updates from the repo's GitHub releases, for each app released there: which releases belong
//! to an app, which one to take, what its files are called and how they're checked.

pub mod release;
pub mod status;
pub mod verify;
pub mod version;

use version::Version;

pub const RELEASES_PAGE: &str = "https://github.com/nitreojs/glint/releases";

pub fn notes_url(tag: &str) -> String {
    format!("{RELEASES_PAGE}/tag/{tag}")
}

/// An app released from the repo; its releases share the list with the other apps'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Product {
    /// The exe, its `%APPDATA%` folder, its release assets and signatures are named after it.
    pub name: &'static str,
    /// What its release tags start with, before the version.
    pub tag_prefix: &'static str,
}

/// glint: tags `v0.2.0`, assets `glint-v0.2.0-windows-x64.exe`.
pub const GLINT: Product = Product { name: "glint", tag_prefix: "v" };
/// afterglow: tags `afterglow-v0.1.0`, assets `afterglow-v0.1.0-windows-x64.exe`.
pub const AFTERGLOW: Product = Product { name: "afterglow", tag_prefix: "afterglow-v" };

const ASSET_SUFFIX: &str = "-windows-x64.exe";

impl Product {
    /// The version `tag` names if it's one of this app's tags.
    pub fn version(&self, tag: &str) -> Option<Version> {
        let rest = tag.strip_prefix(self.tag_prefix)?;
        if !rest.starts_with(|c: char| c.is_ascii_digit()) {
            return None;
        }
        Version::parse(rest)
    }

    /// The tag a release of `version` has.
    pub fn tag(&self, version: &Version) -> String {
        format!("{}{version}", self.tag_prefix)
    }

    /// `tag` without the prefix: `0.2.0-beta.1`.
    fn bare<'a>(&self, tag: &'a str) -> &'a str {
        tag.strip_prefix(self.tag_prefix).unwrap_or(tag)
    }

    /// The exe release `tag` publishes, as named by the release workflow.
    pub fn asset_name(&self, tag: &str) -> String {
        format!("{}-v{}{ASSET_SUFFIX}", self.name, self.bare(tag))
    }

    /// The tag of a downloaded release exe, from its [`Product::asset_name`].
    pub fn asset_tag(&self, file: &str) -> Option<String> {
        let rest = file.strip_prefix(self.name)?.strip_prefix("-v")?.strip_suffix(ASSET_SUFFIX)?;
        Some(format!("{}{rest}", self.tag_prefix))
    }

    /// What the release workflow signs into the trusted comment, binding the file to its release:
    /// `glint v0.2.0 glint-v0.2.0-windows-x64.exe`.
    pub fn trusted_comment(&self, tag: &str) -> String {
        format!("{} v{} {}", self.name, self.bare(tag), self.asset_name(tag))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_app_names_its_tags_and_files() {
        let v = |s| Version::parse(s).unwrap();
        assert_eq!(GLINT.version("v0.2.0"), Some(v("0.2.0")));
        assert_eq!(GLINT.version("afterglow-v0.2.0"), None);
        assert_eq!(GLINT.version("vv0.2.0"), None);
        assert_eq!(AFTERGLOW.version("afterglow-v0.2.0-beta.1"), Some(v("0.2.0-beta.1")));
        assert_eq!(AFTERGLOW.version("v0.2.0"), None);
        assert_eq!(AFTERGLOW.tag(&v("0.2.0")), "afterglow-v0.2.0");
        assert_eq!(GLINT.tag(&v("0.3.0-beta.2")), "v0.3.0-beta.2");

        assert_eq!(GLINT.asset_name("v0.2.0"), "glint-v0.2.0-windows-x64.exe");
        assert_eq!(AFTERGLOW.asset_name("afterglow-v0.2.0"), "afterglow-v0.2.0-windows-x64.exe");
        assert_eq!(GLINT.asset_tag("glint-v0.2.0-windows-x64.exe").as_deref(), Some("v0.2.0"));
        assert_eq!(AFTERGLOW.asset_tag("afterglow-v0.2.0-windows-x64.exe").as_deref(), Some("afterglow-v0.2.0"));
        assert_eq!(GLINT.asset_tag("afterglow-v0.2.0-windows-x64.exe"), None);
        assert_eq!(AFTERGLOW.asset_tag("glint-v0.2.0-windows-x64.exe"), None);
        assert_eq!(GLINT.asset_tag("glint-v0.2.0-windows-x64.exe.minisig"), None);

        assert_eq!(GLINT.trusted_comment("v0.2.0"), "glint v0.2.0 glint-v0.2.0-windows-x64.exe");
        assert_eq!(
            AFTERGLOW.trusted_comment("afterglow-v0.2.0"),
            "afterglow v0.2.0 afterglow-v0.2.0-windows-x64.exe"
        );
    }
}
