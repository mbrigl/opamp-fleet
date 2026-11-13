//! File operations in a directory the Client's service account can write, which never follow a
//! link that account could have planted (ADR-0021).
//!
//! `service install` and `service config` run as root or as an Administrator, and they write into
//! the state directory and the executable layout, which belong to the service's account. A plain
//! `fs::write` there follows a symbolic link, a junction or a Windows symbolic link the account put
//! at the name, and the privileged write lands wherever the link points. So every operation here
//! works on a [`HeldDir`]: a directory opened without following a link at its own name, and held
//! while the operation runs.
//!
//! - **Unix.** Each name is opened, created, renamed and removed relative to the held descriptor
//!   (`openat`, `mkdirat`, `renameat`, `unlinkat`) with `O_NOFOLLOW`. A file is written whole to a
//!   new temporary name, created with `O_CREAT | O_EXCL`, its mode set through the descriptor,
//!   synced, and renamed over the name: a rename replaces a link at the name and never follows it.
//! - **Windows.** The directory is opened as what it is (`FILE_FLAG_OPEN_REPARSE_POINT`) and a
//!   reparse point is refused. The handle is held without `FILE_SHARE_DELETE`, so the directory
//!   cannot be renamed or replaced while it is held, and a directory above it cannot be renamed
//!   while something under it is open. A file is read only when it is no reparse point, and
//!   written to a new temporary name (`CREATE_NEW`, which never opens an existing name) and renamed
//!   over the name.
//!
//! The directory a [`HeldDir`] is opened at is resolved as a path up to its parent: those
//! components are the operator's choice, and only the last one lies in a directory the account
//! writes. A directory below it is reached through [`HeldDir::dir`], one held name at a time.
//!
//! Every file is read one way: opened as a regular file, through no link at its name, and read
//! within a bound into a buffer sized before it reads and wiped when it is dropped (ADR-0014), so
//! a privileged step never reads more than it expects, and no grown copy of a secret is left
//! behind. A path a privileged step did not choose itself is resolved by [`read_privileged`].

use std::io;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

/// The most a file of the Client's own is read to: a certificate, a key, a record of the state
/// directory, `supervisor.toml`. A bundle of many certificates is far below it.
pub const MAX_READ_BYTES: u64 = 1 << 20;

/// A directory opened without following a link at its name, held while it is used.
pub struct HeldDir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    _handle: std::fs::File,
}

impl HeldDir {
    /// Opens the directory at `path`, refusing a link at its last component.
    ///
    /// # Errors
    /// Returns an error when `path` names no directory, or names a link.
    pub fn open(path: &Path) -> io::Result<Self> {
        let (parent, name) = split(path)?;
        sys::open_in(parent, name, false, 0)
    }

    /// Opens a root the operator named — the data root or the layout root — and holds it.
    ///
    /// On Unix the path is followed: the root's name lies in a directory only root writes
    /// (`/var/lib`, `/opt`, `/Library/Application Support`). On Windows it does not: the account
    /// holds Modify, delete included, on the root itself, and Users may create folders in
    /// `%ProgramData%`, so a root that is a reparse point is refused, and the held handle keeps it
    /// from being renamed or replaced while it is held.
    ///
    /// # Errors
    /// Returns an error when `path` names no directory, or on Windows a reparse point.
    pub fn open_root(path: &Path) -> io::Result<Self> {
        sys::open_root(path)
    }

    /// Creates the directory at `path` where it is missing — its parents as plain directories,
    /// itself with `mode` — and opens it as [`open`](Self::open) does. On Unix an existing
    /// directory is narrowed to `mode` too, through the held descriptor.
    ///
    /// # Errors
    /// Returns an error when a directory cannot be created, or `path` names a link.
    pub fn create(path: &Path, mode: u32) -> io::Result<Self> {
        let (parent, name) = split(path)?;
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
        sys::open_in(parent, name, true, mode)
    }

