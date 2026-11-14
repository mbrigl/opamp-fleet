//! An upstream's OpenPGP signature, verified with `gpgv` against a keyring this tool carries
//! (ADR-draft-the-operator-program-opamp-fleetctl clause 29): a detached signature beside an
//! artifact, or a clear-signed repository index.
//!
//! `gpgv` runs in a home of this process's own, owner-only, holding nothing but the carried keyring
//! and what is verified, so no keyring and no option of the user's is read. The verdict is read
//! from `gpgv`'s machine-readable status lines, never from its prose. Which upstream signs, with
//! which key and from which version on, each kind states.

use std::path::{Path, PathBuf};

/// Verifies a detached OpenPGP `signature` of the file `artifact` against `keyring`, a binary
/// keyring this tool carries, requiring a signature by one of `primaries` or a subkey of it and a
/// run `gpgv` calls a success ([`detached_verdict`]).
///
/// # Errors
/// Names what failed: `gpgv` missing, a signature that does not match, none by a pinned key, or a
/// run `gpgv` failed.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
pub(crate) fn verify_detached(
    signature: &[u8],
    artifact: &Path,
    keyring: &[u8],
    primaries: &[&str],
) -> Result<(), String> {
    let artifact = std::path::absolute(artifact)
        .map_err(|e| format!("cannot resolve {}: {e}", artifact.display()))?;
    in_own_home(keyring, |home, keyring| {
        let signature_path = home.join("signature.asc");
        write(&signature_path, signature)?;
        let (status, succeeded) = gpgv(
            home,
            keyring,
            &[signature_path.as_os_str(), artifact.as_os_str()],
        )?;
        detached_verdict(&status, succeeded, primaries)
    })
}

/// Verifies a clear-signed text, such as a repository's `InRelease`, against `keyring`, requiring a
/// signature by one of `primaries` or a subkey of it ([`verdict`]), and returns the text the
/// signature covers as `gpgv` writes it out — never the file as it came, which may carry more
/// around the signed part. `gpgv`'s exit status is not read: an archive signs with several keys at
/// once, and a signature by a key the keyring does not hold fails the run beside a good one.
///
/// # Errors
/// As [`verify_detached`], and a signed part that is not UTF-8 text.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
pub(crate) fn verify_clearsigned(
    signed: &[u8],
    keyring: &[u8],
    primaries: &[&str],
) -> Result<String, String> {
    in_own_home(keyring, |home, keyring| {
        let input = home.join("signed.asc");
        let covered = home.join("covered");
        write(&input, signed)?;
        let (status, _) = gpgv(
            home,
            keyring,
            &[
                std::ffi::OsStr::new("--output"),
                covered.as_os_str(),
                input.as_os_str(),
            ],
        )?;
        verdict(&status, primaries)?;
        let text =
            std::fs::read(&covered).map_err(|e| format!("cannot read what gpgv verified: {e}"))?;
        String::from_utf8(text).map_err(|_| "the signed text is not text".to_string())
    })
}

/// Runs `with` in a home of this process's own holding `keyring`, and removes the home after it,
/// whatever `with` answered.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
fn in_own_home<T>(
    keyring: &[u8],
    with: impl FnOnce(&Path, &Path) -> Result<T, String>,
) -> Result<T, String> {
    let home = own_home()?;
    let keyring_path = home.join("keyring.gpg");
    let answer = write(&keyring_path, keyring).and_then(|()| with(&home, &keyring_path));
    let _ = std::fs::remove_dir_all(&home);
    answer
}

/// Runs `gpgv` in `home` against `keyring` with `args` after its options, and answers its status
/// lines and whether it succeeded. Every path is absolute: `gpgv` reads a bare keyring name from
/// its home.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
fn gpgv(home: &Path, keyring: &Path, args: &[&std::ffi::OsStr]) -> Result<(String, bool), String> {
    let output = std::process::Command::new("gpgv")
        .arg("--homedir")
        .arg(home)
        .arg("--status-fd")
        .arg("1")
        .arg("--keyring")
        .arg(keyring)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            format!("cannot run gpgv, which verifies upstream's signature (install gpgv): {e}")
        })?;
    Ok((
        String::from_utf8_lossy(&output.stdout).into_owned(),
        output.status.success(),
    ))
}

