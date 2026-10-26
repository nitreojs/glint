//! What Steam's files say about an installed game: where it is (`libraryfolders.vdf`, then the
//! library's `appmanifest_<app>.acf`) and the launch options the player gave it
//! (`userdata\<account>\config\localconfig.vdf`).

use std::path::{Path, PathBuf};

/// Deadlock's Steam app id.
pub const DEADLOCK: u32 = 1_422_450;

/// A value in a Valve KeyValues text file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Text(String),
    Block(Vec<(String, Value)>),
}

impl Value {
    /// The value under `key` (keys are case-insensitive), in a block.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Block(entries) => entries.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            Value::Text(_) => None,
        }
    }

    pub fn at(&self, keys: &[&str]) -> Option<&Value> {
        keys.iter().try_fold(self, |v, k| v.get(k))
    }

    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            Value::Block(_) => None,
        }
    }
}

/// The top-level entries of a KeyValues text (`"key" "value"`, `"key" { … }`); whatever doesn't
/// parse ends the block it's in.
pub fn parse(text: &str) -> Value {
    let mut tokens = Tokens { rest: text };
    Value::Block(block(&mut tokens))
}

enum Token {
    Text(String),
    Open,
    Close,
}

struct Tokens<'a> {
    rest: &'a str,
}

impl Iterator for Tokens<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        loop {
            self.rest = self.rest.trim_start();
            if let Some(after) = self.rest.strip_prefix("//") {
                self.rest = after.split_once('\n').map_or("", |(_, r)| r);
                continue;
            }
            // platform conditions (`[$WIN32]`) after a value
            if self.rest.starts_with('[') {
                self.rest = self.rest.split_once(']').map_or("", |(_, r)| r);
                continue;
            }
            break;
        }
        let mut chars = self.rest.char_indices();
        let (_, first) = chars.next()?;
        match first {
            '{' => {
                self.rest = &self.rest[1..];
                Some(Token::Open)
            }
            '}' => {
                self.rest = &self.rest[1..];
                Some(Token::Close)
            }
            '"' => {
                let mut out = String::new();
                let mut escaped = false;
                for (i, c) in chars {
                    match c {
                        _ if escaped => {
                            out.push(match c {
                                'n' => '\n',
                                't' => '\t',
                                c => c,
                            });
                            escaped = false;
                        }
                        '\\' => escaped = true,
                        '"' => {
                            self.rest = &self.rest[i + 1..];
                            return Some(Token::Text(out));
                        }
                        c => out.push(c),
                    }
                }
                self.rest = "";
                Some(Token::Text(out))
            }
            _ => {
                let end = self.rest.find(|c: char| c.is_whitespace() || c == '{' || c == '}' || c == '"').unwrap_or(self.rest.len());
                let word = self.rest[..end].to_string();
                self.rest = &self.rest[end..];
                Some(Token::Text(word))
            }
        }
    }
}

fn block(tokens: &mut Tokens) -> Vec<(String, Value)> {
    let mut entries = Vec::new();
    while let Some(Token::Text(key)) = tokens.next() {
        match tokens.next() {
            Some(Token::Text(value)) => entries.push((key, Value::Text(value))),
            Some(Token::Open) => entries.push((key, Value::Block(block(tokens)))),
            _ => break,
        }
    }
    entries
}

fn read(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path).ok().map(|t| parse(&t))
}

/// Steam's libraries: the folders holding a `steamapps`, Steam's own first.
pub fn libraries(steam: &Path) -> Vec<PathBuf> {
    let mut out = vec![steam.to_path_buf()];
    if let Some(Value::Block(folders)) = read(&steam.join("steamapps").join("libraryfolders.vdf")).as_ref().and_then(|v| v.get("libraryfolders")) {
        for (_, folder) in folders {
            if let Some(path) = folder.get("path").and_then(Value::text).map(PathBuf::from)
                && !out.contains(&path)
            {
                out.push(path);
            }
        }
    }
    out
}

/// Where `app` is installed (`…\steamapps\common\Deadlock`), in any of Steam's libraries.
pub fn app_dir(steam: &Path, app: u32) -> Option<PathBuf> {
    libraries(steam).into_iter().find_map(|lib| {
        let manifest = read(&lib.join("steamapps").join(format!("appmanifest_{app}.acf")))?;
        let dir = manifest.at(&["AppState", "installdir"])?.text()?;
        Some(lib.join("steamapps").join("common").join(dir)).filter(|d| d.is_dir())
    })
}