    /// The path this directory was opened at.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory `name` in this one, created with `mode` where it is missing, never through a
    /// link.
    ///
    /// # Errors
    /// Returns an error when it cannot be created or opened, or is a link.
    pub fn dir(&self, name: &str, mode: u32) -> io::Result<Self> {
        check_name(name)?;
        sys::dir(self, name, mode)
    }

    /// The bytes of the regular file `name`, refusing a link, read as `read_opened` reads.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read, is no regular file, or holds more than
    /// `limit` bytes.
    pub fn read(&self, name: &str, limit: u64) -> io::Result<Zeroizing<Vec<u8>>> {
        read_opened(self.open_file(name)?, limit)
    }

    /// The regular file `name`, opened for reading, refusing a link: what its metadata says is
    /// what is read.
    ///
    /// # Errors
    /// Returns an error when the file cannot be opened, or is no regular file.
    pub fn open_file(&self, name: &str) -> io::Result<std::fs::File> {
        check_name(name)?;
        sys::open_file(self, name)
    }

    /// The existing regular file `name`, opened for writing in place, refusing a link.
    ///
    /// # Errors
    /// Returns an error when the file cannot be opened, or is no regular file.
    pub fn open_for_writing(&self, name: &str) -> io::Result<std::fs::File> {
        check_name(name)?;
        sys::open_for_writing(self, name)
    }

    /// Writes `contents` as the file `name`, with `mode` on Unix: whole, to a new temporary name
    /// beside it, renamed over `name`. A link at `name` is replaced, never followed.
    ///
    /// # Errors
    /// Returns an error when the file cannot be written or renamed into place.
    pub fn write(&self, name: &str, contents: &[u8], mode: u32) -> io::Result<()> {
        self.write_owned(name, contents, mode, None)
    }

    /// Writes `contents` as [`write`](Self::write) does, and on Unix gives the new file `owner`
    /// (user and group) through its descriptor before it is renamed over `name`, never by path
    /// after it. On Windows `owner` is not used: a new file takes what the directory passes on.
    ///
    /// # Errors
    /// Returns an error when the file cannot be written, given its owner, or renamed into place.
    pub fn write_owned(
        &self,
        name: &str,
        contents: &[u8],
        mode: u32,
        owner: Option<(u32, u32)>,
    ) -> io::Result<()> {
        check_name(name)?;
        // `~` is in no name a config-map entry is stored under (`storage::entry_file_name`), so
        // the temporary name never collides with a file this directory is meant to hold.
        let temporary = format!("{name}~");
        sys::write(self, name, &temporary, contents, mode, owner)
    }

    /// Creates the file `name` with `contents`, with `mode` on Unix, and never replaces what
    /// stands at the name: a file, or a link to anywhere, makes it fail with `AlreadyExists`.
    ///
    /// # Errors
    /// Returns an error when something stands at `name`, or the file cannot be written.
    pub fn create_new(&self, name: &str, contents: &[u8], mode: u32) -> io::Result<()> {
        check_name(name)?;
        sys::create_new(self, name, contents, mode)
    }

    /// Makes what was created, renamed or removed in this directory durable. On Windows NTFS
    /// journals the directory's entries itself, and a directory handle opened for reading cannot be
    /// flushed, so nothing is done.
    ///
    /// # Errors
    /// Returns an error when the sync fails.
    pub fn sync(&self) -> io::Result<()> {
        sys::sync(self)
    }

    /// Renames `from` to `to` within this directory. A link is renamed as a link.
    ///
    /// # Errors
    /// Returns an error when the rename fails.
    pub fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        check_name(from)?;
        check_name(to)?;
        sys::rename(self, from, to)
    }

    /// Removes the file `name`; a link is removed as a link.
    ///
    /// # Errors
    /// Returns an error when the removal fails, a missing file included.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        sys::remove(self, name)
    }
}

