//! A withdrawal that is due and not yet done, kept on disk until it is (ADR-0059 clauses 1 and 6).
//!
//! On Windows what a former account was granted is withdrawn after the SCM names its successor:
//! before, the former account's service could still start and run under it. Once the SCM has
//! switched, nothing names the former account any more — a second service dropped is deregistered,
//! and a changed logon account is overwritten — so an install that failed between the switch and
//! the end of the withdrawal would leave the grants in place with nothing to find them by. The
//! accounts due and the roots to walk are therefore written down before the switch, atomically, and
//! removed only once the withdrawal has finished; every install completes what it finds written.
//!
//! The record is a text file in the data root: a line `client <SID>` or `process <SID>` per former
//! account, by the part it had, and `root <path>` per root. On Windows it is created carrying a
//! protected DACL that grants `LocalSystem` and the Administrators alone ([`PRIVATE`]), shared with
//! no other handle while it is written, and read only where it still carries that DACL and one of
//! them owns it. The Client's account, which may modify the data root, is granted neither `DELETE`
//! on the record nor `FILE_DELETE_CHILD` on the root, and so can change it not at all; where a
//! process account is registered it holds `WRITE_DAC` on the root, and could grant itself the
//! second and so remove the record: a gap a file in the data root leaves open. Unix withdraws by a change of owner and needs no record; the logic is compiled
//! everywhere so that its tests run everywhere.

use std::path::{Path, PathBuf};

/// The record's name in the data root.
pub const FILE_NAME: &str = "withdrawal-pending";

/// The DACL the record carries on Windows: full control for `LocalSystem` and the Administrators,
/// and nothing for anyone else, inherited from no parent.
#[cfg(windows)]
const PRIVATE: &str = "D:P(A;;FA;;;SY)(A;;FA;;;BA)";

/// The part a former account had: the Client's, or the process account's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Client,
    Process,
}

impl Part {
    fn key(self) -> &'static str {
        match self {
            Part::Client => "client",
            Part::Process => "process",
        }
    }
}

/// The former accounts, by part and SID, whose grants are still to be withdrawn, and the roots to
/// withdraw them from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Pending {
    pub formers: Vec<(Part, String)>,
    pub roots: Vec<PathBuf>,
}

impl Pending {
    /// Whether nothing is due.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.formers.is_empty()
    }

    /// Adds the former accounts `formers` and the roots `roots`, each once.
    pub fn add(&mut self, formers: impl IntoIterator<Item = (Part, String)>, roots: &[&Path]) {
        for former in formers {
            if !self.formers.contains(&former) {
                self.formers.push(former);
            }
        }
        for root in roots {
            if !self.roots.iter().any(|known| known == root) {
                self.roots.push(root.to_path_buf());
            }
        }
    }

    /// The record's text.
    ///
    /// # Errors
    /// Returns that a SID or a root cannot be written on one line.
    pub fn text(&self) -> Result<String, String> {
        let mut text = String::new();
        for (part, sid) in &self.formers {
            text.push_str(&line(part.key(), sid)?);
        }
        for root in &self.roots {
            let root = root
                .to_str()
                .ok_or_else(|| format!("{} is no Unicode path", root.display()))?;
            text.push_str(&line("root", root)?);
        }
        Ok(text)
    }

    /// Reads the record's text.
    ///
    /// # Errors
    /// Returns the first line that is neither a former account nor a root.
    pub fn parse(text: &str) -> Result<Pending, String> {
        let mut pending = Pending::default();
        for line in text.lines().filter(|line| !line.is_empty()) {
            match line.split_once(' ') {
                Some(("client", sid)) => pending.formers.push((Part::Client, sid.to_string())),
                Some(("process", sid)) => pending.formers.push((Part::Process, sid.to_string())),
                Some(("root", root)) => pending.roots.push(PathBuf::from(root)),
                _ => return Err(format!("{line:?} is no line of a pending withdrawal")),
            }
        }
        Ok(pending)
    }
}

fn line(key: &str, value: &str) -> Result<String, String> {
    if value.is_empty() || value.contains(['\n', '\r']) {
        return Err(format!("{value:?} cannot be recorded on one line"));
    }
    Ok(format!("{key} {value}\n"))
}

