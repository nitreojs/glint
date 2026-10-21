pub mod release;
pub mod verify;
pub mod version;

pub const RELEASES_PAGE: &str = "https://github.com/nitreojs/glint/releases";

pub fn notes_url(tag: &str) -> String {
    format!("{RELEASES_PAGE}/tag/{tag}")
}