#[cfg(unix)]
impl std::os::fd::AsRawFd for HeldDir {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}

/// Reads the regular file at `path`, refusing a link at it or at its directory's name, as
/// `read_opened` reads.
///
/// # Errors
/// Returns an error when the file cannot be read, a link stands at either name, or it holds more
/// than `limit` bytes.
pub fn read(path: &Path, limit: u64) -> io::Result<Zeroizing<Vec<u8>>> {
    let (dir, name) = held_parent(path)?;
    dir.read(name, limit)
}

/// What the opened `file` holds, where it is a regular file of at most `limit` bytes: the one read
/// of this module. The buffer is sized before it reads, so it is never grown and a copy left
/// behind, and it is wiped when it is dropped (ADR-0014). A file that grew past the bound since
/// is refused.
///
/// # Errors
/// Returns an error when the file is no regular file, holds more than `limit` bytes, or cannot be
/// read. It leaves naming the file to the caller.
pub(crate) fn read_opened(file: std::fs::File, limit: u64) -> io::Result<Zeroizing<Vec<u8>>> {
    use std::io::Read as _;
    let over = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than the {limit} bytes it may hold at most"),
        )
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    if meta.len() > limit {
        return Err(over());
    }
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        usize::try_from(meta.len()).unwrap_or_default(),
    ));
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(over());
    }
    Ok(bytes)
}

/// Whose word a path a privileged step reads is, which decides what a link on it may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    /// The administrator gave it: typed at a prompt, or named by a flag. A link on the way is
    /// followed outside the service's account's directories, as the administrator's own.
    ByAdministrator,
    /// The step offers it from a file the service's account may write, such as `[tls] cert_file`
    /// in `supervisor.toml`: a link at the file's own name is refused, wherever the file lies.
    ByAccountFile,
}

/// The most links a privileged read follows on an administrator's path, as Linux's `ELOOP` bound.
const MAX_LINKS: usize = 40;

