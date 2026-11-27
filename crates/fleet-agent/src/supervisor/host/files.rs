//! The roots of a Plugin and the file operations the core carries out in them
//! (CLT-wrappers-act-through-the-core §3 and §4).
//!
//! A Plugin names a file by its root and a relative path. Each operation opens the root and walks
//! the path one held directory at a time through the no-follow adapter
//! (CLT-accounts-service-runs-under §15), so a link anywhere on the way fails the call instead of leading out of the root. A refused call
//! fails alone: its reason goes back to the Plugin, which keeps running. No reason names a host
//! path, since a Plugin never learns where its roots lie.

use std::io;
use std::path::{Path, PathBuf};

use fleet_util::nofollow::{HeldDir, Kind};

use crate::storage::Shared;
use crate::supervisor::port::{EntryKind, Root, RootPath, Value};
use crate::supervisor::ports::{InstallTarget, ProcessAccountGroup, SupervisorContext};

/// The Supervisor's directory the Managed Process writes into, the [`Root::Data`] root.
pub const DATA_DIR: &str = "data";

/// The Plugin's own directory in the Supervisor's, the [`Root::State`] root, the Client's alone.
pub const STATE_DIR: &str = "state";

/// The most a Plugin reads of one file.
pub const READ_LIMIT: u64 = 16 << 20;

/// The Supervisor's own directories, as the roots of CLT-wrappers-act-through-the-core §3 name
/// them.
#[derive(Debug, Clone)]
pub(super) struct Roots {
    pub(super) supervisor_dir: PathBuf,
    pub(super) config: PathBuf,
    pub(super) program: PathBuf,
    pub(super) data: PathBuf,
    pub(super) state: PathBuf,
    /// The process account's group, which the `data` root is shared with
    /// (CLT-accounts-service-runs-under §21).
    pub(super) group: Option<ProcessAccountGroup>,
}

impl Roots {
    pub(super) fn of(ctx: &SupervisorContext) -> Self {
        let program = match &ctx.install {
            InstallTarget::Binary { path, .. } => path
                .parent()
                .map_or_else(|| ctx.supervisor_dir.clone(), Path::to_path_buf),
            InstallTarget::Tree { root, .. } => root.join(crate::config::TREE_DIR),
        };
        Roots {
            supervisor_dir: ctx.supervisor_dir.clone(),
            config: ctx.config_dir.clone(),
            program,
            data: ctx.supervisor_dir.join(DATA_DIR),
            state: ctx.supervisor_dir.join(STATE_DIR),
            group: ctx.process_account_group,
        }
    }

    /// The same roots with `program` elsewhere: where a preflight finds the staged program.
    pub(super) fn with_program(&self, program: PathBuf) -> Self {
        Roots {
            program,
            ..self.clone()
        }
    }

    fn root(&self, root: Root) -> &Path {
        match root {
            Root::Config => &self.config,
            Root::Program => &self.program,
            Root::Data => &self.data,
            Root::State => &self.state,
        }
    }

    /// The absolute path of `file`, refused where it would leave its root
    /// (CLT-wrappers-act-through-the-core §3). Lexical only: what is handed to a program as an
    /// argument is resolved by that program.
    pub(super) fn path(&self, file: &RootPath) -> Result<PathBuf, String> {
        let names = components(file)?;
        Ok(names
            .iter()
            .fold(self.root(file.root).to_path_buf(), |path, name| {
                path.join(name)
            }))
    }

    /// The placeholders naming the Supervisor's own directories, expanded in an operator's text
    /// (CLT-wrappers-act-through-the-core §31); anything else in it stays as written.
    pub(super) fn expand(&self, text: &str) -> String {
        text.replace("${supervisor_dir}", &self.supervisor_dir.to_string_lossy())
            .replace("${config_dir}", &self.config.to_string_lossy())
    }