/// The verdict on what `gpgv` reported, from its machine-readable status lines rather than its
/// prose: no signature failed against the contents (`BADSIG`), and at least one verified
/// (`VALIDSIG`) whose last field — the primary key of the key that made it — is one of
/// `primaries`. A subkey that has expired since it signed is still its primary's (`EXPKEYSIG`
/// beside the `VALIDSIG`). A signature by a key the keyring does not hold (`ERRSIG`) is no
/// evidence either way, since an archive may sign with several keys at once, and does not decide.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
fn verdict(status: &str, primaries: &[&str]) -> Result<(), String> {
    let statuses: Vec<&str> = status
        .lines()
        .filter_map(|line| line.strip_prefix("[GNUPG:] "))
        .collect();
    if statuses.iter().any(|line| line.starts_with("BADSIG")) {
        return Err(
            "the signature does not match what it signs — refusing what nobody vouched for"
                .to_string(),
        );
    }
    let by_primary = statuses.iter().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.first() == Some(&"VALIDSIG")
            && fields.len() == 11
            && primaries
                .iter()
                .any(|primary| fields[10].eq_ignore_ascii_case(primary))
    });
    if !by_primary {
        return Err(format!(
            "carries no valid signature by the key {} — refusing what nobody vouched for",
            primaries.join(" or ")
        ));
    }
    Ok(())
}

/// The verdict on a detached signature: [`verdict`]'s, and a run `gpgv` calls a success. A file
/// beside an artifact carries the one signature its upstream made, so a failing run has nothing
/// beside a good signature to excuse it.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
fn detached_verdict(status: &str, succeeded: bool, primaries: &[&str]) -> Result<(), String> {
    verdict(status, primaries)?;
    if !succeeded {
        return Err(
            "gpgv did not call the signature good — refusing what nobody vouched for".to_string(),
        );
    }
    Ok(())
}

/// A directory of this process's own under the system's temporary directory, owner-only, for
/// `gpgv`'s home.
#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
fn own_home() -> Result<PathBuf, String> {
    // The clock and a count: two verifications in one process within one clock tick get two.
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!(
        "opamp-fleetctl-gpgv-{}-{nonce}-{count}",
        std::process::id()
    ));
    // `create_dir`, not `create_dir_all`: a name that already exists is somebody else's.
    std::fs::create_dir(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    }
    Ok(dir)
}