/// The file at `path`, read by a privileged step: a regular file of at most `limit` bytes, opened
/// without blocking, so a device or a FIFO is never read, and read as `read_opened` reads.
///
/// The roots — the data root and the layout root — and the state directory belong to the
/// service's account. The path is resolved one component at a time, `..` and every link on the
/// way included, and each directory it passes is judged:
///
/// - **A root or the state directory** that the walk reaches must hold the file directly: the
///   name after it is opened in that directory, held, and a link at the name is refused. A root
///   is held as [`HeldDir::open_root`] holds it (ADR-0021 clause 13), the state directory without
///   following a link at its own name (clauses 11 and 12). The state directory directly in a root
///   is reached by that name too, and counts as the account's where it is missing.
/// - **A path that goes on beneath one of them** is refused: a directory there could be a link
///   the account put in place.
/// - **Anywhere else** the path is the administrator's, and a link on it is followed, at most
///   [`MAX_LINKS`]. The file is opened by its name in the directory the walk ends in, held.
///
/// A path [`Named::ByAccountFile`] is held to one rule more: a link at the file's own name is
/// refused wherever it lies, and the file is not read.
///
/// # Errors
/// Returns an error naming `path` when the file is refused or cannot be read.
pub fn read_privileged(
    path: &Path,
    roots: &[&Path],
    state_dir: Option<&Path>,
    named_by: Named,
    limit: u64,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let named = |why: String| io::Error::other(format!("cannot read {}: {why}", path.display()));
    // The roots as resolved; the state directory by its resolved parent and its own name, not
    // followed, so it counts as the account's even where it is missing or a link.
    let state = state_dir
        .and_then(|dir| dir.parent().zip(dir.file_name()))
        .and_then(|(parent, name)| Some(std::fs::canonicalize(parent).ok()?.join(name)));
    let owned: Vec<(PathBuf, &Path, bool)> = roots
        .iter()
        .filter_map(|root| Some((std::fs::canonicalize(root).ok()?, *root, true)))
        .chain(
            state
                .clone()
                .zip(state_dir)
                .map(|(form, dir)| (form, dir, false)),
        )
        .collect();
    let absolute = std::path::absolute(path).map_err(|e| named(e.to_string()))?;
    let mut pending: std::collections::VecDeque<Part> =
        absolute.components().map(Part::of).collect();
    let mut at = PathBuf::new();
    let mut links = 0;
    while let Some(next) = pending.pop_front() {
        match next {
            Part::Up => {
                at.pop();
            }
            Part::Here => {}
            Part::Name(name) => {
                let here = at.join(&name);
                let meta = std::fs::symlink_metadata(&here).map_err(|e| named(e.to_string()))?;
                if meta.file_type().is_symlink() {
                    if pending.is_empty() && named_by == Named::ByAccountFile {
                        return Err(named(refused_link(&here).to_string()));
                    }
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(named(format!("more than {MAX_LINKS} links on the way")));
                    }
                    let target = std::fs::read_link(&here).map_err(|e| named(e.to_string()))?;
                    if target.is_absolute() {
                        at = PathBuf::new();
                    }
                    for part in target.components().rev() {
                        pending.push_front(Part::of(part));
                    }
                    continue;
                }
                at = here;
            }
            // The root, and on Windows the drive.
            Part::Root(root) => at.push(root),
        }
        if pending.is_empty() {
            break;
        }
        // A directory the walk reached: one of the account's holds the file directly, or not at all.
        let real = std::fs::canonicalize(&at).map_err(|e| named(e.to_string()))?;
        if let Some((_, dir, root)) = owned.iter().find(|(form, _, _)| *form == real) {
            // The state directory directly in a root holds its own files, reached by its name.
            if let (2, Some(Part::Name(sub)), Some(Part::Name(name))) =
                (pending.len(), pending.front(), pending.back())
            {
                if let (Some(dir), true) = (state_dir, state.as_ref() == Some(&real.join(sub))) {
                    return read_in(HeldDir::open(dir), name, limit, &named);
                }
            }
            let (1, Some(Part::Name(name))) = (pending.len(), pending.front()) else {
                return Err(named(format!(
                    "it lies beneath {}, which the service's account owns, but not directly in it",
                    dir.display()
                )));
            };
            let held = if *root {
                HeldDir::open_root(dir)
            } else {
                HeldDir::open(dir)
            };
            return read_in(held, name, limit, &named);
        }
    }
    let (dir, name) = match (at.parent(), at.file_name()) {
        (Some(dir), Some(name)) => (dir, name),
        _ => return Err(named("it names no file".to_string())),
    };
    let held = if dir.parent().is_some() {
        HeldDir::open(dir)
    } else {
        HeldDir::open_root(dir)
    };
    read_in(held, name, limit, &named)
}

/// One component of a path being resolved, owned, so a link's target can be put in its place.
enum Part {
    Root(std::ffi::OsString),
    Here,
    Up,
    Name(std::ffi::OsString),
}

impl Part {
    fn of(component: std::path::Component<'_>) -> Self {
        use std::path::Component;
        match component {
            Component::Prefix(_) | Component::RootDir => Part::Root(component.as_os_str().into()),
            Component::CurDir => Part::Here,
            Component::ParentDir => Part::Up,
            Component::Normal(name) => Part::Name(name.into()),
        }
    }
}

/// The regular file `name` in `held`, read within `limit`, a link at the name refused.
fn read_in(
    held: io::Result<HeldDir>,
    name: &std::ffi::OsStr,
    limit: u64,
    named: &impl Fn(String) -> io::Error,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let name = name
        .to_str()
        .ok_or_else(|| named("its name is not UTF-8".to_string()))?;
    held.and_then(|held| held.read(name, limit))
        .map_err(|e| named(e.to_string()))
}

