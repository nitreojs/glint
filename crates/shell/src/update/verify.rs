use glint_core::update::Product;
use glint_core::update::verify as checks;
use glint_core::update::version::Version;
use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};

pub(crate) fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
    let mut out = [0u8; 32];
    let status = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) };
    status.ok().map_err(|e| format!("can't compute sha-256: {}", e.message()))?;
    Ok(out)
}

pub(crate) fn verify(
    app: &Product,
    exe: &[u8],
    sums: &str,
    sig: &str,
    key: &str,
    tag: &str,
    current: &Version,
) -> Result<(), String> {
    checks::verify(app, exe, sums, sig, key, tag, current, sha256)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use glint_core::update::verify::{hex, sums_entry};
    use glint_core::update::{AFTERGLOW, GLINT};

    /// A throwaway minisign key that signed the fixtures; never used for releases.
    const TEST_KEY: &str = "RWQr5QlLKaagCabzmy83MJZXq1/hidI5B+tyy/ykArTUeaZMkmNETif5";
    const EXE: &[u8] = include_bytes!("fixtures/fixture.bin");
    const SIG: &str = include_str!("fixtures/fixture.bin.minisig");
    const SUMS: &str = include_str!("fixtures/SHA256SUMS");
    /// Another throwaway key, which signed the afterglow fixtures.
    pub(in crate::update) const AFTERGLOW_TEST_KEY: &str = "RWSfenV/kCdLJyD7I8uY3VSPiuBTh8sXW3QnEl4G+9FXcHrcnC6Hhu96";
    pub(in crate::update) const AFTERGLOW_EXE: &[u8] = include_bytes!("fixtures/afterglow.bin");
    pub(in crate::update) const AFTERGLOW_SIG: &str = include_str!("fixtures/afterglow.bin.minisig");
    pub(in crate::update) const AFTERGLOW_SUMS: &str = include_str!("fixtures/afterglow-SHA256SUMS");

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
        assert_eq!(GLINT.trusted_comment("v0.2.0"), "glint v0.2.0 glint-v0.2.0-windows-x64.exe");
        assert_eq!(verify(&GLINT, EXE, SUMS, SIG, TEST_KEY, "v0.2.0", &v("0.1.0")), Ok(()));
        assert_eq!(verify(&GLINT, EXE, SUMS, SIG, TEST_KEY, "v0.2.0", &v("0.2.0-beta.4")), Ok(()));
    }

    #[test]
    fn tampering_is_refused() {
        let current = v("0.1.0");
        let mut damaged = EXE.to_vec();
        damaged[40] ^= 1;
        assert!(verify(&GLINT, &damaged, SUMS, SIG, TEST_KEY, "v0.2.0", &current).unwrap_err().contains("is damaged"));
        // a matching checksum doesn't help without the signature
        let resummed = format!("{}  glint-v0.2.0-windows-x64.exe\n", hex(&sha256(&damaged).unwrap()));
        let err = verify(&GLINT, &damaged, &resummed, SIG, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert!(err.contains("isn't signed by glint's release key"), "{err}");
        let other = include_str!("fixtures/other-key.minisig");
        assert!(verify(&GLINT, EXE, SUMS, other, TEST_KEY, "v0.2.0", &current).unwrap_err().contains("isn't signed"));
        assert!(verify(&GLINT, EXE, SUMS, "nope", TEST_KEY, "v0.2.0", &current).unwrap_err().contains("can't be read"));
        let err = verify(&GLINT, EXE, "", SIG, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert_eq!(err, "SHA256SUMS has no entry for glint-v0.2.0-windows-x64.exe");
    }

    #[test]
    fn the_trusted_comment_must_name_this_release() {
        let current = v("0.1.0");
        let name = include_str!("fixtures/wrong-name.minisig");
        let err = verify(&GLINT, EXE, SUMS, name, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert_eq!(err, "glint-v0.2.0-windows-x64.exe was signed as \"glint v0.2.0 glint-v0.2.0-windows-arm64.exe\", not as v0.2.0");
        let version = include_str!("fixtures/wrong-version.minisig");
        let err = verify(&GLINT, EXE, SUMS, version, TEST_KEY, "v0.2.0", &current).unwrap_err();
        assert!(err.contains("signed as \"glint v0.1.0 glint-v0.2.0-windows-x64.exe\""), "{err}");
    }

    #[test]
    fn downgrades_are_refused() {
        for current in ["0.2.0", "0.2.1", "1.0.0"] {
            let err = verify(&GLINT, EXE, SUMS, SIG, TEST_KEY, "v0.2.0", &v(current)).unwrap_err();
            assert_eq!(err, format!("refusing v0.2.0: it isn't newer than the running {current}"));
        }
        // an older build, even a genuinely signed one, can't pass as a newer tag
        let renamed = SUMS.replace("v0.2.0", "v0.3.0");
        let err = verify(&GLINT, EXE, &renamed, SIG, TEST_KEY, "v0.3.0", &v("0.2.0")).unwrap_err();
        assert!(err.contains("not as v0.3.0"), "{err}");
    }

    #[test]
    fn afterglow_releases_verify_as_afterglow_only() {
        let (exe, sums, sig, key) = (AFTERGLOW_EXE, AFTERGLOW_SUMS, AFTERGLOW_SIG, AFTERGLOW_TEST_KEY);
        assert_eq!(verify(&AFTERGLOW, exe, sums, sig, key, "afterglow-v0.2.0", &v("0.1.0")), Ok(()));
        let err = verify(&AFTERGLOW, exe, sums, sig, key, "afterglow-v0.2.0", &v("0.2.0")).unwrap_err();
        assert_eq!(err, "refusing afterglow-v0.2.0: it isn't newer than the running 0.2.0");
        // glint's tag for the same version names another file and another comment
        let err = verify(&GLINT, exe, sums, sig, key, "v0.2.0", &v("0.1.0")).unwrap_err();
        assert_eq!(err, "SHA256SUMS has no entry for glint-v0.2.0-windows-x64.exe");
        let err = verify(&AFTERGLOW, exe, sums, sig, key, "v0.2.0", &v("0.1.0")).unwrap_err();
        assert_eq!(err, "v0.2.0 isn't a release of afterglow");
        // signed by the workflow before it named the app in front of the tag
        let old = include_str!("fixtures/afterglow-old-comment.minisig");
        let err = verify(&AFTERGLOW, exe, sums, old, key, "afterglow-v0.2.0", &v("0.1.0")).unwrap_err();
        assert!(err.contains("was signed as \"afterglow-v0.2.0 afterglow-v0.2.0-windows-x64.exe\""), "{err}");
        assert!(verify(&AFTERGLOW, exe, sums, sig, TEST_KEY, "afterglow-v0.2.0", &v("0.1.0")).unwrap_err().contains("isn't signed"));
    }
}
