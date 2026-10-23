//! A withdrawal that is due and not yet done, kept until it is (ADR-0059 clauses 1 and 6).
//!
//! On Windows what a former account was granted is withdrawn after the SCM names its successor:
//! before, the former account's service could still start and run under it. Once the SCM has
//! switched, nothing names the former account any more — a second service dropped is deregistered,
//! and a changed logon account is overwritten — so an install that failed between the switch and
//! the end of the withdrawal would leave the grants in place with nothing to find them by. The
//! accounts due and the roots to walk are therefore written down before the switch, in one write,
//! and removed only once the withdrawal has finished; every install completes what it finds
//! written, and an uninstall completes it before it deregisters anything.
//!
//! The record is text: a line `client <SID>` or `process <SID>` per former account, by the part it
//! had, and `root <path>` per root. On Windows it is the string value [`VALUE`] of the key
//! `HKEY_LOCAL_MACHINE\SOFTWARE\<PRODUCT_NAME>\`[`KEY`], which the install creates carrying a
//! protected DACL that grants `LocalSystem` and the Administrators alone ([`PRIVATE`]) and reads
//! only where it still carries that DACL and one of them owns it. Nothing the Client or a Managed
//! Process may write holds it: not the data root, whose DACL the Client may change where a process
//! account is registered, and not a path a later `--data-root` would name differently. The key is
//! the install's own rather than one beneath the service's key in the SCM's tree, whose DACL is
//! the system's, undocumented, and gone with `sc delete`. Unix withdraws by a change of owner and
//! needs no record; the logic is compiled everywhere, over a [`Store`], so that its tests run
//! everywhere.

use std::path::{Path, PathBuf};

/// Where a record is kept: a string, or none.
pub trait Store {
    /// The record's text; `None` where nothing is recorded.
    ///
    /// # Errors
    /// Returns why it cannot be read, or why what holds it cannot be trusted.
    fn load(&self) -> Result<Option<String>, String>;
    /// Replaces the record with `text`, in one write.
    ///
    /// # Errors
    /// Returns why it cannot be written.
    fn save(&self, text: &str) -> Result<(), String>;
    /// Removes the record; none there is no error.
    ///
    /// # Errors
    /// Returns why it cannot be removed.
    fn clear(&self) -> Result<(), String>;
    /// Where it is kept, for a message.
    fn describe(&self) -> String;
}

/// The key beneath `SOFTWARE\<PRODUCT_NAME>` that holds the record on Windows.
pub const KEY: &str = "WithdrawalPending";
/// The string value of [`KEY`] that is the record.
pub const VALUE: &str = "Pending";

/// The DACL [`KEY`] carries: full control for `LocalSystem` and the Administrators, nothing for
/// anyone else, inherited from no parent.
#[cfg(windows)]
const PRIVATE: &str = "D:P(A;;KA;;;SY)(A;;KA;;;BA)";

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

/// The withdrawal recorded in `store`; nothing where there is no record.
///
/// # Errors
/// Returns that the record cannot be read or is not one — on Windows also one whose key no longer
/// carries the DACL it was created with, or that neither `LocalSystem` nor the Administrators own:
/// what it says cannot be trusted, and what it would have withdrawn cannot be known, so the install
/// changes nothing until an Administrator has looked at it and removed it.
pub fn read(store: &dyn Store) -> Result<Pending, String> {
    let refused = |e: String| {
        format!(
            "{} records a withdrawal that is due and cannot be read: {e}. Nothing was changed",
            store.describe()
        )
    };
    match store.load().map_err(refused)? {
        None => Ok(Pending::default()),
        Some(text) => Pending::parse(&text).map_err(refused),
    }
}

/// Records `pending` in `store`, replacing what was recorded there.
///
/// # Errors
/// Returns why it cannot be written.
pub fn record(store: &dyn Store, pending: &Pending) -> Result<(), String> {
    store
        .save(&pending.text()?)
        .map_err(|e| format!("cannot record the withdrawal in {}: {e}", store.describe()))
}

/// Completes the withdrawal `pending`, recorded in `store`: `withdraw` for every former account but
/// those of `kept` — the accounts the registration names now in the same part, which an operator
/// may have switched back to — over every root, and the record removed once all are done. A
/// former account named now in the other part is withdrawn all the same: what it is granted in
/// that part is granted again after. A withdrawal that fails leaves the record for the next
/// install.
///
/// # Errors
/// Returns the first withdrawal that failed, or that the record cannot be removed.
pub fn complete(
    store: &dyn Store,
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
    store.clear().map_err(|e| {
        format!(
            "the withdrawal is done, and its record in {} is left: {e}",
            store.describe()
        )
    })
}

