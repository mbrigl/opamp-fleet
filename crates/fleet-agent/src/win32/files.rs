//! Files and directories reached one handle at a time, never through a link, and their owner and
//! DACL read and set through that handle (ADR-0059 clauses 6 and 10) — what `storage::handle` is on
//! Unix.
//!
//! Every entry is opened as itself (`FILE_FLAG_OPEN_REPARSE_POINT`): a symbolic link or a junction
//! is the link, never what it names, and a walk enters no link. A directory on the way is held
//! open without `FILE_SHARE_DELETE` for as long as anything beneath it is reached, so nobody can
//! rename it, or replace it with a link, under the walk (CreateFileW): the path from the held
//! directory to the next entry names the same directory throughout.
//!
//! A DACL is set with `SetKernelObjectSecurity` on the handle, which changes that one object.
//! Learn points file systems to `SetSecurityInfo` and `SetNamedSecurityInfo` instead, because
//! those propagate what a directory passes on to what it holds; that propagation is exactly what
//! is not wanted here, since nothing documents that it stays out of a junction a process account
//! placed. What a directory passes on to what it already holds is applied by the walk itself
//! ([`super::acl`]).

use std::fs::File;
use std::io;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};

use super::acl::Dacl;
use super::Sid;

const OWNER_SECURITY_INFORMATION: u32 = 0x1;
const DACL_SECURITY_INFORMATION: u32 = 0x4;

const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
/// `FILE_FLAG_BACKUP_SEMANTICS`, which opens a directory, and `FILE_FLAG_OPEN_REPARSE_POINT`,
/// which opens a link as itself.
const AS_ITSELF: u32 = 0x0200_0000 | 0x0020_0000;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
const REPARSE_POINT: u32 = 0x400;

/// What an entry is opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Its attributes alone: a directory passed through.
    Pass,
    /// Its owner and its DACL, read and changed.
    Dacl,
    /// Its owner changed too.
    Owner,
}

impl Access {
    fn mask(self) -> u32 {
        let read = FILE_READ_ATTRIBUTES | SYNCHRONIZE;
        match self {
            Access::Pass => read,
            Access::Dacl => read | READ_CONTROL | WRITE_DAC,
            Access::Owner => read | READ_CONTROL | WRITE_DAC | WRITE_OWNER,
        }
    }
}

/// One entry, opened as itself.
pub struct Entry {
    pub path: PathBuf,
    file: File,
    /// A directory, and no link to one.
    pub dir: bool,
    /// A symbolic link or a junction.
    pub link: bool,
}

impl Entry {
    /// Opens `path` as itself for `access`; a directory held so that nobody renames it meanwhile
    /// where `hold`.
    ///
    /// # Errors
    /// Returns why it cannot be opened: `NotFound` where it does not exist.
    pub fn open(path: &Path, access: Access, hold: bool) -> io::Result<Entry> {
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE | if hold { 0 } else { FILE_SHARE_DELETE };
        let file = std::fs::OpenOptions::new()
            .access_mode(access.mask())
            .share_mode(share)
            .custom_flags(AS_ITSELF)
            .open(path)?;
        let meta = file.metadata()?;
        let link = meta.file_attributes() & REPARSE_POINT != 0;
        Ok(Entry {
            path: path.to_path_buf(),
            dir: meta.is_dir() && !link,
            link,
            file,
        })
    }

    /// The entry's owner.
    ///
    /// # Errors
    /// Returns why it could not be read.
    pub fn owner(&self) -> io::Result<Sid> {
        super::file_owner(self.file.as_raw_handle())
    }

    /// The entry's DACL.
    ///
    /// # Errors
    /// Returns why it could not be read.
    pub fn dacl(&self) -> io::Result<Dacl> {
        let text = super::security_text(self.file.as_raw_handle(), DACL_SECURITY_INFORMATION)?;
        Dacl::parse(&text).map_err(io::Error::other)
    }

    /// Sets the entry's DACL to `dacl`, on this object alone.
    ///
    /// # Errors
    /// Returns why it could not be set.
    pub fn set_dacl(&self, dacl: &Dacl) -> io::Result<()> {
        self.set(&dacl.text(), DACL_SECURITY_INFORMATION)
    }

    /// Makes `owner` — a SID or an SDDL alias such as `BA` — the entry's owner.
    ///
    /// # Errors
    /// Returns why it could not be set.
    pub fn set_owner(&self, owner: &str) -> io::Result<()> {
        self.set(&format!("O:{owner}"), OWNER_SECURITY_INFORMATION)
    }

    fn set(&self, sddl: &str, info: u32) -> io::Result<()> {
        super::set_security(self.file.as_raw_handle(), info, sddl)
    }
}