/// The withdrawal recorded at `path`; nothing where there is no record.
///
/// # Errors
/// Returns that the record cannot be read or is not one — on Windows also one that no longer
/// carries the DACL it was written with, or that neither `LocalSystem` nor the Administrators own:
/// what it says cannot be trusted, and what it would have withdrawn cannot be known, so the install
/// changes nothing until an Administrator has looked at it and removed it.
pub fn read(path: &Path) -> Result<Pending, String> {
    let refused = |e: String| {
        format!(
            "{} records a withdrawal that is due and cannot be read: {e}. Nothing was changed",
            path.display()
        )
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pending::default()),
        Err(e) => return Err(refused(e.to_string())),
    };
    #[cfg(windows)]
    check_private(path).map_err(refused)?;
    Pending::parse(&text).map_err(refused)
}

/// Whether the record at `path` carries [`PRIVATE`] and is owned by `LocalSystem` or the
/// Administrators.
#[cfg(windows)]
fn check_private(path: &Path) -> Result<(), String> {
    use crate::win32::files::{Access, Entry};
    let entry = Entry::open(path, Access::Read, false).map_err(|e| e.to_string())?;
    let owner = entry.owner().map_err(|e| e.to_string())?;
    if !matches!(owner.as_str(), "S-1-5-18" | "S-1-5-32-544") {
        return Err(format!("it is owned by {owner}"));
    }
    let dacl = entry.dacl().map_err(|e| e.to_string())?;
    if !crate::win32::same_dacl(&dacl.text(), PRIVATE).map_err(|e| e.to_string())? {
        return Err(format!("it carries {}", dacl.text()));
    }
    Ok(())
}

/// Records `pending` at `path`, replacing what was recorded there, atomically: written to a file
/// of its own beside it, then renamed over it.
///
/// # Errors
/// Returns why it cannot be written.
pub fn record(path: &Path, pending: &Pending) -> Result<(), String> {
    use std::io::Write as _;
    let failed =
        |e: std::io::Error| format!("cannot record the withdrawal at {}: {e}", path.display());
    let text = pending.text()?;
    let (written, mut file) = create_beside(path).map_err(failed)?;
    let done = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let done = done.and_then(|()| std::fs::rename(&written, path));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&written);
        return Err(failed(e));
    }
    Ok(())
}