/// The record on Windows: [`VALUE`] of `HKEY_LOCAL_MACHINE\<parent>\<KEY>`.
#[cfg(windows)]
pub struct Registry {
    parent: String,
}

#[cfg(windows)]
impl Registry {
    /// The install's record, beneath `SOFTWARE\<PRODUCT_NAME>`.
    #[must_use]
    pub fn of_the_install() -> Registry {
        Registry::beneath(&format!(r"SOFTWARE\{}", super::manager::service_name()))
    }

    /// A record beneath `HKEY_LOCAL_MACHINE\<parent>`.
    #[must_use]
    pub fn beneath(parent: &str) -> Registry {
        Registry {
            parent: parent.to_string(),
        }
    }

    fn key(&self) -> String {
        format!(r"{}\{KEY}", self.parent)
    }

    /// Removes the key, once nothing is recorded in it.
    ///
    /// # Errors
    /// Returns why it cannot be removed.
    pub fn remove(&self) -> Result<(), String> {
        crate::win32::delete_key(&self.key())
            .map_err(|e| format!("cannot remove {}: {e}", self.describe()))
    }
}

/// Whether `key` is owned by `LocalSystem` or the Administrators and carries [`PRIVATE`].
#[cfg(windows)]
fn check_private(key: &crate::win32::Key) -> Result<(), String> {
    const OWNER: u32 = 0x1;
    const DACL: u32 = 0x4;
    let owner = key.security_text(OWNER).map_err(|e| e.to_string())?;
    if !matches!(
        owner.as_str(),
        "O:SY" | "O:BA" | "O:S-1-5-18" | "O:S-1-5-32-544"
    ) {
        return Err(format!("it is owned by {owner}"));
    }
    let dacl = key.security_text(DACL).map_err(|e| e.to_string())?;
    if !crate::win32::same_dacl(&dacl, PRIVATE).map_err(|e| e.to_string())? {
        return Err(format!("it carries {dacl}"));
    }
    Ok(())
}

#[cfg(windows)]
impl Store for Registry {
    fn load(&self) -> Result<Option<String>, String> {
        use crate::win32::{Key, KEY_QUERY_VALUE, KEY_READ_CONTROL};
        let Some(key) = Key::open(&self.key(), KEY_QUERY_VALUE | KEY_READ_CONTROL)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        check_private(&key)?;
        key.string(VALUE).map_err(|e| e.to_string())
    }

    fn save(&self, text: &str) -> Result<(), String> {
        use crate::win32::{Key, KEY_QUERY_VALUE, KEY_READ_CONTROL, KEY_SET_VALUE};
        let key = Key::create(
            &self.parent,
            KEY,
            PRIVATE,
            KEY_QUERY_VALUE | KEY_SET_VALUE | KEY_READ_CONTROL,
        )
        .map_err(|e| e.to_string())?;
        // A key that existed keeps its own descriptor, which may not be the one asked for.
        check_private(&key)?;
        key.set_string(VALUE, text).map_err(|e| e.to_string())
    }

    fn clear(&self) -> Result<(), String> {
        use crate::win32::{Key, KEY_SET_VALUE};
        match Key::open(&self.key(), KEY_SET_VALUE).map_err(|e| e.to_string())? {
            None => Ok(()),
            Some(key) => key.delete_value(VALUE).map_err(|e| e.to_string()),
        }
    }

