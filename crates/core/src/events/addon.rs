//! Installing afterglow's Deadlock addon (`mods/deadlock`, which writes the lines
//! [`super::deadlock::Addon`] reads) into the addons folder the game loads, beside the player's
//! other mods.
//!
//! Deadlock loads `pakNN_dir.vpk` files (01-99, a lower number wins a file two of them have) from
//! the folder its `gameinfo.gi` names. Ours takes the highest free number, out of the way of the
//! mods the player orders. Deadlock Mod Manager (DMM) keeps that folder's mods in `.dmm.json`
//! there: ours goes in as the local mod [`DMM_ID`], so DMM lists it, keeps it through its
//! renumbering (it may move it to any number; ours is told by its contents), and the player can
//! turn it off there, which is left alone.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The addon's entry in DMM's `.dmm.json`.
pub const DMM_ID: &str = "local-afterglow";
/// The file name DMM keeps as the addon's original one.
pub const FILE: &str = "afterglow_events_dir.vpk";
/// A folder only the addon's VPKs have in their tree.
const MARK: &[u8] = b"panorama/scripts/afterglow\0";
const MANIFEST: &str = ".dmm.json";
const MANIFEST_VERSION: u64 = 3;

/// Where the addon stands in an addons folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Missing,
    /// Installed under this name, the same as the one at hand.
    Current(String),
    /// Installed under this name, another version.
    Outdated(String),
    /// Turned off in DMM.
    Off,
}

/// What [`sync`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    Nothing,
    Installed(String),
    Updated(String),
    Removed,
}

/// The addons folder `game` (the install, `…\common\Deadlock`) loads: the first `Game
/// citadel/addons…` search path of its `gameinfo.gi` (DMM's profiles have their own folder).
pub fn addons_dir(game: &Path) -> Result<PathBuf, String> {
    let gameinfo = game.join("game").join("citadel").join("gameinfo.gi");
    let text = std::fs::read_to_string(&gameinfo).map_err(|e| format!("{}: {e}", gameinfo.display()))?;
    text.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let key = words.next()?;
            let path = words.next()?.trim_matches('"');
            (key.eq_ignore_ascii_case("game") && path.to_ascii_lowercase().starts_with("citadel/addons")).then(|| game.join("game").join(path))
        })
        .next()
        .ok_or_else(|| "deadlock loads no addons (its gameinfo.gi names no addons folder)".into())
}

/// Whether the VPK at `path` is the addon (any version).
fn is_ours(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut header = [0u8; 12];
    if file.read_exact(&mut header).is_err() || header[..4] != [0x34, 0x12, 0xAA, 0x55] {
        return false;
    }
    let tree = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as u64;
    let skip = if header[4] == 1 { 0 } else { 16 };
    let mut bytes = Vec::new();
    if tree > 16 << 20 || std::io::copy(&mut file.by_ref().take(skip), &mut std::io::sink()).is_err() || file.take(tree).read_to_end(&mut bytes).is_err() {
        return false;
    }
    bytes.windows(MARK.len()).any(|w| w == MARK)
}

fn pak_number(name: &str) -> Option<u32> {
    let n = name.strip_prefix("pak")?.strip_suffix("_dir.vpk")?;
    (n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.parse().ok())?
}

/// The addon's paks the game loads (normally one), by name.
fn installed(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| pak_number(n).is_some() && is_ours(&dir.join(n)))
        .collect();
    out.sort();
    out
}

fn read_manifest(dir: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST)).ok()?).ok()
}

fn turned_off(manifest: Option<&Value>) -> bool {
    manifest.and_then(|m| m["mods"][DMM_ID]["enabled"].as_bool()) == Some(false)
}

pub fn state(dir: &Path, vpk: &[u8]) -> State {
    if turned_off(read_manifest(dir).as_ref()) {
        return State::Off;
    }
    match installed(dir).into_iter().next() {
        None => State::Missing,
        Some(name) if std::fs::read(dir.join(&name)).is_ok_and(|b| b == vpk) => State::Current(name),
        Some(name) => State::Outdated(name),
    }
}

/// Where DMM's things go besides the addons folder.
pub struct Dmm<'a> {
    /// A copy of `.dmm.json` is kept here before it's changed.
    pub backup: &'a Path,
    /// DMM's local mods store (`%LOCALAPPDATA%\dev.stormix.deadlock-mod-manager\mods`), where
    /// the addon's name for DMM's list goes; `None` without DMM.
    pub store: Option<&'a Path>,
}

