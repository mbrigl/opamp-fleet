//! File operations in a directory the Client's service account can write, which never follow a
//! link that account could have planted (CLT-accounts-service-runs-under).
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
//! within a bound into a buffer sized before it reads and wiped when it is dropped (SEC-secrets-wiped-on-drop), so
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

    /// The existing directory `name` in this one, opened and held, never through a link and never
    /// created.
    ///
    /// # Errors
    /// Returns an error when `name` names no directory, or names a link.
    pub fn open_dir(&self, name: &str) -> io::Result<Self> {
        check_name(name)?;
        sys::open_dir(self, name)
    }

    /// Removes the empty directory `name` in this one; a link at the name is no directory.
    ///
    /// # Errors
    /// Returns an error when `name` names no empty directory.
    pub fn remove_dir(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        sys::remove_dir(self, name)
    }

    /// The entries of this directory, each by name and by what it is itself, a link as a link.
    /// A name that is not UTF-8 is left out, since no caller of this module could name it back.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be read.
    pub fn list(&self) -> io::Result<Vec<(String, Kind)>> {
        sys::list(self)
    }
}

/// What an entry of a [`HeldDir`] is itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symbolic link, a junction or another reparse point.
    Link,
    Other,
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
/// behind, and it is wiped when it is dropped (SEC-secrets-wiped-on-drop). A file that grew past the bound since
/// is refused.
///
/// # Errors
/// Returns an error when the file is no regular file, holds more than `limit` bytes, or cannot be
/// read. It leaves naming the file to the caller.
pub fn read_opened(file: std::fs::File, limit: u64) -> io::Result<Zeroizing<Vec<u8>>> {
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
///   is held as [`HeldDir::open_root`] holds it (CLT-accounts-service-runs-under §13), the state directory without
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
    io::Error::other(RefusedLink(path.to_path_buf()))
}

/// A link found where a plain entry was required, as the error of [`refused_link`] carries it.
#[derive(Debug)]
struct RefusedLink(std::path::PathBuf);

impl std::fmt::Display for RefusedLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is a link, which is refused: the service's account could have put it there",
            self.0.display()
        )
    }
}

impl std::error::Error for RefusedLink {}

/// Whether `e` is the refusal of a link where a plain entry was required: the refusal of this
/// module, or the system's own `ELOOP` of a path opened without following its last link.
#[must_use]
pub fn is_refused_link(e: &io::Error) -> bool {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ELOOP) {
        return true;
    }
    e.get_ref().is_some_and(|inner| inner.is::<RefusedLink>())
}