    fn describe(&self) -> String {
        format!(r"HKLM\{}", self.key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record kept in memory, as a test keeps it.
    #[derive(Default)]
    struct Memory(std::cell::RefCell<Option<String>>);

    impl Store for Memory {
        fn load(&self) -> Result<Option<String>, String> {
            Ok(self.0.borrow().clone())
        }
        fn save(&self, text: &str) -> Result<(), String> {
            *self.0.borrow_mut() = Some(text.to_string());
            Ok(())
        }
        fn clear(&self) -> Result<(), String> {
            *self.0.borrow_mut() = None;
            Ok(())
        }
        fn describe(&self) -> String {
            "memory".to_string()
        }
    }

    /// A withdrawal that failed — an install interrupted after the SCM switched — is still
    /// recorded, and the next install, which finds no former account of its own, completes it from
    /// the record and removes it; one that succeeds leaves no record (ADR-0059 clauses 1 and 6).
    /// Verifies: ADR-0059
    #[test]
    fn an_interrupted_withdrawal_is_completed_by_the_next_install() {
        let store = Memory::default();
        let (root, config) = (Path::new("/root"), Path::new("/etc/supervisor.toml"));
        assert_eq!(read(&store), Ok(Pending::default()), "nothing recorded");

        // The first install: two former accounts, recorded before the switch.
        let mut due = read(&store).expect("read");
        due.add(
            [
                (Part::Process, "S-1-5-19".to_string()),
                (Part::Client, "S-1-5-80-1".to_string()),
            ],
            &[root, config],
        );
        record(&store, &due).expect("recorded");
        let mut asked = Vec::new();
        let interrupted = complete(&store, &due, &[], |sid, roots| {
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
        assert!(
            store.0.borrow().is_some(),
            "a withdrawal that failed stays recorded"
        );

        // The next install names no former account; it finds the record, adds what it walks, and
        // completes it — but not for an account the registration names again in the same part,
        // while one named now in the other part is withdrawn all the same.
        let mut due = read(&store).expect("read");
        assert_eq!(
            due.formers,
            [
                (Part::Process, "S-1-5-19".to_string()),
                (Part::Client, "S-1-5-80-1".to_string())
            ]
        );
        due.add(Vec::new(), &[root, Path::new("/state")]);
        assert_eq!(due.roots.len(), 3, "each root once");
        record(&store, &due).expect("recorded");
        let mut asked = Vec::new();
        complete(
            &store,
            &due,
            &[(Part::Process, "S-1-5-19"), (Part::Process, "S-1-5-80-1")],
            |sid, roots| {
                asked.push((sid.to_string(), roots.len()));
                Ok(())
            },
        )
        .expect("completed");
        assert_eq!(asked, [("S-1-5-80-1".to_string(), 3)]);
        assert_eq!(
            *store.0.borrow(),
            None,
            "a completed withdrawal leaves no record"
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

        let store = Memory::default();
        store.save("nonsense").expect("saved");
        let refused = read(&store).expect_err("refused");
        assert!(refused.contains("Nothing was changed"), "{refused}");
    }

    /// On Windows the record goes into the registry and comes back as it was written, beneath a
    /// key the install creates carrying a DACL that grants `LocalSystem` and the Administrators
    /// alone — nobody else may write it — and a key that carries any other DACL is refused. A key
    /// of the test's own stands in for the install's.
    /// Verifies: ADR-0059
    #[cfg(windows)]
    #[test]
    fn on_windows_the_record_round_trips_through_a_key_only_the_administrators_write() {
        let parent = format!(
            r"SOFTWARE\{}-test-{}",
            super::super::manager::service_name(),
            std::process::id()
        );
        let store = Registry::beneath(&parent);
        assert_eq!(read(&store), Ok(Pending::default()), "nothing recorded");
        let mut pending = Pending::default();
        pending.add(
            [(Part::Process, "S-1-5-19".to_string())],
            &[Path::new(r"C:\ProgramData\x y")],
        );
        record(&store, &pending).expect("recorded");
        assert_eq!(read(&store), Ok(pending.clone()), "read back");

        let key = crate::win32::Key::open(&store.key(), crate::win32::KEY_READ_CONTROL)
            .expect("opened")
            .expect("there");
        let dacl = key.security_text(0x4).expect("its DACL");
        assert!(
            crate::win32::same_dacl(&dacl, "D:P(A;;KA;;;SY)(A;;KA;;;BA)").expect("compared"),
            "{dacl}"
        );
        drop(key);

        complete(&store, &pending, &[], |_, _| Ok(())).expect("completed");
        assert_eq!(read(&store), Ok(Pending::default()), "cleared");
        store.remove().expect("key removed");
        crate::win32::delete_key(&parent).expect("parent removed");

        // A key someone else made in its place, inheriting what its parent passes on, is refused.
        let planted = crate::win32::Key::create(
            &parent,
            KEY,
            "D:(A;;KA;;;SY)(A;;KA;;;BA)(A;;KA;;;BU)",
            crate::win32::KEY_SET_VALUE,
        )
        .expect("planted");
        planted
            .set_string(VALUE, &pending.text().expect("text"))
            .expect("written");
        drop(planted);
        let refused = read(&store).expect_err("refused");
        assert!(refused.contains("it carries"), "{refused}");
        assert!(record(&store, &pending).is_err(), "and not written to");
        store.remove().expect("key removed");
        crate::win32::delete_key(&parent).expect("parent removed");
    }
}
