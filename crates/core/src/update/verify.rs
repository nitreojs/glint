use minisign_verify::{PublicKey, Signature};

use super::release::asset_name;
use super::version::Version;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The digest `sha256sum` listed for `name`: lines of `<hex>  <name>` (or `<hex> *<name>`).
pub fn sums_entry(sums: &str, name: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.trim_end().split_once(' ')?;
        let file = file.strip_prefix([' ', '*'])?;
        if file != name || digest.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(digest.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        Some(out)
    })
}

/// What the release workflow signs into the trusted comment, binding the file to its release.
pub fn trusted_comment(tag: &str) -> String {
    format!("glint {tag} {}", asset_name(tag))
}

/// Accepts `exe` as release `tag` only if it is newer than `current`, matches SHA256SUMS, and
/// carries a signature from `key` made for exactly this release and file.
pub fn verify(
    exe: &[u8],
    sums: &str,
    sig: &str,
    key: &str,
    tag: &str,
    current: &Version,
    sha256: impl Fn(&[u8]) -> Result<[u8; 32], String>,
) -> Result<(), String> {
    let version = Version::parse(tag).ok_or_else(|| format!("{tag} isn't a version"))?;
    if version <= *current {
        return Err(format!("refusing {tag}: it isn't newer than the running {current}"));
    }
    let name = asset_name(tag);
    let expected = sums_entry(sums, &name).ok_or_else(|| format!("SHA256SUMS has no entry for {name}"))?;
    let actual = sha256(exe)?;
    if actual != expected {
        return Err(format!("{name} is damaged: sha-256 {} instead of {}", hex(&actual), hex(&expected)));
    }
    let key = PublicKey::from_base64(key).map_err(|e| format!("bad update key: {e}"))?;
    let sig = Signature::decode(sig).map_err(|e| format!("{name}.minisig can't be read: {e}"))?;
    key.verify(exe, &sig, false).map_err(|e| format!("{name} isn't signed by glint's release key: {e}"))?;
    let comment = sig.trusted_comment();
    if comment != trusted_comment(tag) {
        return Err(format!("{name} was signed as \"{comment}\", not as {tag}"));
    }
    Ok(())
}
