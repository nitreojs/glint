use glint_core::update::verify as checks;
use glint_core::update::version::Version;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};

pub(crate) fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    status.ok().map_err(|e| format!("can't compute sha-256: {}", e.message()))?;
    Ok(out)
}

pub(crate) fn verify(exe: &[u8], sums: &str, sig: &str, key: &str, tag: &str, current: &Version) -> Result<(), String> {
    checks::verify(exe, sums, sig, key, tag, current, sha256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glint_core::update::verify::{hex, sums_entry, trusted_comment};

    /// A throwaway minisign key that signed the fixtures; never used for releases.
    const TEST_KEY: &str = "RWQr5QlLKaagCabzmy83MJZXq1/hidI5B+tyy/ykArTUeaZMkmNETif5";
    const EXE: &[u8] = include_bytes!("fixtures/fixture.bin");
    const SIG: &str = include_str!("fixtures/fixture.bin.minisig");
    const SUMS: &str = include_str!("fixtures/SHA256SUMS");

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(hex(&sha256(b"").unwrap()), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc").unwrap()), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn sha256sums_lines_parse() {
        let sums = "5bfa03225597b9e3945bc5dc6ad0d1037147de02e9204325ffc966b2c26dc45c  glint-v0.2.0-windows-x64.exe\r\n\
                    e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *empty.bin\n";
        assert_eq!(hex(&sums_entry(sums, "glint-v0.2.0-windows-x64.exe").unwrap()), &sums[..64]);
        assert_eq!(hex(&sums_entry(sums, "empty.bin").unwrap()), hex(&sha256(b"").unwrap()));
        assert_eq!(sums_entry(sums, "glint-v0.2.0-windows-x64"), None);
        assert_eq!(sums_entry("abcd  x.exe", "x.exe"), None);
        assert_eq!(sums_entry(&format!("{}  x.exe", "z".repeat(64)), "x.exe"), None);
        assert_eq!(sums_entry(&format!("{} x.exe", "a".repeat(64)), "x.exe"), None);
        assert_eq!(sums_entry(SUMS, "glint-v0.2.0-windows-x64.exe"), Some(sha256(EXE).unwrap()));
    }

    #[test]
    fn signed_fixture_verifies() {
        assert_eq!(trusted_comment("v0.2.0"), "glint v0.2.0 glint-v0.2.0-windows-x64.exe");
        assert_eq!(verify(EXE, SUMS, SIG, TEST_KEY, "v0.2.0", &v("0.1.0")), Ok(()));
        assert_eq!(verify(EXE, SUMS, SIG, TEST_KEY, "v0.2.0", &v("0.2.0-beta.4")), Ok(()));
    }

    #[test]
    fn tampering_is_refused() {
        let current = v("0.1.0");
        let mut damaged = EXE.to_vec();
        damaged[40] ^= 1;
        assert!(verify(&damaged, SUMS, SIG, TEST_KEY, "v0.2.0", &current).unwrap_err().contains("is damaged"));
        // a matching checksum doesn't help without the signature
        let resummed = format!("{}  glint-v0.2.0-windows-x64.exe\n", hex(&sha256(&damaged).unwrap()));
        let err = verify(&damaged, &resummed, SIG, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert!(err.contains("isn't signed by glint's release key"), "{err}");
        let other = include_str!("fixtures/other-key.minisig");
        assert!(verify(EXE, SUMS, other, TEST_KEY, "v0.2.0", &current).unwrap_err().contains("isn't signed"));
        assert!(verify(EXE, SUMS, "nope", TEST_KEY, "v0.2.0", &current).unwrap_err().contains("can't be read"));
        let err = verify(EXE, "", SIG, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert_eq!(err, "SHA256SUMS has no entry for glint-v0.2.0-windows-x64.exe");
    }

    #[test]
    fn the_trusted_comment_must_name_this_release() {
        let current = v("0.1.0");
        let name = include_str!("fixtures/wrong-name.minisig");
        let err = verify(EXE, SUMS, name, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert_eq!(err, "glint-v0.2.0-windows-x64.exe was signed as \"glint v0.2.0 glint-v0.2.0-windows-arm64.exe\", not as v0.2.0");
        let version = include_str!("fixtures/wrong-version.minisig");
        let err = verify(EXE, SUMS, version, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert!(err.contains("signed as \"glint v0.1.0 glint-v0.2.0-windows-x64.exe\""), "{err}");
    }

    #[test]
    fn downgrades_are_refused() {
        for current in ["0.2.0", "0.2.1", "1.0.0"] {
            let err = verify(EXE, SUMS, SIG, TEST_KEY, "v0.2.0", &v(current)).unwrap_err();
            assert_eq!(err, format!("refusing v0.2.0: it isn't newer than the running {current}"));
        }
        // an older build, even a genuinely signed one, can't pass as a newer tag
        let renamed = SUMS.replace("v0.2.0", "v0.3.0");
        let err = verify(EXE, &renamed, SIG, TEST_KEY, "v0.3.0", &v("0.2.0")).unwrap_err();
        assert!(err.contains("not as v0.3.0"), "{err}");
    }
}