    pub(super) fn value(&self, value: &Value) -> Result<String, String> {
        match value {
            Value::Text(text) => Ok(text.clone()),
            Value::Template(text) => Ok(self.expand(text)),
            Value::Rooted(file) => Ok(self.path(file)?.to_string_lossy().into_owned()),
        }
    }

    /// The root itself, opened and held. The `data` root is created first, shared with the
    /// process account's group, and the `state` root as the Client's alone
    /// (CLT-accounts-service-runs-under §21).
    fn held_root(&self, root: Root) -> io::Result<HeldDir> {
        match root {
            Root::Config | Root::Program => HeldDir::open(self.root(root)),
            Root::Data => {
                crate::storage::create_shared_dir(
                    &self.supervisor_dir,
                    &self.data,
                    self.group,
                    Shared::Written,
                )?;
                HeldDir::open(&self.data)
            }
            Root::State => HeldDir::create(&self.state, 0o700),
        }
    }

    /// The directory `names` leads to beneath `root`, each one held while the next is opened.
    fn walk(&self, root: Root, names: &[String]) -> io::Result<HeldDir> {
        let mut held = self.held_root(root)?;
        for name in names {
            held = held.open_dir(name)?;
        }
        Ok(held)
    }

    /// The held directory holding `file`, and the file's name in it.
    fn parent(&self, file: &RootPath) -> Result<(HeldDir, String), String> {
        let mut names = components(file)?;
        let Some(name) = names.pop() else {
            return Err(format!("{} names its root, not a file in it", shown(file)));
        };
        let held = self
            .walk(file.root, &names)
            .map_err(|e| failed("reach", file, &e))?;
        Ok((held, name))
    }

    pub(super) fn read(&self, file: &RootPath) -> Result<Vec<u8>, String> {
        let (held, name) = self.parent(file)?;
        held.read(&name, READ_LIMIT)
            .map(|content| content.to_vec())
            .map_err(|e| failed("read", file, &e))
    }

    pub(super) fn write(&self, file: &RootPath, contents: &[u8]) -> Result<(), String> {
        writable(file)?;
        let (held, name) = self.parent(file)?;
        held.write(&name, contents, file_mode(file.root))
            .map_err(|e| failed("write", file, &e))
    }

    /// Renames within one directory: a rename across directories would need two held parents,
    /// which no kind has asked for.
    pub(super) fn rename(&self, from: &RootPath, to: &RootPath) -> Result<(), String> {
        writable(from)?;
        let mut from_names = components(from)?;
        let mut to_names = components(to)?;
        let (Some(from_name), Some(to_name)) = (from_names.pop(), to_names.pop()) else {
            return Err("a rename names two files, not a root".to_string());
        };
        if from.root != to.root || from_names != to_names {
            return Err(format!(
                "{} and {} are not in one directory: a rename stays in its directory",
                shown(from),
                shown(to)
            ));
        }
        let held = self
            .walk(from.root, &from_names)
            .map_err(|e| failed("reach", from, &e))?;
        held.rename(&from_name, &to_name)
            .map_err(|e| failed("rename", from, &e))
    }

    /// Removes a file, or an empty directory.
    pub(super) fn remove(&self, file: &RootPath) -> Result<(), String> {
        writable(file)?;
        let (held, name) = self.parent(file)?;
        held.remove(&name)
            .or_else(|first| held.remove_dir(&name).map_err(|_| first))
            .map_err(|e| failed("remove", file, &e))
    }

    pub(super) fn list(&self, dir: &RootPath) -> Result<Vec<(String, EntryKind)>, String> {
        let names = components(dir)?;
        let held = self
            .walk(dir.root, &names)
            .map_err(|e| failed("reach", dir, &e))?;
        let entries = held.list().map_err(|e| failed("list", dir, &e))?;
        Ok(entries
            .into_iter()
            .map(|(name, kind)| {
                let kind = match kind {
                    Kind::File => EntryKind::Regular,
                    Kind::Dir => EntryKind::Directory,
                    Kind::Link => EntryKind::Link,
                    Kind::Other => EntryKind::Other,
                };
                (name, kind)
            })
            .collect())
    }

