use std::path::PathBuf;

use crate::history::Kind;

/// The app window a capture came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Source {
    /// `Telegram.exe`
    pub exe: String,
    /// The exe's description (`Telegram Desktop`), else its name without `.exe`.
    pub app: String,
    pub title: String,
}

/// What a capture shows, for `{app}` and `{title}`.
#[derive(Clone, Copy, Debug)]
pub enum Subject<'a> {
    Window(&'a Source),
    /// Whole monitors.
    Screen,
    /// A dragged region, or windows of several apps.
    Region,
}

/// Everything a file name pattern can refer to.
#[derive(Clone, Copy, Debug)]
pub struct Naming<'a> {
    /// `2026-10-20`
    pub date: &'a str,
    /// `14-32-07`
    pub time: &'a str,
    pub kind: Kind,
    pub subject: Subject<'a>,
}

pub const DEFAULT_PATTERN: &str = "glint_{date}_{time}";
/// The file part of the pattern "name files after the window" sets.
pub const WINDOW_NAME: &str = "{app}_{date}_{time}";
/// The folder "sort into folders by app" puts in front.
pub const APP_FOLDER: &str = "{app}/";
const MAX_TOKEN: usize = 60;
/// Characters Windows doesn't allow in file names, besides `/` which makes folders.
const BAD_CHARS: &[char] = &['\\', ':', '*', '?', '"', '<', '>', '|'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2",
    "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Why `pattern` can't name files, if it can't.
pub fn pattern_problem(pattern: &str) -> Option<&'static str> {
    if pattern.contains(BAD_CHARS) || pattern.contains(char::is_control) {
        return Some("file names can't contain \\ : * ? \" < > |");
    }
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return None;
    }
    let segments: Vec<&str> = pattern.split('/').map(str::trim).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Some("a / needs a folder name before it and a file name after it");
    }
    if segments.iter().any(|s| s.trim_end_matches('.').is_empty()) {
        return Some("folders can't be called . or ..");
    }
    None
}

/// Text from outside (an app name, a window title) made safe for one file or folder name:
/// no characters Windows rejects, no control characters, at most `max` characters, trimmed.
pub fn sanitize(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() || c == '/' || BAD_CHARS.contains(&c) { ' ' } else { c })
        .collect();
    let words: Vec<&str> = clean.split_whitespace().collect();
    let joined: String = words.join(" ").chars().take(max).collect();
    joined.trim_end_matches(['.', ' ']).trim_start().to_string()
}

/// The capture's path below the output folder, without the extension: folders from the
/// pattern's `/`, then the file name.
pub fn expand(pattern: &str, n: &Naming) -> PathBuf {
    let pattern = match pattern.trim() {
        "" => DEFAULT_PATTERN,
        p => p,
    };
    let segments: Vec<&str> = pattern.split('/').map(str::trim).filter(|s| !s.is_empty()).collect();
    let last = segments.len().saturating_sub(1);
    let mut path = PathBuf::new();
    for (i, segment) in segments.iter().enumerate() {
        let folder = i < last;
        path.push(finish(&fill(segment, n, folder), if folder { "other" } else { "glint" }));
    }
    path
}

/// The pattern's tokens filled in; unknown `{...}` stays as typed.
fn fill(segment: &str, n: &Naming, folder: bool) -> String {
    let fallback = match n.subject {
        Subject::Screen => "screen",
        _ if folder => "other",
        _ => "screen",
    };
    let app = match n.subject {
        Subject::Window(s) => Some(sanitize(&s.app, MAX_TOKEN)).filter(|a| !a.is_empty()),
        _ => None,
    };
    let title = match n.subject {
        Subject::Window(s) => Some(sanitize(&s.title, MAX_TOKEN)).filter(|t| !t.is_empty()),
        _ => None,
    };
    let mut out = String::with_capacity(segment.len() + 32);
    let mut rest = segment;
    while let Some(open) = rest.find('{') {
        out += &rest[..open];
        let after = &rest[open..];
        let Some(close) = after.find('}') else {
            rest = after;
            break;
        };
        match &after[1..close] {
            "date" => out += n.date,
            "time" => out += n.time,
            "kind" => out += kind_folder(n.kind),
            "app" => out += app.as_deref().unwrap_or(fallback),
            "title" => out += title.as_deref().or(app.as_deref()).unwrap_or(fallback),
            _ => out += &after[..=close],
        }
        rest = &after[close + 1..];
    }
    out + rest
}