/// Opens `dir`, `root` or beneath it, for `access`, one component at a time from `root`: each held
/// while the next is opened, none a link, and each that is missing created where `create`. Returns
/// the directories on the way, held, and `dir`.
///
/// # Errors
/// Returns why `dir` cannot be reached: a link or a file on the way, or a component missing.
pub fn open_beneath(
    root: &Path,
    dir: &Path,
    access: Access,
    create: bool,
) -> io::Result<(Vec<Entry>, Entry)> {
    let beneath = dir.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not beneath {}", dir.display(), root.display()),
        )
    })?;
    let parts: Vec<_> = beneath.components().collect();
    let mut held = Vec::new();
    let mut path = root.to_path_buf();
    for index in 0..=parts.len() {
        if index > 0 {
            path.push(parts[index - 1]);
        }
        let last = index == parts.len();
        let wanted = if last { access } else { Access::Pass };
        let entry = match Entry::open(&path, wanted, true) {
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => {
                match std::fs::create_dir(&path) {
                    Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
                    _ => {}
                }
                Entry::open(&path, wanted, true)?
            }
            entry => entry?,
        };
        if !entry.dir {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is a link or no directory", path.display()),
            ));
        }
        if last {
            return Ok((held, entry));
        }
        held.push(entry);
    }
    unreachable!("the loop returns at its last component")
}

/// Walks what the directory `top` holds, top-down, calling `visit` for each entry with its
/// parent: never into a link, and past every entry `skip` owns with all beneath it, each named in
/// `skipped`. `visit` answers whether to go into a directory. An entry gone meanwhile is passed
/// over.
///
/// # Errors
/// Returns the first error opening or visiting an entry.
pub fn walk(
    top: &Entry,
    access: Access,
    skip: Option<&Sid>,
    visit: &mut dyn FnMut(&Entry, &Entry) -> io::Result<bool>,
    skipped: &mut Vec<PathBuf>,
) -> io::Result<()> {
    for item in std::fs::read_dir(&top.path)? {
        let path = match item {
            Ok(item) => item.path(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let entry = match open_held(&path, access) {
            Ok(entry) => entry,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            // What may not be changed is first asked whose it is: the skipped account's needs no
            // change.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && skip.is_some() => {
                let owned = Entry::open(&path, Access::Pass, false)
                    .ok()
                    .and_then(|entry| {
                        let read = std::fs::OpenOptions::new()
                            .access_mode(READ_CONTROL)
                            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                            .custom_flags(AS_ITSELF)
                            .open(&entry.path)
                            .ok()?;
                        super::file_owner(read.as_raw_handle()).ok()
                    });
                if owned.as_ref() == skip {
                    skipped.push(path);
                    continue;
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        if skip.is_some_and(|skip| entry.owner().ok().as_ref() == Some(skip)) {
            skipped.push(path);
            continue;
        }
        if visit(&entry, top)? && entry.dir {
            walk(&entry, access, skip, visit, skipped)?;
        }
    }
    Ok(())
}

/// `ERROR_SHARING_VIOLATION`.
const SHARING_VIOLATION: i32 = 32;

/// Opens `path` as itself, held where it is a directory: a file that something holds open for
/// deleting cannot be held, and needs no holding, as nothing is reached through it.
fn open_held(path: &Path, access: Access) -> io::Result<Entry> {
    match Entry::open(path, access, true) {
        Err(e) if e.raw_os_error() == Some(SHARING_VIOLATION) => {
            let entry = Entry::open(path, access, false)?;
            if entry.dir {
                return Err(e);
            }
            Ok(entry)
        }
        entry => entry,
    }
}

/// Grants `sid` the ACEs `[on_top, on_dir, on_file]`: `on_top` on `top`, a directory or a file,
/// `on_dir` on each directory and `on_file` on each file beneath it, each in place of what that
/// account was granted there, never through a link and past what `skip` owns. Changes nothing
/// where `top` holds `on_top` already and `again` is not asked. Returns what was skipped.
///
/// # Errors
/// Returns the first entry whose DACL could not be read or set.
pub fn grant_tree(
    top: &Entry,
    sid: &str,
    [on_top, on_dir, on_file]: [&[String]; 3],
    skip: Option<&Sid>,
    again: bool,
) -> io::Result<Vec<PathBuf>> {
    let mut skipped = Vec::new();
    let own = |entry: &Entry| if entry.dir { on_dir } else { on_file };
    let dacl = top.dacl()?;
    if !again && on_top.iter().all(|ace| dacl.holds(ace)) {
        return Ok(skipped);
    }
    top.set_dacl(&dacl.granting(sid, on_top))?;
    if top.dir {
        walk(
            top,
            Access::Dacl,
            skip,
            &mut |entry, _| {
                entry.set_dacl(&entry.dacl()?.granting(sid, own(entry)))?;
                Ok(true)
            },
            &mut skipped,
        )?;
    }
    Ok(skipped)
}