    /// Creates the directory and what is missing above it, never through a link.
    /// In `data`, where a process account is registered, it is made as the Client makes a kind's
    /// data directories: shared with that account's group, its mode set whatever the umask, and
    /// an existing one corrected (CLT-accounts-service-runs-under §21). Elsewhere, and where none
    /// is, it is the Client's alone, as the Managed Process then runs under the Client's account.
    pub(super) fn create_dir(&self, dir: &RootPath) -> Result<(), String> {
        writable(dir)?;
        let names = components(dir)?;
        let mut held = self
            .held_root(dir.root)
            .map_err(|e| failed("reach", dir, &e))?;
        if let (Root::Data, Some(group), false) = (dir.root, self.group, names.is_empty()) {
            return crate::storage::create_shared_dir(
                &self.data,
                &self.path(dir)?,
                Some(group),
                Shared::Written,
            )
            .map_err(|e| failed("create", dir, &e));
        }
        for name in &names {
            held = held
                .dir(name, PRIVATE_DIR_MODE)
                .map_err(|e| failed("create", dir, &e))?;
        }
        Ok(())
    }
}

/// The plain names of `file`'s path, `/`-separated, refused where a name is empty, `.` or `..`,
/// where the path is absolute, and on Windows where a name holds a separator or a drive
/// (CLT-wrappers-act-through-the-core §3). The empty path names the root itself.
pub(super) fn components(file: &RootPath) -> Result<Vec<String>, String> {
    if file.path.is_empty() {
        return Ok(Vec::new());
    }
    file.path
        .split('/')
        .map(|name| {
            let plain =
                !matches!(name, "" | "." | "..") && !(cfg!(windows) && name.contains(['\\', ':']));
            if plain {
                Ok(name.to_string())
            } else {
                Err(leaves(file))
            }
        })
        .collect()
}

fn leaves(file: &RootPath) -> String {
    format!(
        "the path {:?} leaves its root: a Plugin names a file by a relative path of plain names",
        file.path
    )
}

/// A write is refused outside `data` and `state` (CLT-wrappers-act-through-the-core §3).
fn writable(file: &RootPath) -> Result<(), String> {
    if file.root.writable() {
        Ok(())
    } else {
        Err(format!(
            "{} is read-only to a Plugin: it writes in `data` and `state` alone",
            shown(file)
        ))
    }
}

/// The mode of a file a Plugin writes: the group's too in `data`, where the directory gives it
/// the process account's group; the Client's alone in `state`.
fn file_mode(root: Root) -> u32 {
    match root {
        Root::Data => 0o660,
        _ => 0o600,
    }
}

/// The mode of a directory a Plugin creates that is not shared: the Client's alone.
const PRIVATE_DIR_MODE: u32 = 0o700;

/// `file` as a Plugin named it.
fn shown(file: &RootPath) -> String {
    let root = match file.root {
        Root::Config => "config",
        Root::Program => "program",
        Root::Data => "data",
        Root::State => "state",
    };
    format!("{root}:{}", file.path)
}