/// Puts the addon (`vpk`) into `dir`, brings an older one up to date, or with `want` off takes
/// it out, with its DMM entry. Nothing else in the folder is touched. The game must not be
/// running: it holds its addons open.
pub fn sync(dir: &Path, want: bool, vpk: &[u8], dmm: &Dmm) -> Result<Done, String> {
    let at = |name: &str| dir.join(name);
    let err = |what: &str, e: std::io::Error| format!("{what}: {e}");
    let manifest = read_manifest(dir);
    let ours = installed(dir);
    if !want {
        let disabled = disabled_copies(dir);
        let had = !ours.is_empty() || !disabled.is_empty() || manifest.as_ref().is_some_and(|m| !m["mods"][DMM_ID].is_null());
        for name in ours.iter().chain(&disabled) {
            std::fs::remove_file(at(name)).map_err(|e| err(name, e))?;
        }
        if let Some(m) = manifest {
            unregister(dir, m, dmm.backup)?;
        }
        if let Some(store) = dmm.store {
            forget_metadata(store);
        }
        return Ok(if had { Done::Removed } else { Done::Nothing });
    }
    if turned_off(manifest.as_ref()) {
        return Ok(Done::Nothing);
    }
    let (name, done) = match ours.first() {
        Some(name) if std::fs::read(at(name)).is_ok_and(|b| b == vpk) => (name.clone(), Done::Nothing),
        Some(name) => {
            write(dir, name, vpk)?;
            (name.clone(), Done::Updated(name.clone()))
        }
        None => {
            let name = (1..=99).rev().map(|n| format!("pak{n:02}_dir.vpk")).find(|n| !at(n).exists()).ok_or("the addons folder has no free pak number")?;
            write(dir, &name, vpk)?;
            (name.clone(), Done::Installed(name))
        }
    };
    for extra in ours.iter().skip(1) {
        std::fs::remove_file(at(extra)).map_err(|e| err(extra, e))?;
    }
    if let Some(m) = manifest {
        register(dir, m, &name, dmm.backup)?;
    }
    if let Some(store) = dmm.store {
        write_metadata(store)?;
    }
    Ok(done)
}

/// The addon's paks DMM turned off (`local-afterglow_<name>.vpk`).
fn disabled_copies(dir: &Path) -> Vec<String> {
    let prefix = format!("{DMM_ID}_");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.starts_with(&prefix) && n.ends_with(".vpk")).collect()
}

/// Writes `bytes` to `dir\name` through a temporary file, so the folder never holds half of it.
fn write(dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let tmp = dir.join(format!("{name}.afterglow-tmp"));
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(name)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{name}: {e}")
    })
}

/// DMM's entry for the addon under `name`.
fn entry(name: &str) -> Value {
    json!({
        "enabled": true,
        "compatibilityEnabled": false,
        "compatibilityOptedOut": false,
        "order": null,
        "shard": 1,
        "currentVpks": [name],
        "disabledVpks": [],
        "originalVpkNames": [FILE],
    })
}

/// A manifest of a version DMM's this code knows, else `None` (it's left alone).
fn known(mut manifest: Value) -> Option<Value> {
    let version = manifest.get("version").map_or(Some(MANIFEST_VERSION), Value::as_u64);
    (version == Some(MANIFEST_VERSION) && manifest.get("mods").is_none_or(Value::is_object)).then(|| {
        if manifest.get("mods").is_none() {
            manifest["mods"] = json!({});
        }
        manifest
    })
}

fn register(dir: &Path, manifest: Value, name: &str, backup: &Path) -> Result<(), String> {
    let Some(mut m) = known(manifest) else { return Ok(()) };
    let want = entry(name);
    if m["mods"][DMM_ID] == want {
        return Ok(());
    }
    m["mods"][DMM_ID] = want;
    save_manifest(dir, &m, backup)
}

fn unregister(dir: &Path, manifest: Value, backup: &Path) -> Result<(), String> {
    let Some(mut m) = known(manifest) else { return Ok(()) };
    if m["mods"].as_object_mut().and_then(|mods| mods.remove(DMM_ID)).is_none() {
        return Ok(());
    }
    save_manifest(dir, &m, backup)
}

fn save_manifest(dir: &Path, manifest: &Value, backup: &Path) -> Result<(), String> {
    let path = dir.join(MANIFEST);
    if let Some(parent) = backup.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::copy(&path, backup).map_err(|e| format!("backing up {}: {e}", path.display()))?;
    let text = serde_json::to_string_pretty(manifest).map_err(|e| e.to_string())?;
    write(dir, MANIFEST, text.as_bytes())
}