/// What `{kind}` becomes.
fn kind_folder(kind: Kind) -> &'static str {
    match kind {
        Kind::Shot => "screenshots",
        Kind::Recording => "recordings",
    }
}

/// A finished name Windows accepts: no trailing dots or spaces, no device names.
fn finish(name: &str, empty: &str) -> String {
    let name = name.trim().trim_end_matches(['.', ' ']);
    if name.is_empty() {
        return empty.into();
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return format!("{name}_");
    }
    name.into()
}

fn split_last(pattern: &str) -> (&str, &str) {
    match pattern.rfind('/') {
        Some(i) => (&pattern[..=i], &pattern[i + 1..]),
        None => ("", pattern),
    }
}

fn or_default(pattern: &str) -> &str {
    match pattern.trim() {
        "" => DEFAULT_PATTERN,
        p => p,
    }
}

/// Whether file names are the window's app with the date and time.
pub fn names_after_window(pattern: &str) -> bool {
    split_last(or_default(pattern)).1.trim() == WINDOW_NAME
}

/// `pattern` with its file part named after the window (`on`) or back to the default.
pub fn with_window_names(pattern: &str, on: bool) -> String {
    let (folders, _) = split_last(or_default(pattern));
    format!("{folders}{}", if on { WINDOW_NAME } else { DEFAULT_PATTERN })
}

/// Whether captures go to a folder per app.
pub fn sorted_by_app(pattern: &str) -> bool {
    or_default(pattern).starts_with(APP_FOLDER)
}

