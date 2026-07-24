use crate::install;
    let exe = layout::running_exe()?;
    // Process's package comes in, which is why the unpacking is shared with the Supervisor's swap.
    install::write_program(artifact, &binary, BINARY_FILENAME, archive_key)?;
/// Runs `<binary> self-check`, retrying briefly past `ETXTBSY`
/// ([`install::is_text_file_busy`] states why).
/// The loop stays here rather than being shared with the Supervisor's: that one drives a
/// `tokio::process` spawn and this one a blocking `std::process` run, and the only thing they would
/// have in common after being generalised over both is the predicate and the two constants they
/// already share.
            Err(e) if install::is_text_file_busy(&e) && attempt < install::BUSY_RETRIES => {
                std::thread::sleep(install::BUSY_DELAY);
    let reported = reported.trim();
    // The commit the binary was built from is provenance, not identity (ADR-0011): it is the one
    // part of the string an operator neither knows nor can type when uploading a release, and
    // SemVer itself says metadata is ignored when versions are compared. The pre-release is *not*
    // dropped — a `-dev` build is not the release it heads for, and this is the last gate that can
    // say so before a fleet installs one. A value that is not a version at all matches nothing.
    if !opamp::version::same_release(reported, expected_version) {
            "the staged binary reports version {reported:?}, but the package offered \
             {expected_version:?}"
    let exe = layout::running_exe()?;

    /// ADR-0011, and the failure that prompted it: a package is uploaded under the release number,
    /// while the binary in it reports the commit it was built from. Those are the same release.
    #[cfg(unix)]
    #[test]
    fn the_probe_ignores_the_commit_a_build_came_from() {
        let binary = self_check_stub(dir.path(), "0.1.1+799e36a");

        probe(&binary, "0.1.1").expect("the release number is what an operator uploads");
        probe(&binary, "0.1.1+799e36a").expect("and the full string still works");
        // A rebuild of the same release passes too; which bytes arrived is the content hash's
        // question (ADR-0028), never this one.
        probe(&binary, "0.1.1+deadbee").expect("same release, other build");
    }

    /// What is deliberately *not* dropped: a development build is not the release it heads for
    /// (ADR-0011), and this is the last gate that can refuse one before a fleet installs it.
    #[cfg(unix)]
    #[test]
    fn the_probe_refuses_a_development_build_offered_as_a_release() {
        let binary = self_check_stub(dir.path(), "0.1.1-dev+799e36a");

        let err = probe(&binary, "0.1.1").expect_err("a -dev build is not the release");
        assert!(err.contains("reports version"), "got {err}");
        probe(&binary, "0.1.1-dev").expect("offered as what it is, it installs");
    }

    /// A package version is free-form by the API's own contract, so the offer may not be a version
    /// at all — including the `0.1.1 799e36a` a query string makes of an unencoded `+`.
    #[cfg(unix)]
    #[test]
    fn the_probe_refuses_an_offer_that_is_not_a_version() {
        let binary = self_check_stub(dir.path(), "0.1.1+799e36a");

        for offered in ["0.1.1 799e36a", "latest", "v0.1.1", ""] {
            assert!(
                probe(&binary, offered).is_err(),
                "{offered:?} must not install"
            );
        }
    }

    /// A stand-in for a staged Client: it answers the self-check with the version it is told to.
    #[cfg(unix)]
    fn self_check_stub(dir: &Path, reports: &str) -> std::path::PathBuf {

        let binary = dir.join("staged-client");
        std::fs::write(
            &binary,
            format!("#!/bin/sh\necho '{SELF_CHECK_TOKEN}{reports}'\n"),
        binary
    }