/// Writes `contents` to `path` as [`HeldDir::write`] does, refusing a link at its directory's name.
///
/// # Errors
/// Returns an error when the file cannot be written, or the directory is a link.
pub fn write(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let (dir, name) = held_parent(path)?;
    dir.write(name, contents, mode)
}

/// The directory holding `path`, held, and the name of `path` in it.
pub fn held_parent(path: &Path) -> io::Result<(HeldDir, &str)> {
    let (parent, name) = split(path)?;
    let name = name.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a UTF-8 name", path.display()),
        )
    })?;
    Ok((HeldDir::open(parent)?, name))
}

/// `path` as its parent directory and its last component, which must be a name. A path without a
/// parent is in the working directory.
fn split(path: &Path) -> io::Result<(&Path, &std::ffi::OsStr)> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no file or directory", path.display()),
        )
    })?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    Ok((parent, name))
}

/// A name within a held directory is one plain component: never a path, `.` or `..`.
fn check_name(name: &str) -> io::Result<()> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a plain file name"),
        )),
    }
}

/// The error for a link where a plain entry was required.
fn refused_link(path: &Path) -> io::Error {
    io::Error::other(format!(
        "{} is a link, which is refused: the service's account could have put it there",
        path.display()
    ))
}

#[cfg(unix)]
mod sys {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io::{self, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    use super::{refused_link, HeldDir};