/// `pattern` with or without the per-app folder in front.
pub fn with_app_folders(pattern: &str, on: bool) -> String {
    let pattern = or_default(pattern);
    match (on, pattern.strip_prefix(APP_FOLDER)) {
        (true, None) => format!("{APP_FOLDER}{pattern}"),
        (false, Some(rest)) => rest.to_string(),
        _ => pattern.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn telegram() -> Source {
        Source { exe: "Telegram.exe".into(), app: "Telegram Desktop".into(), title: "Saved Messages".into() }
    }

    fn named(pattern: &str, kind: Kind, subject: Subject) -> PathBuf {
        expand(pattern, &Naming { date: "2026-10-20", time: "14-32-07", kind, subject })
    }

    fn path(parts: &[&str]) -> PathBuf {
        parts.iter().collect()
    }

    #[test]
    fn tokens_fill_in() {
        let tg = telegram();
        let win = Subject::Window(&tg);
        assert_eq!(named(DEFAULT_PATTERN, Kind::Shot, win), path(&["glint_2026-10-20_14-32-07"]));
        assert_eq!(named("  ", Kind::Shot, win), path(&["glint_2026-10-20_14-32-07"]));
        assert_eq!(named(WINDOW_NAME, Kind::Recording, win), path(&["Telegram Desktop_2026-10-20_14-32-07"]));
        assert_eq!(named("{title} {time} {time}", Kind::Shot, win), path(&["Saved Messages 14-32-07 14-32-07"]));
        assert_eq!(named("{kind}/{app}", Kind::Shot, win), path(&["screenshots", "Telegram Desktop"]));
        assert_eq!(named("{kind}/x", Kind::Recording, win), path(&["recordings", "x"]));
        assert_eq!(named("{nope} {date", Kind::Shot, win), path(&["{nope} {date"]));
        // a title that looks like a token isn't filled in again
        let odd = Source { title: "{date}".into(), ..telegram() };
        assert_eq!(named("{title}", Kind::Shot, Subject::Window(&odd)), path(&["{date}"]));
        assert_eq!(named("clip {date}. ", Kind::Shot, win), path(&["clip 2026-10-20"]));
    }

    #[test]
    fn screens_and_regions_fall_back() {
        assert_eq!(named(WINDOW_NAME, Kind::Shot, Subject::Screen), path(&["screen_2026-10-20_14-32-07"]));
        assert_eq!(named(WINDOW_NAME, Kind::Shot, Subject::Region), path(&["screen_2026-10-20_14-32-07"]));
        assert_eq!(named("{app}/{title}", Kind::Shot, Subject::Region), path(&["other", "screen"]));
        assert_eq!(named("{app}/{app}", Kind::Shot, Subject::Screen), path(&["screen", "screen"]));
        let untitled = Source { title: " \t".into(), ..telegram() };
        assert_eq!(named("{title}", Kind::Shot, Subject::Window(&untitled)), path(&["Telegram Desktop"]));
        let nameless = Source { app: "???".into(), title: String::new(), exe: "x.exe".into() };
        assert_eq!(named("{app}/{title}", Kind::Shot, Subject::Window(&nameless)), path(&["other", "screen"]));
    }

    #[test]
    fn outside_text_is_sanitized() {
        assert_eq!(sanitize("a/b\\c:d*e?f\"g<h>i|j", 60), "a b c d e f g h i j");
        assert_eq!(sanitize("  tab\there\nnew\u{7}line  ", 60), "tab here new line");
        assert_eq!(sanitize("readme.txt...", 60), "readme.txt");
        assert_eq!(sanitize(&"ж".repeat(100), 60).chars().count(), 60);
        assert_eq!(sanitize(&format!("{}  end", "x".repeat(59)), 60), "x".repeat(59));
        let long = Source { title: "t".repeat(200), ..telegram() };
        let name = named("{title}", Kind::Shot, Subject::Window(&long));
        assert_eq!(name.to_string_lossy().chars().count(), MAX_TOKEN);
        let slashes = Source { app: "../../etc".into(), title: "C:\\Windows".into(), ..telegram() };
        assert_eq!(named("{app}/{title}", Kind::Shot, Subject::Window(&slashes)), path(&[".. .. etc", "C Windows"]));
        let device = Source { app: "con".into(), ..telegram() };
        assert_eq!(named("{app}/x", Kind::Shot, Subject::Window(&device)), path(&["con_", "x"]));
        let dots = Source { app: "..".into(), ..telegram() };
        assert_eq!(named("{app}/x", Kind::Shot, Subject::Window(&dots)), path(&["other", "x"]));
    }

    #[test]
    fn patterns_are_checked() {
        for ok in ["", "glint {date}", "{app}/{date}", "{kind}/{app}/{title} {time}", "a.b/c"] {
            assert_eq!(pattern_problem(ok), None, "{ok:?}");
        }
        for bad in ["a\\b", "a:b", "a|b", "x\u{1}"] {
            assert!(pattern_problem(bad).unwrap().contains("can't contain"), "{bad:?}");
        }
        for bad in ["/x", "x/", "a//b", "a/ /b"] {
            assert!(pattern_problem(bad).unwrap().contains("needs a folder"), "{bad:?}");
        }
        for bad in ["../x", "a/./b", "a/.../b"] {
            assert!(pattern_problem(bad).unwrap().contains(". or .."), "{bad:?}");
        }
    }

    #[test]
    fn toggles_edit_the_pattern() {
        assert!(!names_after_window("") && !sorted_by_app(""));
        let named = with_window_names("", true);
        assert_eq!(named, WINDOW_NAME);
        assert!(names_after_window(&named));
        let sorted = with_app_folders(&named, true);
        assert_eq!(sorted, "{app}/{app}_{date}_{time}");
        assert!(sorted_by_app(&sorted) && names_after_window(&sorted));
        assert_eq!(with_app_folders(&sorted, true), sorted);
        assert_eq!(with_window_names(&sorted, false), "{app}/glint_{date}_{time}");
        assert_eq!(with_app_folders(&sorted, false), WINDOW_NAME);
        assert_eq!(with_window_names("{kind}/mine {time}", true), "{kind}/{app}_{date}_{time}");
        assert_eq!(with_app_folders("mine {time}", true), "{app}/mine {time}");
        assert_eq!(with_app_folders("mine {time}", false), "mine {time}");
        let folders = expand(&sorted, &Naming { date: "d", time: "t", kind: Kind::Shot, subject: Subject::Region });
        assert_eq!(folders, Path::new("other").join("screen_d_t"));
    }
}