/// Why `what` failed on `file`, without the host path an I/O error carries.
fn failed(what: &str, file: &RootPath, e: &io::Error) -> String {
    let reason = if fleet_util::nofollow::is_refused_link(e) {
        "a link stands on the way, which no host function follows".to_string()
    } else {
        e.kind().to_string()
    };
    format!("cannot {what} {}: {reason}", shown(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(root: Root, path: &str) -> RootPath {
        RootPath {
            root,
            path: path.to_string(),
        }
    }

    fn fixed() -> Roots {
        Roots {
            supervisor_dir: PathBuf::from("/s/agent"),
            config: PathBuf::from("/s/agent/config"),
            program: PathBuf::from("/s/agent/program"),
            data: PathBuf::from("/s/agent/data"),
            state: PathBuf::from("/s/agent/state"),
            group: None,
        }
    }

    /// A Supervisor's roots in a temporary directory, its `config/` and `program/` made.
    fn made(dir: &Path) -> Roots {
        let supervisor_dir = dir.join("agent");
        std::fs::create_dir_all(supervisor_dir.join("config")).expect("config");
        std::fs::create_dir_all(supervisor_dir.join("program")).expect("program");
        Roots {
            config: supervisor_dir.join("config"),
            program: supervisor_dir.join("program"),
            data: supervisor_dir.join(DATA_DIR),
            state: supervisor_dir.join(STATE_DIR),
            supervisor_dir,
            group: None,
        }
    }

    /// A file a Plugin names resolves inside its root, and a path that would leave the root fails.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[test]
    fn a_plugin_path_stays_inside_its_root() {
        let roots = fixed();
        assert_eq!(
            roots.path(&file(Root::Config, "a/b.yaml")),
            Ok(PathBuf::from("/s/agent/config/a/b.yaml"))
        );
        assert_eq!(
            roots.path(&file(Root::Program, "")),
            Ok(PathBuf::from("/s/agent/program"))
        );
        for escape in ["../x", "a/../../x", "/etc/passwd", "./x", "a//b", "a/", "."] {
            assert!(
                roots.path(&file(Root::Config, escape)).is_err(),
                "{escape} is refused"
            );
        }
        // A backslash separates on Windows alone; elsewhere it is part of a plain name, as an
        // operator's file in `config/` may carry one.
        assert_eq!(
            roots.path(&file(Root::Config, "a\\..\\x")).is_err(),
            cfg!(windows)
        );
    }

    /// An operator's template expands the two placeholders and leaves any other `${…}` as written.
    #[test]
    fn a_template_expands_the_supervisors_own_directories() {
        let roots = fixed();
        assert_eq!(
            roots.value(&Value::Template("-c ${config_dir}/x ${env:Y}".to_string())),
            Ok("-c /s/agent/config/x ${env:Y}".to_string())
        );
        assert_eq!(
            roots.value(&Value::Text("${config_dir}".to_string())),
            Ok("${config_dir}".to_string()),
            "a text is passed as written"
        );
    }

    /// A Plugin writes, lists, renames and removes in `state` and `data`, and reads back what it
    /// wrote.
    #[test]
    fn a_plugin_keeps_files_in_state_and_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = made(dir.path());
        for root in [Root::State, Root::Data] {
            roots.create_dir(&file(root, "a/b")).expect("create");
            roots.write(&file(root, "a/b/f"), b"one").expect("write");
            assert_eq!(roots.read(&file(root, "a/b/f")), Ok(b"one".to_vec()));
            roots
                .rename(&file(root, "a/b/f"), &file(root, "a/b/g"))
                .expect("rename");
            assert_eq!(
                roots.list(&file(root, "a/b")),
                Ok(vec![("g".to_string(), EntryKind::Regular)])
            );
            assert_eq!(
                roots.list(&file(root, "")),
                Ok(vec![("a".to_string(), EntryKind::Directory)])
            );
            roots.remove(&file(root, "a/b/g")).expect("remove a file");
            roots
                .remove(&file(root, "a/b"))
                .expect("remove an empty dir");
            assert!(
                roots.rename(&file(root, "a/x"), &file(root, "y")).is_err(),
                "a rename stays in its directory"
            );
        }
    }

    /// The `state` root and what a Plugin writes in it are the Client's alone, and the `data`
    /// root is shared as a kind's data directory (CLT-accounts-service-runs-under §21).
    /// Verifies: CLT-wrappers-act-through-the-core
    #[cfg(unix)]
    #[test]
    fn state_is_the_clients_alone_and_data_is_shared() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let mut roots = made(dir.path());
        // The test's own primary group stands in for the process account's: the Client's account
        // belongs to it, as the install requires of the real one.
        // SAFETY: getgid(2) and getuid(2) cannot fail and touch no memory.
        let (gid, uid) = unsafe { (libc::getgid(), libc::getuid()) };
        roots.group = Some(ProcessAccountGroup(gid));
        roots
            .write(&file(Root::State, "s"), b"secret")
            .expect("state");
        roots
            .write(&file(Root::Data, "d"), b"shared")
            .expect("data");
        let meta = |p: &Path| std::fs::symlink_metadata(p).expect("meta");
        let state = meta(&roots.state);
        assert_eq!((state.uid(), state.mode() & 0o7777), (uid, 0o700));
        assert_eq!(meta(&roots.state.join("s")).mode() & 0o777, 0o600);
        let data = meta(&roots.data);
        assert_eq!(
            (data.uid(), data.gid(), data.mode() & 0o7777),
            (uid, gid, 0o2770)
        );
        let written = meta(&roots.data.join("d"));
        assert_eq!(written.gid(), gid, "the data file is the group's");
        // A directory a Plugin creates in `data` is shared as the root is, whatever the umask
        // narrows a new directory to, and one already there narrower is corrected.
        roots
            .create_dir(&file(Root::Data, "spool/in"))
            .expect("create");
        std::fs::create_dir(roots.data.join("narrow")).expect("narrow");
        std::fs::set_permissions(
            roots.data.join("narrow"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .expect("mode");
        roots
            .create_dir(&file(Root::Data, "narrow"))
            .expect("corrected");
        for made in ["spool/in", "narrow"] {
            let made = meta(&roots.data.join(made));
            assert_eq!((made.gid(), made.mode() & 0o7777), (gid, 0o2770));
        }
    }

    /// A path with `..`, an absolute path or a link out of its root fails that call alone; the
    /// next call works.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[cfg(unix)]
    #[test]
    fn a_path_outside_its_root_fails_only_that_call() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = made(dir.path());
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).expect("outside");
        std::fs::write(outside.join("x"), b"theirs").expect("x");
        roots
            .write(&file(Root::State, "mine"), b"m")
            .expect("state");
        std::os::unix::fs::symlink(&outside, roots.state.join("away")).expect("dir link");
        std::os::unix::fs::symlink(outside.join("x"), roots.state.join("x")).expect("file link");
        for escape in ["../outside/x", "/etc/passwd", "away/x", "x"] {
            let refused = roots.read(&file(Root::State, escape));
            assert!(refused.is_err(), "{escape} is refused");
            assert!(
                !refused
                    .unwrap_err()
                    .contains(&dir.path().display().to_string()),
                "a reason names no host path"
            );
        }
        assert!(roots.write(&file(Root::State, "away/y"), b"y").is_err());
        assert!(!outside.join("y").exists(), "nothing landed outside");
        assert!(roots.list(&file(Root::State, "away")).is_err());
        assert_eq!(roots.read(&file(Root::State, "mine")), Ok(b"m".to_vec()));
    }

    /// A Plugin reads but never writes in `config` and `program`.
    /// Verifies: CLT-wrappers-act-through-the-core
    #[test]
    fn a_plugin_cannot_write_its_config_or_program() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = made(dir.path());
        std::fs::write(roots.config.join("fleet"), b"cfg").expect("entry");
        assert_eq!(
            roots.read(&file(Root::Config, "fleet")),
            Ok(b"cfg".to_vec())
        );
        for root in [Root::Config, Root::Program] {
            assert!(roots.write(&file(root, "f"), b"x").is_err());
            assert!(roots.create_dir(&file(root, "d")).is_err());
            assert!(roots.remove(&file(root, "fleet")).is_err());
            assert!(roots
                .rename(&file(root, "fleet"), &file(root, "other"))
                .is_err());
        }
        assert!(roots.config.join("fleet").exists());
    }
}