/// The `*at` system calls this module and the Client share on Unix, each wrapped once here: a
/// directory held by its handle, and an entry in it named and never followed where the flags say
/// so. The rest of this module and the Client's state, its reconfiguration and its service manager
/// work by directory handle through these (CLT-accounts-service-runs-under §11–§15).
#[cfg(unix)]
pub mod at {
    use std::ffi::CStr;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};

    /// The flags of an [`open`], which creates nothing. They are built only from the named flags
    /// below, none of which creates, so no value of this type carries `O_CREAT` or `O_TMPFILE`:
    /// [`or`](Self::or) joins the bits of two values and [`without`](Self::without) drops bits.
    /// A flag that creates is [`create`]'s, which passes the mode `openat` then reads.
    ///
    /// Each example below differs from this one, which compiles, in the flags alone:
    ///
    /// ```
    /// use fleet_util::nofollow::at::{self, Flags};
    /// let _ = at::open(libc::AT_FDCWD, c"new", Flags::O_WRONLY.or(Flags::O_NOFOLLOW));
    /// ```
    ///
    /// Raw flags do not compile, so `O_CREAT` cannot reach [`open`]:
    ///
    /// ```compile_fail,E0308
    /// use fleet_util::nofollow::at::{self, Flags};
    /// let _ = at::open(libc::AT_FDCWD, c"new", libc::O_WRONLY | libc::O_CREAT);
    /// ```
    ///
    /// Nor does a value built from them, as the field is private:
    ///
    /// ```compile_fail,E0423
    /// use fleet_util::nofollow::at::{self, Flags};
    /// let _ = at::open(libc::AT_FDCWD, c"new", Flags(libc::O_WRONLY | libc::O_CREAT));
    /// ```
    ///
    /// And no named flag creates:
    ///
    /// ```compile_fail,E0599
    /// use fleet_util::nofollow::at::{self, Flags};
    /// let _ = at::open(libc::AT_FDCWD, c"new", Flags::O_WRONLY.or(Flags::O_CREAT));
    /// ```
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Flags(libc::c_int);

    impl Flags {
        /// Opens for reading.
        pub const O_RDONLY: Self = Self::named(libc::O_RDONLY);
        /// Opens for writing.
        pub const O_WRONLY: Self = Self::named(libc::O_WRONLY);
        /// Opens a directory or nothing.
        pub const O_DIRECTORY: Self = Self::named(libc::O_DIRECTORY);
        /// Opens no link at the last component.
        pub const O_NOFOLLOW: Self = Self::named(libc::O_NOFOLLOW);
        /// Closes the descriptor across an `exec`.
        pub const O_CLOEXEC: Self = Self::named(libc::O_CLOEXEC);
        /// Never blocks on a FIFO.
        pub const O_NONBLOCK: Self = Self::named(libc::O_NONBLOCK);
        /// Never takes a terminal as the controlling one.
        pub const O_NOCTTY: Self = Self::named(libc::O_NOCTTY);
        /// Opens a path-only handle.
        #[cfg(target_os = "linux")]
        pub const O_PATH: Self = Self::named(libc::O_PATH);
        /// Opens a directory to search it, not to read it.
        #[cfg(target_os = "macos")]
        pub const O_SEARCH: Self = Self::named(libc::O_SEARCH);
        /// Opens a link itself.
        #[cfg(target_os = "macos")]
        pub const O_SYMLINK: Self = Self::named(libc::O_SYMLINK);

        /// The flags of `self` and of `other`.
        #[must_use]
        pub const fn or(self, other: Self) -> Self {
            Self(self.0 | other.0)
        }

        /// The flags of `self` without those of `other`.
        #[must_use]
        pub const fn without(self, other: Self) -> Self {
            Self(self.0 & !other.0)
        }

        /// Whether `self` holds every flag of `other`. `O_RDONLY` is no bit, so every value holds
        /// it.
        #[must_use]
        pub const fn contains(self, other: Self) -> bool {
            self.0 & other.0 == other.0
        }

        /// The flag `bits`, which must not create: a named flag that does fails to compile, as the
        /// assertion is evaluated with the constant. `O_TMPFILE` holds `O_DIRECTORY`, which opens
        /// nothing new, so only its own bit is the one that creates.
        const fn named(bits: libc::c_int) -> Self {
            #[cfg(target_os = "linux")]
            let tmpfile = libc::O_TMPFILE & !libc::O_DIRECTORY;
            #[cfg(not(target_os = "linux"))]
            let tmpfile = 0;
            assert!(bits & (libc::O_CREAT | tmpfile) == 0, "a flag that creates");
            Self(bits)
        }
    }

    /// A directory, opened as one and never through a link.
    pub const DIRECTORY: Flags = Flags::O_RDONLY
        .or(Flags::O_DIRECTORY)
        .or(Flags::O_NOFOLLOW)
        .or(Flags::O_CLOEXEC);

    /// `name` in `dir`, opened with `flags`; the last component never follows a link where
    /// `flags` says `O_NOFOLLOW`. Nothing is created ([`Flags`]), so `openat` reads no mode.
    pub fn open(dir: RawFd, name: &CStr, flags: Flags) -> io::Result<OwnedFd> {
        // SAFETY: a descriptor or `AT_FDCWD` and a NUL-terminated name; `Flags` holds no flag
        // that creates, so no mode.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags.0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// `name` in `dir`, created with `flags`, which name `O_CREAT`, at `mode` before the umask.
    pub fn create(dir: RawFd, name: &CStr, flags: libc::c_int, mode: u32) -> io::Result<OwnedFd> {
        // SAFETY: a descriptor or `AT_FDCWD`, a NUL-terminated name, and the mode `O_CREAT` reads.
        let fd = unsafe { libc::openat(dir, name.as_ptr(), flags, mode as libc::c_uint) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openat` returned a new descriptor that nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    /// Creates the directory `name` in `dir` at `mode` before the umask. A name that exists, a
    /// link included, is `AlreadyExists`: `mkdirat` never follows a link at the name.
    pub fn mkdir(dir: RawFd, name: &CStr, mode: u32) -> io::Result<()> {
        // The mode is at most `0o7777`, which every `mode_t` holds.
        #[allow(clippy::cast_possible_truncation)]
        let mode = mode as libc::mode_t;
        // SAFETY: a descriptor or `AT_FDCWD` and a NUL-terminated name.
        if unsafe { libc::mkdirat(dir, name.as_ptr(), mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Removes the entry `name` in `dir`; a link is removed as a link.
    pub fn unlink(dir: RawFd, name: &CStr) -> io::Result<()> {
        // SAFETY: a valid descriptor and a NUL-terminated name; without `AT_REMOVEDIR` a link is
        // removed as a link.
        if unsafe { libc::unlinkat(dir, name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Removes the empty directory `name` in `dir`; a link at the name is no directory.
    pub fn rmdir(dir: RawFd, name: &CStr) -> io::Result<()> {
        // SAFETY: a valid descriptor and a NUL-terminated name; only an empty directory goes.
        if unsafe { libc::unlinkat(dir, name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The entry `name` in `dir` itself, a link as a link.
    pub fn lstat(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: a valid descriptor, a NUL-terminated name and room for one `stat`.
        let rc = unsafe {
            libc::fstatat(
                dir,
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstatat` succeeded and filled it.
        Ok(unsafe { st.assume_init() })
    }

    /// Renames `from` in `from_dir` to `to` in `to_dir`; neither name is followed.
    pub fn rename(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> io::Result<()> {
        // SAFETY: valid descriptors and two NUL-terminated names.
        if unsafe { libc::renameat(from_dir, from.as_ptr(), to_dir, to.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Sets the mode of what `handle` holds.
    pub fn fchmod(handle: &OwnedFd, mode: u32) -> io::Result<()> {
        // The mode is at most `0o7777`, which every `mode_t` holds.
        #[allow(clippy::cast_possible_truncation)]
        let mode = mode as libc::mode_t;
        // SAFETY: a valid descriptor.
        if unsafe { libc::fchmod(handle.as_raw_fd(), mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Flushes what `handle` holds to the disk.
    pub fn sync(handle: &OwnedFd) -> io::Result<()> {
        // SAFETY: a valid descriptor.
        if unsafe { libc::fsync(handle.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(unix)]
mod sys {
    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io::{self, Write as _};
    use std::os::fd::{AsRawFd as _, RawFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::Path;

    use super::at::{self, Flags, DIRECTORY};
    use super::{refused_link, HeldDir, Kind};

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    /// A link at the last component opened with `O_NOFOLLOW` reads as `ELOOP`.
    fn named(e: io::Error, path: &Path) -> io::Error {
        if e.raw_os_error() == Some(libc::ELOOP) {
            refused_link(path)
        } else {
            e
        }
    }

    /// Creates the directory, or finds it there: `mkdirat` never follows a link at the name, so one
    /// there is `EEXIST`, and the open after it refuses it.
    fn mkdirat(dir: RawFd, name: &CString, mode: u32) -> io::Result<()> {
        match at::mkdir(dir, name, mode) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => Err(e),
            _ => Ok(()),
        }
    }

    pub(super) fn open_root(path: &Path) -> io::Result<HeldDir> {
        let fd = at::open(
            libc::AT_FDCWD,
            &c_name(path.as_os_str())?,
            Flags::O_RDONLY.or(Flags::O_DIRECTORY).or(Flags::O_CLOEXEC),
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
        let parent_fd = at::open(
            libc::AT_FDCWD,
            &c_name(parent.as_os_str())?,
            Flags::O_RDONLY.or(Flags::O_DIRECTORY).or(Flags::O_CLOEXEC),
        )?;
        let name = c_name(name)?;
        if create {
            mkdirat(parent_fd.as_raw_fd(), &name, mode)?;
        }
        let fd = at::open(parent_fd.as_raw_fd(), &name, DIRECTORY).map_err(|e| named(e, &path))?;
        if create {
            at::fchmod(&fd, mode)?;
        }
        Ok(HeldDir { path, fd })
    }

    pub(super) fn dir(held: &HeldDir, name: &str, mode: u32) -> io::Result<HeldDir> {
        let path = held.path.join(name);
        let c = c_name(OsStr::new(name))?;
        mkdirat(held.fd.as_raw_fd(), &c, mode)?;
        let fd = at::open(held.fd.as_raw_fd(), &c, DIRECTORY).map_err(|e| named(e, &path))?;
        Ok(HeldDir { path, fd })
    }

    pub(super) fn open_file(held: &HeldDir, name: &str) -> io::Result<File> {
        open_regular(held, name, Flags::O_RDONLY)
    }

    pub(super) fn open_for_writing(held: &HeldDir, name: &str) -> io::Result<File> {
        open_regular(held, name, Flags::O_WRONLY)
    }

    fn open_regular(held: &HeldDir, name: &str, access: Flags) -> io::Result<File> {
        let path = held.path.join(name);
        // `O_NONBLOCK`: a FIFO at the name must not stall a privileged run; a regular file ignores it.
        let fd = at::open(
            held.fd.as_raw_fd(),
            &c_name(OsStr::new(name))?,
            access
                .or(Flags::O_NOFOLLOW)
                .or(Flags::O_NONBLOCK)
                .or(Flags::O_CLOEXEC),
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
        match at::unlink(dir, &temporary) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let written = at::create(
            dir,
            &temporary,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
        .and_then(|fd| {
            // The umask may have narrowed the mode the open asked for.
            at::fchmod(&fd, mode)?;
            if let Some((uid, gid)) = owner {
                std::os::unix::fs::fchown(&fd, Some(uid), Some(gid))?;
            }
            let mut file = File::from(fd);
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|()| at::rename(dir, &temporary, dir, &c_name(OsStr::new(name))?));
        if written.is_err() {
            let _ = at::unlink(dir, &temporary);
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
        let fd = at::create(
            dir,
            &c,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )?;
        let written = at::fchmod(&fd, mode).and_then(|()| {
            let mut file = File::from(fd);
            file.write_all(contents)?;
            file.sync_all()
        });
        if written.is_err() {
            let _ = at::unlink(dir, &c);
        }
        written
    }

    pub(super) fn rename(held: &HeldDir, from: &str, to: &str) -> io::Result<()> {
        let dir = held.fd.as_raw_fd();
        at::rename(
            dir,
            &c_name(OsStr::new(from))?,
            dir,
            &c_name(OsStr::new(to))?,
        )
    }

    pub(super) fn remove(held: &HeldDir, name: &str) -> io::Result<()> {
        at::unlink(held.fd.as_raw_fd(), &c_name(OsStr::new(name))?)
    }

    pub(super) fn open_dir(held: &HeldDir, name: &str) -> io::Result<HeldDir> {
        let path = held.path.join(name);
        let fd = at::open(held.fd.as_raw_fd(), &c_name(OsStr::new(name))?, DIRECTORY)
            .map_err(|e| named(e, &path))?;
        Ok(HeldDir { path, fd })
    }

    pub(super) fn remove_dir(held: &HeldDir, name: &str) -> io::Result<()> {
        at::rmdir(held.fd.as_raw_fd(), &c_name(OsStr::new(name))?)
    }

    /// Read through a duplicate of the held descriptor, so the directory read is the one held,
    /// whatever stands at its path by now.
    pub(super) fn list(held: &HeldDir) -> io::Result<Vec<(String, Kind)>> {
        use std::os::fd::IntoRawFd as _;
        let raw = held.fd.try_clone()?.into_raw_fd();
        // SAFETY: `raw` is an open descriptor this function owns; `fdopendir` takes it over on
        // success, and it is closed here on failure.
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: `raw` is still this function's own on failure.
            unsafe { libc::close(raw) };
            return Err(e);
        }
        // SAFETY: `dir` is a valid stream; the duplicate shares the held descriptor's offset.
        unsafe { libc::rewinddir(dir) };
        let mut entries = Vec::new();
        loop {
            // SAFETY: `dir` is a valid stream; the entry stays valid until the next call.
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                break;
            }
            // SAFETY: `d_name` of a returned entry is NUL-terminated.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            let bytes = name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };
            let kind = match at::lstat(held.fd.as_raw_fd(), name) {
                Ok(st) => match st.st_mode & libc::S_IFMT {
                    libc::S_IFREG => Kind::File,
                    libc::S_IFDIR => Kind::Dir,
                    libc::S_IFLNK => Kind::Link,
                    _ => Kind::Other,
                },
                Err(_) => Kind::Other,
            };
            entries.push((text.to_string(), kind));
        }
        // SAFETY: `dir` is a valid stream, closed once; it closes the duplicate with it.
        unsafe { libc::closedir(dir) };
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }

    pub(super) fn sync(held: &HeldDir) -> io::Result<()> {
        at::sync(&held.fd)
    }
}

#[cfg(windows)]
mod sys {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Write as _};
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    use super::{refused_link, HeldDir, Kind};

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

    pub(super) fn open_dir(held: &HeldDir, name: &str) -> io::Result<HeldDir> {
        hold(&held.path.join(name))
    }

    pub(super) fn remove_dir(held: &HeldDir, name: &str) -> io::Result<()> {
        std::fs::remove_dir(held.path.join(name))
    }

    /// Read by path: the held handle keeps the directory from being renamed or replaced.
    pub(super) fn list(held: &HeldDir) -> io::Result<Vec<(String, Kind)>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&held.path)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let kind = match std::fs::symlink_metadata(entry.path()) {
                Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                    Kind::Link
                }
                Ok(meta) if meta.is_dir() => Kind::Dir,
                Ok(meta) if meta.is_file() => Kind::File,
                _ => Kind::Other,
            };
            entries.push((name, kind));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(entries)
    }

    #[allow(clippy::unnecessary_wraps)]
    pub(super) fn sync(_: &HeldDir) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A held directory lists its entries as what they are, opens a directory in it but no link,
    /// and removes an empty directory.
    #[test]
    fn a_held_directory_lists_opens_and_removes() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f"), b"x").expect("write");
        std::fs::create_dir(dir.path().join("d")).expect("mkdir");
        std::os::unix::fs::symlink("/", dir.path().join("l")).expect("link");
        let held = HeldDir::open(dir.path()).expect("held");
        assert_eq!(
            held.list().expect("list"),
            vec![
                ("d".to_string(), Kind::Dir),
                ("f".to_string(), Kind::File),
                ("l".to_string(), Kind::Link)
            ]
        );
        assert!(held.open_dir("d").is_ok());
        assert!(held.open_dir("l").is_err(), "a link is not entered");
        assert!(held.open_dir("missing").is_err(), "nothing is created");
        held.remove_dir("d").expect("an empty directory goes");
        assert!(held.remove_dir("l").is_err(), "a link is no directory");
        assert_eq!(held.list().expect("list").len(), 2);
    }

    /// A link at the file's own name is refused for a path the account's file offered, and
    /// followed for the administrator's; a file over the bound is refused, naming the bound.
    /// Verifies: CLT-accounts-service-runs-under
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