#[allow(
    dead_code,
    reason = "a kind whose upstream signs with OpenPGP verifies with it, and the core carries none"
)]
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The throwaway keys the fixtures under `testdata/openpgp/` were made with, for these tests
    /// alone: a signer whose primary key certifies and whose subkey signs — the shape of the keys
    /// upstreams sign with — and a stranger with a key of its own.
    const SIGNER: &str = "4276DFDC00A7EF87A420D843C4EEBCCE5907EBA2";
    /// The signer's signing subkey, which made every signature of the signer's.
    #[cfg(target_os = "linux")]
    const SIGNER_SUBKEY: &str = "D19F352E56C964A32A69AB4CB049168EAAA0597E";
    #[cfg(target_os = "linux")]
    const STRANGER: &str = "091CC95B8E451EBD8DF94185797DCCA461EAB251";
    #[cfg(target_os = "linux")]
    const SIGNER_KEYRING: &[u8] = include_bytes!("../testdata/openpgp/signer.gpg");
    #[cfg(target_os = "linux")]
    const BOTH_KEYRING: &[u8] = include_bytes!("../testdata/openpgp/signer-and-stranger.gpg");

    /// A detached signature verifies only as made by a pinned key over these very bytes: an
    /// altered byte is refused as a bad signature, a signature by a key the keyring does not hold
    /// is refused, and so is one by a key the keyring holds beside the pinned one. What a server
    /// answers in place of a signature verifies nothing.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[cfg(target_os = "linux")]
    #[test]
    fn a_detached_signature_verifies_only_by_a_pinned_key_over_its_bytes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("artifact");
        let bytes = include_bytes!("../testdata/openpgp/artifact");
        std::fs::write(&artifact, bytes).expect("artifact");
        let by_signer = include_bytes!("../testdata/openpgp/artifact.asc");
        let by_stranger = include_bytes!("../testdata/openpgp/artifact.stranger.asc");

        verify_detached(by_signer, &artifact, SIGNER_KEYRING, &[SIGNER])
            .expect("signed by the signer's subkey");
        verify_detached(by_signer, &artifact, BOTH_KEYRING, &[SIGNER])
            .expect("a second key in the keyring changes nothing");
        verify_detached(by_signer, &artifact, BOTH_KEYRING, &[STRANGER, SIGNER])
            .expect("one of the pinned keys is enough");

        let mut altered = bytes.to_vec();
        altered[0] ^= 1;
        std::fs::write(&artifact, &altered).expect("altered");
        let refused =
            verify_detached(by_signer, &artifact, SIGNER_KEYRING, &[SIGNER]).expect_err("altered");
        assert!(refused.contains("does not match"), "{refused}");
        std::fs::write(&artifact, bytes).expect("artifact");

        let refused = verify_detached(by_stranger, &artifact, SIGNER_KEYRING, &[SIGNER])
            .expect_err("a key the keyring does not hold");
        assert!(refused.contains("nobody vouched for"), "{refused}");
        let refused = verify_detached(by_stranger, &artifact, BOTH_KEYRING, &[SIGNER])
            .expect_err("a key the keyring holds, but not the pinned one");
        assert!(refused.contains(SIGNER), "{refused}");
        verify_detached(by_signer, &artifact, SIGNER_KEYRING, &[SIGNER_SUBKEY])
            .expect_err("a subkey pinned is not its primary");
        verify_detached(by_signer, &artifact, b"", &[SIGNER]).expect_err("an empty keyring");
        let page = b"<!doctype html><html><body>Not Found</body></html>\n";
        verify_detached(page, &artifact, SIGNER_KEYRING, &[SIGNER])
            .expect_err("a page is no signature");
    }

    /// A clear-signed text verifies only as made by a pinned key, and what comes back is the text
    /// the signature covers, without its armour: an altered byte inside the signed part, a
    /// signature by a key the keyring does not hold, and one by a key it holds but does not pin
    /// are each refused.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[cfg(target_os = "linux")]
    #[test]
    fn a_clear_signed_text_yields_only_what_a_pinned_key_signed() {
        let signed = include_str!("../testdata/openpgp/InRelease");
        let by_stranger = include_bytes!("../testdata/openpgp/InRelease.stranger");

        let text = verify_clearsigned(signed.as_bytes(), SIGNER_KEYRING, &[SIGNER])
            .expect("signed by the signer's subkey");
        assert!(text.starts_with("Origin: Fixture\n"), "{text}");
        assert!(!text.contains("BEGIN PGP"), "only the signed text: {text}");

        let altered = signed.replacen("Codename: fixture", "Codename: fixturf", 1);
        let refused =
            verify_clearsigned(altered.as_bytes(), SIGNER_KEYRING, &[SIGNER]).expect_err("altered");
        assert!(refused.contains("does not match"), "{refused}");
        // Text put before the armour is not covered, and is not what comes back.
        let prefixed = format!("Codename: injected\n{signed}");
        if let Ok(text) = verify_clearsigned(prefixed.as_bytes(), SIGNER_KEYRING, &[SIGNER]) {
            assert!(!text.contains("injected"), "{text}");
        }

        let refused = verify_clearsigned(by_stranger, SIGNER_KEYRING, &[SIGNER])
            .expect_err("a key the keyring does not hold");
        assert!(refused.contains("nobody vouched for"), "{refused}");
        let refused = verify_clearsigned(by_stranger, BOTH_KEYRING, &[SIGNER])
            .expect_err("a key the keyring holds, but not the pinned one");
        assert!(refused.contains(SIGNER), "{refused}");
        verify_clearsigned(b"Origin: unsigned\n", SIGNER_KEYRING, &[SIGNER])
            .expect_err("an unsigned text");
    }

    /// The verdict is read from `gpgv`'s status lines: one good signature by a pinned primary key
    /// or a subkey of it is needed, a subkey that has expired since it signed still counts, a bad
    /// signature anywhere refuses, and one by a key the keyring does not hold decides nothing.
    /// The lines are `gpgv`'s status lines for an upstream's and Debian's signatures.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[test]
    fn a_signature_verdict_needs_a_pinned_primary_and_no_bad_signature() {
        let upstream = "24C975CBA61A024EE1B631787C3D57159FC2F927";
        let current = "[GNUPG:] NEWSIG\n[GNUPG:] KEYEXPIRED 1768664487\n\
            [GNUPG:] GOODSIG DA61C26A0585BD3B Upstream Package Signing Key <signing@upstream.example>\n\
            [GNUPG:] VALIDSIG AC10D7449F343ADCEFDDC2B6DA61C26A0585BD3B 2026-10-12 1791815708 0 4 0 1 \
            10 00 24C975CBA61A024EE1B631787C3D57159FC2F927\n";
        let expired_since = "[GNUPG:] NEWSIG\n[GNUPG:] KEYEXPIRED 1768664487\n\
            [GNUPG:] EXPKEYSIG D8FF8E1F7DF8B07E Upstream Package Signing Key <signing@upstream.example>\n\
            [GNUPG:] VALIDSIG 9D539D90D3328DC7D6C8D3B9D8FF8E1F7DF8B07E 2025-09-08 1757364015 0 4 0 1 \
            10 00 24C975CBA61A024EE1B631787C3D57159FC2F927\n";
        let beside_an_unknown = format!("[GNUPG:] ERRSIG 6ED0E7B82643E131 1 8\n{current}");
        for accepted in [current, expired_since, &beside_an_unknown] {
            verdict(accepted, &[upstream]).expect(accepted);
            verdict(accepted, &[SIGNER, upstream]).expect("one of the pinned keys");
        }
        verdict(current, &[SIGNER]).expect_err("another primary");
        for refused in [
            "[GNUPG:] NEWSIG\n[GNUPG:] ERRSIG A928A4141FCA213C 22 8 00 1793175148 9 \
             4D61513B7079F1D51D106975A928A4141FCA213C\n[GNUPG:] NO_PUBKEY A928A4141FCA213C\n",
            "[GNUPG:] BADSIG DA61C26A0585BD3B Upstream Package Signing Key\n",
            &format!("{current}[GNUPG:] BADSIG 6ED0E7B82643E131 Debian\n"),
            // A subkey's own fingerprint is not its primary's.
            "[GNUPG:] VALIDSIG 24C975CBA61A024EE1B631787C3D57159FC2F927 2026-10-12 1791815708 0 4\n",
            "gpgv: Good signature from \"Upstream Package Signing Key\"\n",
            "",
        ] {
            verdict(refused, &[upstream]).expect_err(refused);
        }
        verdict(current, &[]).expect_err("nothing pinned");
    }

    /// A detached signature also needs a run `gpgv` calls a success: the same status lines that
    /// carry a pinned `VALIDSIG` and no `BADSIG` are refused when `gpgv` failed, while a
    /// clear-signed text, which an archive signs with several keys at once, is judged by the lines
    /// alone.
    /// Verifies: ADR-draft-the-operator-program-opamp-fleetctl
    #[test]
    fn a_detached_signature_needs_a_run_gpgv_calls_a_success() {
        let upstream = "24C975CBA61A024EE1B631787C3D57159FC2F927";
        let good = "[GNUPG:] NEWSIG\n\
            [GNUPG:] GOODSIG DA61C26A0585BD3B Upstream Package Signing Key <signing@upstream.example>\n\
            [GNUPG:] VALIDSIG AC10D7449F343ADCEFDDC2B6DA61C26A0585BD3B 2026-10-12 1791815708 0 4 0 1 \
            10 00 24C975CBA61A024EE1B631787C3D57159FC2F927\n";
        detached_verdict(good, true, &[upstream]).expect("good, and gpgv succeeded");
        let refused = detached_verdict(good, false, &[upstream]).expect_err("gpgv failed");
        assert!(refused.contains("nobody vouched for"), "{refused}");
        detached_verdict(
            "[GNUPG:] BADSIG DA61C26A0585BD3B Upstream\n",
            true,
            &[upstream],
        )
        .expect_err("a bad signature, whatever the exit status");
        let beside_an_unknown = format!("[GNUPG:] ERRSIG 6ED0E7B82643E131 1 8\n{good}");
        detached_verdict(&beside_an_unknown, false, &[upstream])
            .expect_err("a second, unverifiable signature fails a detached one");
        verdict(&beside_an_unknown, &[upstream]).expect("a clear-signed text's verdict");
    }
}