/// A new file beside `path` with a name of its own, created private: none that someone made
/// ahead of it is ever written.
fn create_beside(path: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    let mut last = None;
    for attempt in 0..16u32 {
        let name = format!("{FILE_NAME}.{}.{nanos:x}.{attempt}", std::process::id());
        let candidate = path.with_file_name(name);
        match create_private(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no name left")))
}

#[cfg(windows)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    crate::win32::create_private(path, PRIVATE)
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Completes the withdrawal `pending`, recorded at `path`: `withdraw` for every former account but
/// those of `kept` — the accounts the registration names now in the same part, which an operator
/// may have switched back to — over every root, and the record removed once all are done. A
/// former account named now in the other part is withdrawn all the same: what it is granted in
/// that part is granted again after. A withdrawal that fails leaves the record for the next
/// install.
///
/// # Errors
/// Returns the first withdrawal that failed, or that the record cannot be removed.
pub fn complete(
    path: &Path,
    pending: &Pending,
    kept: &[(Part, &str)],
    mut withdraw: impl FnMut(&str, &[&Path]) -> Result<(), String>,
) -> Result<(), String> {
    let roots: Vec<&Path> = pending.roots.iter().map(PathBuf::as_path).collect();
    let mut done: Vec<&str> = Vec::new();
    for (part, sid) in &pending.formers {
        if kept.contains(&(*part, sid.as_str())) || done.contains(&sid.as_str()) {
            continue;
        }
        withdraw(sid, &roots)?;
        done.push(sid);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "the withdrawal is done, and its record {} is left: {e}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A withdrawal that failed — an install interrupted after the SCM switched — is still
    /// recorded, and the next install, which finds no former account of its own, completes it from
    /// the record and removes it; one that succeeds leaves no record (ADR-0059 clauses 1 and 6).
    /// Verifies: ADR-0059
    #[test]
    fn an_interrupted_withdrawal_is_completed_by_the_next_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        let (root, config) = (dir.path().join("root"), dir.path().join("supervisor.toml"));
        assert_eq!(read(&path), Ok(Pending::default()), "nothing recorded");

        // The first install: two former accounts, recorded before the switch.
        let mut due = read(&path).expect("read");
        due.add(
            [
                (Part::Process, "S-1-5-19".to_string()),
                (Part::Client, "S-1-5-80-1".to_string()),
            ],
            &[&root, &config],
        );
        record(&path, &due).expect("recorded");
        let mut asked = Vec::new();
        let interrupted = complete(&path, &due, &[], |sid, roots| {
            asked.push((sid.to_string(), roots.len()));
            if sid == "S-1-5-80-1" {
                return Err("held".to_string());
            }
            Ok(())
        });
        assert_eq!(interrupted, Err("held".to_string()));
        assert_eq!(
            asked,
            [("S-1-5-19".to_string(), 2), ("S-1-5-80-1".to_string(), 2)]
        );
        assert!(path.exists(), "a withdrawal that failed stays recorded");

        // The next install names no former account; it finds the record, adds what it walks, and
        // completes it — but not for an account the registration names again in the same part,
        // while one named now in the other part is withdrawn all the same.
        let mut due = read(&path).expect("read");
        assert_eq!(
            due.formers,
            [
                (Part::Process, "S-1-5-19".to_string()),
                (Part::Client, "S-1-5-80-1".to_string())
            ]
        );
        due.add(Vec::new(), &[&root, &dir.path().join("state")]);
        assert_eq!(due.roots.len(), 3, "each root once");
        record(&path, &due).expect("recorded");
        let mut asked = Vec::new();
        complete(
            &path,
            &due,
            &[(Part::Process, "S-1-5-19"), (Part::Process, "S-1-5-80-1")],
            |sid, roots| {
                asked.push((sid.to_string(), roots.len()));
                Ok(())
            },
        )
        .expect("completed");
        assert_eq!(asked, [("S-1-5-80-1".to_string(), 3)]);
        assert!(!path.exists(), "a completed withdrawal leaves no record");
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("dir").count(),
            0,
            "and nothing beside it"
        );
    }

    /// The record is read back as it was written, and what is no record is refused rather than
    /// passed over.
    /// Verifies: ADR-0059
    #[test]
    fn a_record_reads_back_and_what_is_none_is_refused() {
        let mut pending = Pending::default();
        pending.add(
            [(Part::Client, "S-1-5-20".to_string())],
            &[Path::new("/data root/x")],
        );
        let text = pending.text().expect("text");
        assert_eq!(text, "client S-1-5-20\nroot /data root/x\n");
        assert_eq!(Pending::parse(&text), Ok(pending));
        assert!(Pending::parse("withdraw everything\n").is_err());
        let mut broken = Pending::default();
        broken.add([(Part::Client, "S-1-5-20\nroot C:\\".to_string())], &[]);
        assert!(broken.text().is_err(), "no SID writes a second line");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "nonsense").expect("written");
        let refused = read(&path).expect_err("refused");
        assert!(refused.contains("Nothing was changed"), "{refused}");
    }

    /// On Windows a record someone else made — a file written in its place by an account that may
    /// write the data root — carries neither the private DACL nor, necessarily, a trusted owner,
    /// and is refused; one of its own is read.
    /// Verifies: ADR-0059
    #[cfg(windows)]
    #[test]
    fn on_windows_a_record_not_written_private_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        let mut pending = Pending::default();
        pending.add([(Part::Process, "S-1-5-19".to_string())], &[dir.path()]);
        record(&path, &pending).expect("recorded");
        assert_eq!(
            read(&path),
            Ok(pending.clone()),
            "a record of its own is read"
        );
        std::fs::remove_file(&path).expect("removed");
        std::fs::write(&path, pending.text().expect("text")).expect("written in its place");
        let refused = read(&path).expect_err("refused");
        assert!(
            refused.contains("it carries") || refused.contains("it is owned by"),
            "{refused}"
        );
    }
}