fn write_metadata(store: &Path) -> Result<(), String> {
    let dir = store.join(DMM_ID);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let meta = json!({
        "id": DMM_ID,
        "kind": "local",
        "name": "afterglow events",
        "author": "afterglow",
        "description": "Writes your kills, deaths, assists and objectives to the console log for afterglow's clip markers. Managed by afterglow's settings.",
    });
    let path = dir.join("metadata.json");
    let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())?;
    if std::fs::read_to_string(&path).is_ok_and(|t| t == text) {
        return Ok(());
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn forget_metadata(store: &Path) {
    let dir = store.join(DMM_ID);
    let _ = std::fs::remove_file(dir.join("metadata.json"));
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vpk(tree_dir: &str, payload: &[u8]) -> Vec<u8> {
        let mut tree = Vec::new();
        tree.extend_from_slice(b"vjs_c\0");
        tree.extend_from_slice(tree_dir.as_bytes());
        tree.extend_from_slice(b"\0events\0");
        tree.extend_from_slice(&[0; 18]);
        tree.extend_from_slice(b"\0\0\0");
        let mut out = vec![0x34, 0x12, 0xAA, 0x55, 2, 0, 0, 0];
        out.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 16]);
        out.extend_from_slice(&tree);
        out.extend_from_slice(payload);
        out
    }

    struct Folder {
        root: PathBuf,
    }

    impl Folder {
        fn new(name: &str) -> Folder {
            let root = std::env::temp_dir().join(format!("glint_addon_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("game").join("citadel").join("addons")).unwrap();
            Folder { root }
        }
        fn addons(&self) -> PathBuf {
            self.root.join("game").join("citadel").join("addons")
        }
        fn files(&self) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.addons()).unwrap().flatten().map(|e| e.file_name().into_string().unwrap()).collect();
            v.sort();
            v
        }
        fn manifest(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.addons().join(MANIFEST)).unwrap()).unwrap()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const USERS: &str = r#"{
  "version": 3,
  "mods": {
    "727968": {
      "enabled": true,
      "compatibilityEnabled": false,
      "compatibilityOptedOut": false,
      "order": null,
      "shard": 1,
      "currentVpks": ["pak05_dir.vpk"],
      "disabledVpks": [],
      "originalVpkNames": ["pak41_dir.vpk"]
    },
    "731315": {
      "enabled": false,
      "compatibilityEnabled": false,
      "compatibilityOptedOut": false,
      "order": null,
      "shard": 1,
      "currentVpks": [],
      "disabledVpks": ["731315_pak10_dir.vpk"],
      "originalVpkNames": ["pak10_dir.vpk"]
    }
  }
}"#;

    #[test]
    fn the_addons_folder_comes_from_gameinfo() {
        let f = Folder::new("gameinfo");
        let gi = f.root.join("game").join("citadel").join("gameinfo.gi");
        std::fs::write(&gi, "\"GameInfo\"\n{\n\tFileSystem\n\t{\n\t\tSearchPaths\n\t\t{\n\t\t\tGame_LowViolence\tcitadel_lv\n\t\t\tGame\t\t\t\tcitadel/addons\n\t\t\tGame\t\t\t\tcitadel\n\t\t}\n\t}\n}\n").unwrap();
        assert_eq!(addons_dir(&f.root), Ok(f.root.join("game").join("citadel/addons")));
        std::fs::write(&gi, "SearchPaths\n{\n\tGame citadel/addons/ranked\n\tGame citadel/addons2/ranked\n\tGame citadel\n}\n").unwrap();
        assert_eq!(addons_dir(&f.root), Ok(f.root.join("game").join("citadel/addons/ranked")), "a DMM profile's folder");
        std::fs::write(&gi, "SearchPaths\n{\n\tGame citadel\n}\n").unwrap();
        assert!(addons_dir(&f.root).is_err());
    }

    #[test]
    fn it_goes_in_beside_the_players_mods_and_comes_out_clean() {
        let f = Folder::new("sync");
        let dir = f.addons();
        let theirs = vpk("panorama/layout", b"fov unlock");
        for name in ["pak01_dir.vpk", "pak05_dir.vpk", "731315_pak10_dir.vpk"] {
            std::fs::write(dir.join(name), &theirs).unwrap();
        }
        std::fs::write(dir.join("pak99_dir.vpk"), &theirs).unwrap();
        std::fs::write(dir.join(MANIFEST), USERS).unwrap();
        let backup = f.root.join("backup").join("dmm.json");
        let store = f.root.join("store");
        let dmm = Dmm { backup: &backup, store: Some(&store) };
        let v1 = vpk("panorama/scripts/afterglow", b"v1");
        let v2 = vpk("panorama/scripts/afterglow", b"v2");

        assert_eq!(state(&dir, &v1), State::Missing);
        assert_eq!(sync(&dir, true, &v1, &dmm), Ok(Done::Installed("pak98_dir.vpk".into())), "the highest free number");
        assert_eq!(std::fs::read(dir.join("pak98_dir.vpk")).unwrap(), v1);
        assert_eq!(state(&dir, &v1), State::Current("pak98_dir.vpk".into()));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), USERS, "the manifest as it was, kept");
        let m = f.manifest();
        assert_eq!(m["mods"][DMM_ID], entry("pak98_dir.vpk"));
        let before: Value = serde_json::from_str(USERS).unwrap();
        assert_eq!(m["mods"]["727968"], before["mods"]["727968"], "the player's mods stay as they were");
        assert_eq!(m["mods"]["731315"], before["mods"]["731315"]);
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(store.join(DMM_ID).join("metadata.json")).unwrap()).unwrap();
        assert_eq!((meta["id"].as_str(), meta["kind"].as_str()), (Some(DMM_ID), Some("local")));
        assert_eq!(sync(&dir, true, &v1, &dmm), Ok(Done::Nothing));

        std::fs::rename(dir.join("pak98_dir.vpk"), dir.join("pak06_dir.vpk")).unwrap();
        assert_eq!(state(&dir, &v2), State::Outdated("pak06_dir.vpk".into()), "found by its contents after DMM renumbered it");
        assert_eq!(sync(&dir, true, &v2, &dmm), Ok(Done::Updated("pak06_dir.vpk".into())));
        assert_eq!(std::fs::read(dir.join("pak06_dir.vpk")).unwrap(), v2);
        assert_eq!(f.manifest()["mods"][DMM_ID]["currentVpks"], json!(["pak06_dir.vpk"]));
        assert!(!f.files().iter().any(|n| n.ends_with("afterglow-tmp")));

        std::fs::rename(dir.join("pak06_dir.vpk"), dir.join(format!("{DMM_ID}_{FILE}"))).unwrap();
        let mut m = f.manifest();
        m["mods"][DMM_ID]["enabled"] = json!(false);
        std::fs::write(dir.join(MANIFEST), serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(state(&dir, &v2), State::Off, "turned off in DMM, and left so");
        assert_eq!(sync(&dir, true, &v2, &dmm), Ok(Done::Nothing));
        assert!(!dir.join("pak98_dir.vpk").exists() && !dir.join("pak06_dir.vpk").exists());

        assert_eq!(sync(&dir, false, &v2, &dmm), Ok(Done::Removed));
        assert_eq!(f.files(), [MANIFEST, "731315_pak10_dir.vpk", "pak01_dir.vpk", "pak05_dir.vpk", "pak99_dir.vpk"]);
        assert_eq!(f.manifest(), before, "the manifest is the player's again");
        assert!(!store.join(DMM_ID).exists());
        assert_eq!(sync(&dir, false, &v2, &dmm), Ok(Done::Nothing));
    }

    #[test]
    fn without_dmm_only_the_pak_goes_in() {
        let f = Folder::new("plain");
        let dir = f.addons();
        let backup = f.root.join("backup.json");
        let dmm = Dmm { backup: &backup, store: None };
        let v1 = vpk("panorama/scripts/afterglow", b"v1");
        assert_eq!(sync(&dir, true, &v1, &dmm), Ok(Done::Installed("pak99_dir.vpk".into())));
        assert_eq!(f.files(), ["pak99_dir.vpk"]);
        assert_eq!(sync(&dir, false, &v1, &dmm), Ok(Done::Removed));
        assert!(f.files().is_empty());

        std::fs::write(dir.join(MANIFEST), r#"{"version":4,"mods":{}}"#).unwrap();
        assert_eq!(sync(&dir, true, &v1, &dmm), Ok(Done::Installed("pak99_dir.vpk".into())));
        assert_eq!(std::fs::read_to_string(dir.join(MANIFEST)).unwrap(), r#"{"version":4,"mods":{}}"#, "a newer DMM's manifest is left alone");
        assert!(!backup.exists());
    }
}