    const DIRECTORY: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    fn openat(dir: RawFd, name: &CString, flags: libc::c_int, mode: u32) -> io::Result<OwnedFd> {
        // SAFETY: a descriptor or `AT_FDCWD`, a NUL-terminated name, and the mode `O_CREAT` reads.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags, mode as libc::c_uint) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// A link at the last component opened with `O_NOFOLLOW` reads as `ELOOP`.
    fn named(e: io::Error, path: &Path) -> io::Error {
        if e.raw_os_error() == Some(libc::ELOOP) {
            refused_link(path)
        } else {
            e
        }
    }

    fn mkdirat(dir: RawFd, name: &CString, mode: u32) -> io::Result<()> {
        // SAFETY: a descriptor or `AT_FDCWD` and a NUL-terminated name.
        if unsafe { libc::mkdirat(dir, name.as_ptr(), mode as libc::mode_t) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        // `mkdirat` never follows a link at the name: one there is `EEXIST`, and the open after it
        // refuses it.
        if e.kind() == io::ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(e)
        }
    }

    fn fchmod(fd: &OwnedFd, mode: u32) -> io::Result<()> {
        // SAFETY: a valid descriptor.
        if unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn open_root(path: &Path) -> io::Result<HeldDir> {
        let fd = openat(
            libc::AT_FDCWD,
            &c_name(path.as_os_str())?,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?;
        Ok(HeldDir {
            path: path.to_path_buf(),
            fd,
        })
    }

    pub(super) fn open_in(
        parent: &Path,
        name: &OsStr,
        create: bool,
        mode: u32,
    ) -> io::Result<HeldDir> {
        let path = parent.join(name);
        let parent_fd = openat(
            libc::AT_FDCWD,
            &c_name(parent.as_os_str())?,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )?;
        let name = c_name(name)?;
        if create {
            mkdirat(parent_fd.as_raw_fd(), &name, mode)?;
        }
        let fd = openat(parent_fd.as_raw_fd(), &name, DIRECTORY, 0).map_err(|e| named(e, &path))?;
        if create {
            fchmod(&fd, mode)?;
        }
        Ok(HeldDir { path, fd })
    }

    pub(super) fn dir(held: &HeldDir, name: &str, mode: u32) -> io::Result<HeldDir> {
        let path = held.path.join(name);
        let c = c_name(OsStr::new(name))?;
        mkdirat(held.fd.as_raw_fd(), &c, mode)?;
        let fd = openat(held.fd.as_raw_fd(), &c, DIRECTORY, 0).map_err(|e| named(e, &path))?;
        Ok(HeldDir { path, fd })
    }

    pub(super) fn open_file(held: &HeldDir, name: &str) -> io::Result<File> {
        open_regular(held, name, libc::O_RDONLY)
    }

    pub(super) fn open_for_writing(held: &HeldDir, name: &str) -> io::Result<File> {
        open_regular(held, name, libc::O_WRONLY)
    }

    fn open_regular(held: &HeldDir, name: &str, access: libc::c_int) -> io::Result<File> {
        let path = held.path.join(name);
        // `O_NONBLOCK`: a FIFO at the name must not stall a privileged run; a regular file ignores it.
        let fd = openat(
            held.fd.as_raw_fd(),
            &c_name(OsStr::new(name))?,
            access | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )
        .map_err(|e| named(e, &path))?;
        let file = File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(io::Error::other(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(file)
    }

    pub(super) fn write(
        held: &HeldDir,
        name: &str,
        temporary: &str,
        contents: &[u8],
        mode: u32,
        owner: Option<(u32, u32)>,
    ) -> io::Result<()> {
        let dir = held.fd.as_raw_fd();
        let temporary = c_name(OsStr::new(temporary))?;
        // A leftover from an interrupted run would make `O_EXCL` refuse; a link there goes as a link.
        match unlinkat(dir, &temporary) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let written = openat(
            dir,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
        .and_then(|fd| {
            // The umask may have narrowed the mode the open asked for.
            fchmod(&fd, mode)?;
            if let Some((uid, gid)) = owner {
                std::os::unix::fs::fchown(&fd, Some(uid), Some(gid))?;
            }
            let mut file = File::from(fd);
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| renameat(dir, &temporary, &c_name(OsStr::new(name))?));
        if written.is_err() {
            let _ = unlinkat(dir, &temporary);
        }
        written
    }

    pub(super) fn create_new(
        held: &HeldDir,
        name: &str,
        contents: &[u8],
        mode: u32,
    ) -> io::Result<()> {
        let dir = held.fd.as_raw_fd();
        let c = c_name(OsStr::new(name))?;
        // `O_EXCL` fails on any name that exists, a link included, and never follows one.
        let fd = openat(
            dir,
            &c,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )?;
        let written = fchmod(&fd, mode).and_then(|()| {
            let mut file = File::from(fd);
            file.write_all(contents)?;
            file.sync_all()
        });
        if written.is_err() {
            let _ = unlinkat(dir, &c);
        }
        written
    }

    fn unlinkat(dir: RawFd, name: &CString) -> io::Result<()> {
        // SAFETY: a valid descriptor and a NUL-terminated name; without `AT_REMOVEDIR` a link is
        // removed as a link.
        if unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn renameat(dir: RawFd, from: &CString, to: &CString) -> io::Result<()> {
        // SAFETY: a valid descriptor and two NUL-terminated names; neither is followed.
        if unsafe { libc::renameat(dir, from.as_ptr(), dir, to.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn rename(held: &HeldDir, from: &str, to: &str) -> io::Result<()> {
        renameat(
            held.fd.as_raw_fd(),
            &c_name(OsStr::new(from))?,
            &c_name(OsStr::new(to))?,
        )
    }

    pub(super) fn remove(held: &HeldDir, name: &str) -> io::Result<()> {
        unlinkat(held.fd.as_raw_fd(), &c_name(OsStr::new(name))?)
    }

    pub(super) fn sync(held: &HeldDir) -> io::Result<()> {
        // SAFETY: a valid descriptor.
        if unsafe { libc::fsync(held.fd.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write as _};
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    use super::{refused_link, HeldDir};

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    /// The directory at `path` itself, held against a rename or a delete, or refused when it is a
    /// reparse point — a junction, a symbolic link or a mount point.
    fn hold(path: &Path) -> io::Result<HeldDir> {
        let handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let meta = handle.metadata()?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(refused_link(path));
        }
        if !meta.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a directory",
                path.display()
            )));
        }
        Ok(HeldDir {
            path: path.to_path_buf(),
            _handle: handle,
        })
    }

    pub(super) fn open_root(path: &Path) -> io::Result<HeldDir> {
        hold(path)
    }

    fn create_dir(path: &Path) -> io::Result<()> {
        match std::fs::create_dir(path) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => Ok(()),
        }
    }

    pub(super) fn open_in(
        parent: &Path,
        name: &OsStr,
        create: bool,
        _mode: u32,
    ) -> io::Result<HeldDir> {
        let path = parent.join(name);
        if create {
            create_dir(&path)?;
        }
        hold(&path)
    }

    pub(super) fn dir(held: &HeldDir, name: &str, _mode: u32) -> io::Result<HeldDir> {
        let path = held.path.join(name);
        create_dir(&path)?;
        hold(&path)
    }

    pub(super) fn open_file(held: &HeldDir, name: &str) -> io::Result<File> {
        open_regular(held, name, OpenOptions::new().read(true))
    }

    pub(super) fn open_for_writing(held: &HeldDir, name: &str) -> io::Result<File> {
        open_regular(held, name, OpenOptions::new().write(true))
    }

    fn open_regular(held: &HeldDir, name: &str, options: &mut OpenOptions) -> io::Result<File> {
        let path = held.path.join(name);
        let file = options
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)?;
        let meta = file.metadata()?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(refused_link(&path));
        }
        if !meta.is_file() {
            return Err(io::Error::other(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        Ok(file)
    }

    pub(super) fn write(
        held: &HeldDir,
        name: &str,
        temporary: &str,
        contents: &[u8],
        _mode: u32,
        _owner: Option<(u32, u32)>,
    ) -> io::Result<()> {
        let temporary = held.path.join(temporary);
        // A leftover from an interrupted run would make `create_new` refuse; a link goes as a link.
        match std::fs::remove_file(&temporary) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .and_then(|mut file: File| {
                file.write_all(contents)?;
                file.sync_all()
            })
            .and_then(|()| std::fs::rename(&temporary, held.path.join(name)));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written
    }

    pub(super) fn create_new(
        held: &HeldDir,
        name: &str,
        contents: &[u8],
        _mode: u32,
    ) -> io::Result<()> {
        let path = held.path.join(name);
        // `CREATE_NEW` fails on any name that exists, a link or a junction included.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let written = file.write_all(contents).and_then(|()| file.sync_all());
        if written.is_err() {
            drop(file);
            let _ = std::fs::remove_file(&path);
        }
        written
    }

    pub(super) fn rename(held: &HeldDir, from: &str, to: &str) -> io::Result<()> {
        std::fs::rename(held.path.join(from), held.path.join(to))
    }

    pub(super) fn remove(held: &HeldDir, name: &str) -> io::Result<()> {
        std::fs::remove_file(held.path.join(name))
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn sync(_: &HeldDir) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A link at the file's own name is refused for a path the account's file offered, and
    /// followed for the administrator's; a file over the bound is refused, naming the bound.
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn a_privileged_read_holds_a_link_at_the_name_to_whose_word_the_path_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::write(&target, "bytes").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("link");

        let refused = read_privileged(&link, &[], None, Named::ByAccountFile, 16)
            .expect_err("a link at the name");
        assert!(refused.to_string().contains("is a link"), "{refused}");
        let followed = read_privileged(&link, &[], None, Named::ByAdministrator, 16).expect("read");
        assert_eq!(&**followed, b"bytes");

        let over = read_privileged(&target, &[], None, Named::ByAdministrator, 4)
            .expect_err("over the bound");
        let over = over.to_string();
        assert!(
            over.contains("larger than") && over.contains("4 bytes"),
            "{over}"
        );
    }
}