/// The launch options of `app` for the account that used Steam last (its `localconfig.vdf` is
/// the newest); empty when none are set, `None` when that can't be told.
pub fn launch_options(steam: &Path, app: u32) -> Option<String> {
    let newest = std::fs::read_dir(steam.join("userdata"))
        .ok()?
        .flatten()
        .map(|account| account.path().join("config").join("localconfig.vdf"))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .max()?
        .1;
    let config = read(&newest)?;
    let app = config.at(&["UserLocalConfigStore", "Software", "Valve", "Steam", "apps", &app.to_string()])?;
    Some(app.get("LaunchOptions").and_then(Value::text).unwrap_or("").to_string())
}

/// Whether launch options hold `-condebug`, which makes Deadlock write its console log.
pub fn has_condebug(options: &str) -> bool {
    options.split_whitespace().any(|o| o.eq_ignore_ascii_case("-condebug"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyvalues_parse() {
        let text = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\Steam\"\n\t\t\"apps\" { \"1422450\" \"39217249687\" }\n\t}\n}\n// done\n";
        let v = parse(text);
        assert_eq!(v.at(&["libraryfolders", "0", "path"]).and_then(Value::text), Some(r"D:\Steam"));
        assert_eq!(v.at(&["LIBRARYFOLDERS", "0", "apps", "1422450"]).and_then(Value::text), Some("39217249687"), "keys are case-insensitive");
        assert_eq!(parse("\"a\" \"b\" [$WIN32] \"c\" { \"d\" \"e\" }").at(&["c", "d"]).and_then(Value::text), Some("e"));
        assert_eq!(parse("\"unclosed\" { \"a\" \"b\"").at(&["unclosed", "a"]).and_then(Value::text), Some("b"));
        assert_eq!(parse("\"esc\" \"say \\\"hi\\\"\"").get("esc").and_then(Value::text), Some("say \"hi\""));
    }

    #[test]
    fn a_game_and_its_launch_options_are_found() {
        let root = std::env::temp_dir().join(format!("glint_steam_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let steam = root.join("Steam");
        let lib = root.join("Library");
        let quoted = |p: &Path| p.display().to_string().replace('\\', "\\\\");
        let write = |path: PathBuf, text: String| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write(
            steam.join("steamapps").join("libraryfolders.vdf"),
            format!("\"libraryfolders\"\n{{\n\"0\" {{ \"path\" \"{}\" }}\n\"1\" {{ \"path\" \"{}\" }}\n}}\n", quoted(&steam), quoted(&lib)),
        );
        write(lib.join("steamapps").join("appmanifest_1422450.acf"), "\"AppState\"\n{\n\t\"appid\"\t\t\"1422450\"\n\t\"installdir\"\t\t\"Deadlock\"\n}\n".into());
        std::fs::create_dir_all(lib.join("steamapps").join("common").join("Deadlock")).unwrap();
        assert_eq!(libraries(&steam), [steam.clone(), lib.clone()]);
        assert_eq!(app_dir(&steam, DEADLOCK), Some(lib.join("steamapps").join("common").join("Deadlock")));
        assert_eq!(app_dir(&steam, 730), None);

        assert_eq!(launch_options(&steam, DEADLOCK), None, "no accounts");
        let config = |opts: &str| {
            format!("\"UserLocalConfigStore\"\n{{\n\"Software\" {{ \"Valve\" {{ \"Steam\" {{ \"apps\" {{ \"1422450\" {{ \"Playtime\" \"2714\" {opts} }} }} }} }} }}\n}}\n")
        };
        write(steam.join("userdata").join("1").join("config").join("localconfig.vdf"), config("\"LaunchOptions\" \"-dx11\""));
        assert_eq!(launch_options(&steam, DEADLOCK).as_deref(), Some("-dx11"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(steam.join("userdata").join("2").join("config").join("localconfig.vdf"), config("\"LaunchOptions\" \"-dx11 -condebug\""));
        let newest = launch_options(&steam, DEADLOCK).unwrap();
        assert_eq!(newest, "-dx11 -condebug", "the account that used steam last");
        assert!(has_condebug(&newest));
        assert!(!has_condebug("-dx11 -condebugger"));
        write(steam.join("userdata").join("2").join("config").join("localconfig.vdf"), config(""));
        assert_eq!(launch_options(&steam, DEADLOCK).as_deref(), Some(""), "none set");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
