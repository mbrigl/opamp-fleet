//! The password a password file holds, read the way the Client's own setup reads one; the operator
//! tool reads the operator's password files with it too.

use std::path::Path;

/// The password a password file holds: its first line, without its `\n` or `\r\n` (ADR-0027 §18).
/// The file is read once and left as it is: the administrator's path, read as
/// [`crate::nofollow::read_privileged`] reads one with `roots` as the service's account's, and a
/// file of more than 4 KiB is refused (ADR-0022 §11). `flag` is the option that named it,
/// such as the Client's `--bootstrap-password-file`, for the error. The file's bytes and the
/// password are held in buffers wiped when they are dropped (ADR-0015).
///
/// # Errors
/// Returns an error naming the flag when the file cannot be read or its first line is empty.
pub fn read_password_file(
    path: &Path,
    flag: &str,
    roots: &[&Path],
) -> Result<zeroize::Zeroizing<String>, String> {
    const LIMIT: u64 = 4096;
    let bytes = crate::nofollow::read_privileged(
        path,
        roots,
        None,
        crate::nofollow::Named::ByAdministrator,
        LIMIT,
    )
    .map_err(|e| format!("{flag}: {e}"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("{flag} {}: not UTF-8 text", path.display()))?;
    let line = text.split('\n').next().unwrap_or_default();
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.is_empty() {
        return Err(format!(
            "{flag} {}: its first line is empty",
            path.display()
        ));
    }
    Ok(zeroize::Zeroizing::new(line.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The password is the file's first line without its line ending; an empty first line, a
    /// missing file and a file over 4 KiB are refused, naming the flag (ADR-0027 §18).
    /// Verifies: ADR-0027
    #[test]
    fn a_password_file_gives_its_first_line_and_refuses_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("pass");
        for (content, expected) in [
            ("secret password\n", "secret password"),
            ("secret password\r\nsecond line\n", "secret password"),
            ("secret password", "secret password"),
        ] {
            std::fs::write(&file, content).expect("write");
            let read = read_password_file(&file, "--password-file", &[]).expect("read");
            assert_eq!(read.as_str(), expected);
        }
        std::fs::write(&file, "\nsecret").expect("write");
        let refused = read_password_file(&file, "--password-file", &[]).expect_err("empty");
        assert!(refused.contains("--password-file"), "{refused}");
        assert!(read_password_file(&dir.path().join("none"), "--password-file", &[]).is_err());
        std::fs::write(&file, "x".repeat(4097)).expect("write");
        assert!(
            read_password_file(&file, "--password-file", &[]).is_err(),
            "over 4 KiB"
        );
    }
}
