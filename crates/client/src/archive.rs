//! Unpacking a package artifact (ADR-0018).
//!
//! An upstream release is an archive — `opentelemetry-collector-releases` publishes `.tar.gz` and
//! never a bare binary — so the artifact a Supervisor is handed is often a container holding the
//! program rather than the program itself. Nothing between the artifact's author and this host
//! repacks it (ADR-0018), which is what lets an Agent verify the very hash the release published;
//! the price is that the Agent is the end that has to open it.
//!
//! Two rules keep opening someone else's archive from becoming a way in:
//!
//! - **The archive chooses as little as it can.** For a single-file package it chooses nothing:
//!   exactly one member is extracted, to a destination this Client picked, so a member named
//!   `../../etc/cron.d/x` writes to that destination like any other and there is no traversal to
//!   defend against. A tree (ADR-0023) cannot work that way — its members keep their own relative
//!   paths — so every path is validated by [`safe_member_path`] *before a byte is written*, and one
//!   member this Client will not write refuses the whole archive.
//! - **The output is bounded.** Extraction stops at [`MAX_UNPACKED_BYTES`] — for a tree, across all
//!   its members, and at [`MAX_TREE_MEMBERS`] members — so an archive that expands without end
//!   fills a temporary file up to that limit and then fails, rather than a disk.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

/// The largest member this Client will write out of an archive. An agent binary is hundreds of
/// megabytes; anything past this is not a program but a way to fill a disk.
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// The most members a tree may hold (ADR-0023). An agent with its libraries and plugins is a few
/// hundred files; a hundred thousand empty ones is a way to spend an afternoon creating inodes.
const MAX_TREE_MEMBERS: usize = 10_000;

/// Whether a member's path may be written at all (ADR-0023).
///
/// Unpacking a whole tree is the point where an archive starts having a say in *where* bytes land —
/// the single-member path has no traversal to defend against because it never uses an archive's
/// path at all. So every path is checked before anything is written, and one bad member refuses the
/// whole archive rather than the member: a half-unpacked agent is worse than none, and an archive
/// that tried is not one to take the rest of on trust.
///
/// Rejected: an absolute path, a root or drive prefix, and any `..` component. `.` components are
/// dropped, which is what `Path::components` does anyway.
fn safe_member_path(raw: &Path) -> Result<std::path::PathBuf, String> {
    use std::path::Component;
    let mut safe = std::path::PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!("{} climbs out of the archive", raw.display()))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("{} is an absolute path", raw.display()))
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(format!("{} names nothing", raw.display()));
    }
    Ok(safe)
}

/// Whether `path` ends with all of `suffix`'s components — how `program_path` finds its member
/// (ADR-0023).
///
/// A release archive wraps its tree in a version-named directory, so matching from the root would
/// mean writing that version into the configuration and having it be wrong at the next release.
/// `bin/fluent-bit` matches `fluent-bit-3.1.0/bin/fluent-bit` and keeps being right.
fn ends_with_components(path: &Path, suffix: &Path) -> bool {
    let mut left: Vec<_> = path.components().collect();
    let right: Vec<_> = suffix.components().collect();
    if right.is_empty() || left.len() < right.len() {
        return false;
    }
    left.split_off(left.len() - right.len()) == right
}

/// The directory prefix that `program_path`'s match sits under — everything before it, which is
/// what gets dropped so the unpacked tree starts where the configuration says it does.
fn tree_prefix(matched: &Path, program_path: &Path) -> std::path::PathBuf {
    let components: Vec<_> = matched.components().collect();
    let keep = components.len() - program_path.components().count();
    components[..keep].iter().collect()
}

/// Picks the one member matching `program_path` and returns the prefix to strip, or says why it
/// cannot: nothing matched (naming what the archive holds), or several did (naming them, so the
/// operator can write more of the path).
fn locate_program(
    members: &[std::path::PathBuf],
    program_path: &Path,
) -> Result<std::path::PathBuf, String> {
    let matches: Vec<&std::path::PathBuf> = members
        .iter()
        .filter(|m| ends_with_components(m, program_path))
        .collect();
    match matches.as_slice() {
        [one] => Ok(tree_prefix(one, program_path)),
        [] => {
            let mut seen: Vec<String> = members
                .iter()
                .take(8)
                .map(|m| m.display().to_string())
                .collect();
            if members.len() > seen.len() {
                seen.push(format!("… and {} more", members.len() - seen.len()));
            }
            Err(format!(
                "the archive holds no member at {} (it holds: {})",
                program_path.display(),
                if seen.is_empty() {
                    "nothing".to_string()
                } else {
                    seen.join(", ")
                }
            ))
        }
        several => Err(format!(
            "{} matches {} members ({}) — write more of the path to say which",
            program_path.display(),
            several.len(),
            several
                .iter()
                .map(|m| m.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Creates `dest`'s parent directories, refusing to leave `root`.
fn prepare_parent(root: &Path, dest: &Path) -> Result<(), String> {
    let Some(parent) = dest.parent() else {
        return Ok(());
    };
    debug_assert!(parent.starts_with(root));
    std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))
}

/// What a downloaded artifact turns out to be, decided by its leading bytes rather than by a file
/// name — the artifact arrives named after its package, and the Server never inspects it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The program itself; install it as it is.
    Raw,
    /// A gzip stream — in practice a `.tar.gz` holding the program.
    TarGz,
    /// A 7z container, which may be encrypted (ADR-0018).
    SevenZ,
}

/// Reads the first bytes of `path` and says what it is.
///
/// # Errors
/// Returns an error when the file cannot be read.
pub fn detect(path: &Path) -> Result<Kind, String> {
    let mut head = [0u8; 6];
    let mut file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let read = read_up_to(&mut file, &mut head)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    // 1f 8b is the gzip magic; every `.tar.gz` starts with it.
    if read >= 2 && head[..2] == [0x1f, 0x8b] {
        return Ok(Kind::TarGz);
    }
    // "7z" followed by the rest of the 7z signature.
    if read == 6 && head == [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c] {
        return Ok(Kind::SevenZ);
    }
    Ok(Kind::Raw)
}

/// Extracts the member called `member` from the `.7z` at `archive`, writing it to `out`. `key`
/// opens an encrypted archive (`[packages] archive_key`); `None` is for one that is not encrypted.
///
/// Member selection and the output bound work exactly as for a `.tar.gz`: the archive names no
/// destination, and nothing past [`MAX_UNPACKED_BYTES`] is written.
///
/// # Errors
/// Returns an error when the archive cannot be opened — a wrong or missing key lands here — holds
/// no such member, or the member exceeds the limit.
pub fn extract_7z(
    archive: &Path,
    member: &str,
    out: &mut File,
    key: Option<&str>,
) -> Result<u64, String> {
    extract_7z_within(archive, member, out, key, MAX_UNPACKED_BYTES)
}

fn extract_7z_within(
    archive: &Path,
    member: &str,
    out: &mut File,
    key: Option<&str>,
    limit: u64,
) -> Result<u64, String> {
    let password = key.map(sevenz_rust2::Password::from).unwrap_or_default();
    let mut reader = sevenz_rust2::ArchiveReader::open(archive, password).map_err(open_7z_error)?;

    // The callback's error type is the crate's, and a member past the limit is no fault of the
    // archive format — so the outcome travels out here instead of being dressed as a 7z error.
    let mut outcome: Result<u64, String> = Err(String::new());
    let mut seen: Vec<String> = Vec::new();
    reader
        .for_each_entries(|entry, entry_reader| {
            if entry.is_directory {
                return Ok(true);
            }
            let name = Path::new(&entry.name)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name != member {
                if seen.len() < 8 && !name.is_empty() {
                    seen.push(name);
                }
                return Ok(true);
            }
            outcome = copy_within(entry_reader, out, limit)
                .map_err(|e| format!("cannot unpack {member:?} from the archive: {e}"));
            Ok(false) // found it; stop reading
        })
        .map_err(|e| format!("cannot read the archive: {e}"))?;

    match outcome {
        Err(reason) if reason.is_empty() => Err(format!(
            "the archive holds no member named {member:?} (it holds: {})",
            if seen.is_empty() {
                "nothing".to_string()
            } else {
                seen.join(", ")
            }
        )),
        other => other,
    }
}

/// Extracts the member called `member` from the `.tar.gz` at `archive`, writing it to `out`.
///
/// `member` is matched against each entry's **file name**, so `otelcol-contrib` finds the program
/// whether the archive stores it at the root or under a directory — which is the difference between
/// how one project and the next lays out a release.
///
/// # Errors
/// Returns an error when the archive cannot be read, holds no such member, or the member exceeds
/// [`MAX_UNPACKED_BYTES`].
pub fn extract_tar_gz(archive: &Path, member: &str, out: &mut File) -> Result<u64, String> {
    extract_tar_gz_within(archive, member, out, MAX_UNPACKED_BYTES)
}

fn extract_tar_gz_within(
    archive: &Path,
    member: &str,
    out: &mut File,
    limit: u64,
) -> Result<u64, String> {
    let file =
        File::open(archive).map_err(|e| format!("cannot read {}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let entries = tar
        .entries()
        .map_err(|e| format!("cannot read the archive: {e}"))?;

    // Kept for the error message: an operator who named the wrong member is best served by being
    // told what the archive actually holds.
    let mut seen: Vec<String> = Vec::new();
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("cannot read the archive: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("cannot read an archive entry: {e}"))?
            .into_owned();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name != member {
            if seen.len() < 8 && !name.is_empty() {
                seen.push(name);
            }
            continue;
        }
        return copy_within(&mut entry, out, limit)
            .map_err(|e| format!("cannot unpack {member:?} from the archive: {e}"));
    }
    Err(format!(
        "the archive holds no member named {member:?} (it holds: {})",
        if seen.is_empty() {
            "nothing".to_string()
        } else {
            seen.join(", ")
        }
    ))
}

/// What unpacking a tree wrote — for the log line, because members outside the program's own
/// directory are dropped and silently dropping files is how an install becomes a mystery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeSummary {
    /// Files written (directories are not counted).
    pub files: usize,
    /// Total bytes written.
    pub bytes: u64,
    /// Members outside the program's own directory, left unwritten.
    pub skipped: usize,
}

/// Extracts a `.tar.gz` **whole** into `dest` (ADR-0023), keeping each member's relative path and
/// dropping the directory prefix that `program_path`'s match sits under — so
/// `fluent-bit-3.1.0/bin/fluent-bit` with `program_path = bin/fluent-bit` lands at
/// `dest/bin/fluent-bit`, and the tree beside it comes along unchanged.
///
/// Every member is validated before anything is written; one that cannot be written safely refuses
/// the whole archive.
///
/// # Errors
/// Returns an error when the archive cannot be read, holds no or several members matching
/// `program_path`, carries a member this Client will not write, or exceeds the size or member
/// bounds.
pub fn extract_tree_tar_gz(
    archive: &Path,
    program_path: &Path,
    dest: &Path,
) -> Result<TreeSummary, String> {
    extract_tree_tar_gz_within(archive, program_path, dest, MAX_UNPACKED_BYTES)
}

/// The same, with the total budget spelled out — the bound is across *all* members, so a tree of
/// harmless-looking files still stops somewhere.
fn extract_tree_tar_gz_within(
    archive: &Path,
    program_path: &Path,
    dest: &Path,
    limit: u64,
) -> Result<TreeSummary, String> {
    let members = list_tar_gz(archive)?;
    let prefix = locate_program(&members, program_path)?;

    let file =
        File::open(archive).map_err(|e| format!("cannot read {}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut summary = TreeSummary {
        files: 0,
        bytes: 0,
        skipped: 0,
    };
    for entry in tar
        .entries()
        .map_err(|e| format!("cannot read the archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("cannot read the archive: {e}"))?;
        let raw = entry
            .path()
            .map_err(|e| format!("cannot read an archive entry: {e}"))?
            .into_owned();
        let member = safe_member_path(&raw)?;
        let Ok(relative) = member.strip_prefix(&prefix) else {
            summary.skipped += 1;
            continue;
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let out_path = dest.join(relative);
        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| format!("cannot create {}: {e}", out_path.display()))?;
            continue;
        }
        prepare_parent(dest, &out_path)?;
        let mut out = File::create(&out_path)
            .map_err(|e| format!("cannot write {}: {e}", out_path.display()))?;
        let written = copy_within(&mut entry, &mut out, limit - summary.bytes)
            .map_err(|e| format!("cannot unpack {}: {e}", relative.display()))?;
        summary.files += 1;
        summary.bytes += written;
        // A tar carries its own modes, and a tree needs them: the program is made executable by
        // its caller either way, but a helper beside it is only executable if the archive said so.
        #[cfg(unix)]
        if let Ok(mode) = entry.header().mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(summary)
}

/// Reads every member path of a `.tar.gz`, validating each — the pass that decides whether the
/// archive may be unpacked at all, before a byte of it is written.
fn list_tar_gz(archive: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let file =
        File::open(archive).map_err(|e| format!("cannot read {}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut members = Vec::new();
    for entry in tar
        .entries()
        .map_err(|e| format!("cannot read the archive: {e}"))?
    {
        let entry = entry.map_err(|e| format!("cannot read the archive: {e}"))?;
        let kind = entry.header().entry_type();
        // A symbolic or hard link is a member that names a path *outside* itself, which is the one
        // thing the sanitizer above cannot check by looking at where the member goes.
        if !kind.is_file() && !kind.is_dir() {
            return Err(format!(
                "the archive holds {:?}, which is not a file or a directory ({:?}) — refusing the \
                 whole archive",
                entry
                    .path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                kind
            ));
        }
        let raw = entry
            .path()
            .map_err(|e| format!("cannot read an archive entry: {e}"))?
            .into_owned();
        members.push(safe_member_path(&raw)?);
        if members.len() > MAX_TREE_MEMBERS {
            return Err(format!(
                "the archive holds more than {MAX_TREE_MEMBERS} members"
            ));
        }
    }
    Ok(members)
}

/// Extracts a `.7z` **whole** into `dest` (ADR-0023), exactly as [`extract_tree_tar_gz`] does for a
/// `.tar.gz`. `key` opens an encrypted archive.
///
/// Modes are not taken from the archive here: 7z carries Windows attributes, and a Unix mode
/// survives in them only by a convention this Client will not bet an agent's executability on. The
/// program is made executable by its caller; anything beside it that must be executable is a reason
/// to ship the tree as a `.tar.gz`.
///
/// # Errors
/// As [`extract_tree_tar_gz`], plus a wrong or missing `key`.
pub fn extract_tree_7z(
    archive: &Path,
    program_path: &Path,
    dest: &Path,
    key: Option<&str>,
) -> Result<TreeSummary, String> {
    let members = list_7z(archive, key)?;
    let prefix = locate_program(&members, program_path)?;

    let password = key.map(sevenz_rust2::Password::from).unwrap_or_default();
    let mut reader = sevenz_rust2::ArchiveReader::open(archive, password).map_err(open_7z_error)?;
    let mut summary = TreeSummary {
        files: 0,
        bytes: 0,
        skipped: 0,
    };
    let mut failure: Option<String> = None;
    reader
        .for_each_entries(|entry, entry_reader| {
            let member = match safe_member_path(Path::new(&entry.name)) {
                Ok(member) => member,
                Err(e) => {
                    failure = Some(e);
                    return Ok(false);
                }
            };
            let Ok(relative) = member.strip_prefix(&prefix) else {
                summary.skipped += 1;
                return Ok(true);
            };
            if relative.as_os_str().is_empty() {
                return Ok(true);
            }
            let out_path = dest.join(relative);
            if entry.is_directory {
                if let Err(e) = std::fs::create_dir_all(&out_path) {
                    failure = Some(format!("cannot create {}: {e}", out_path.display()));
                    return Ok(false);
                }
                return Ok(true);
            }
            if let Err(e) = prepare_parent(dest, &out_path) {
                failure = Some(e);
                return Ok(false);
            }
            let mut out = match File::create(&out_path) {
                Ok(out) => out,
                Err(e) => {
                    failure = Some(format!("cannot write {}: {e}", out_path.display()));
                    return Ok(false);
                }
            };
            match copy_within(entry_reader, &mut out, MAX_UNPACKED_BYTES - summary.bytes) {
                Ok(written) => {
                    summary.files += 1;
                    summary.bytes += written;
                    Ok(true)
                }
                Err(e) => {
                    failure = Some(format!("cannot unpack {}: {e}", relative.display()));
                    Ok(false)
                }
            }
        })
        .map_err(|e| format!("cannot read the archive: {e}"))?;
    match failure {
        Some(reason) => Err(reason),
        None => Ok(summary),
    }
}

/// Reads every member of a `.7z`, validating each — the counterpart to [`list_tar_gz`].
fn list_7z(archive: &Path, key: Option<&str>) -> Result<Vec<std::path::PathBuf>, String> {
    // Windows attribute bits a member may not carry. A reparse point is a symbolic link by another
    // name; the unix-extension bit puts a mode in the high half, and a mode may say link too.
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const UNIX_EXTENSION: u32 = 0x8000;
    const S_IFMT: u32 = 0o170000;
    const S_IFLNK: u32 = 0o120000;

    let password = key.map(sevenz_rust2::Password::from).unwrap_or_default();
    let reader = sevenz_rust2::ArchiveReader::open(archive, password).map_err(open_7z_error)?;
    let mut members = Vec::new();
    for entry in &reader.archive().files {
        if entry.is_anti_item {
            return Err(format!(
                "the archive holds an anti-item ({:?}), which deletes rather than installs",
                entry.name
            ));
        }
        if entry.has_windows_attributes {
            let attributes = entry.windows_attributes;
            let is_link = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                || (attributes & UNIX_EXTENSION != 0 && (attributes >> 16) & S_IFMT == S_IFLNK);
            if is_link {
                return Err(format!(
                    "the archive holds a link ({:?}), which names a path outside itself — \
                     refusing the whole archive",
                    entry.name
                ));
            }
        }
        members.push(safe_member_path(Path::new(&entry.name))?);
        if members.len() > MAX_TREE_MEMBERS {
            return Err(format!(
                "the archive holds more than {MAX_TREE_MEMBERS} members"
            ));
        }
    }
    Ok(members)
}

/// The two failures an operator actually causes when opening a `.7z`, named as such rather than as
/// a codec complaint.
fn open_7z_error(e: sevenz_rust2::Error) -> String {
    match e {
        sevenz_rust2::Error::PasswordRequired => {
            "the archive is encrypted; set [packages] archive_key in client.toml".to_string()
        }
        sevenz_rust2::Error::MaybeBadPassword(_) => {
            "cannot open the archive — [packages] archive_key is wrong, or the archive is damaged"
                .to_string()
        }
        other => format!("cannot read the archive: {other}"),
    }
}

/// Copies at most `limit` bytes, failing rather than writing past it.
fn copy_within(
    source: &mut (impl Read + ?Sized),
    out: &mut File,
    limit: u64,
) -> std::io::Result<u64> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        written += read as u64;
        if written > limit {
            return Err(std::io::Error::other(format!(
                "the member exceeds the {limit}-byte unpacking limit"
            )));
        }
        out.write_all(&buffer[..read])?;
    }
    out.flush()?;
    Ok(written)
}

/// Reads until the buffer is full or the file ends, returning how much was read. A short artifact
/// is not an error here — it simply is not gzip.
fn read_up_to(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    /// Builds a `.tar.gz` holding `members` — (path inside the archive, contents).
    fn tar_gz(dir: &Path, members: &[(&str, &[u8])]) -> std::path::PathBuf {
        tar_gz_at(&dir.join("release.tar.gz"), members)
    }

    /// The same, at a path of the caller's choosing — several archives in one temporary directory.
    fn tar_gz_at(path: &Path, members: &[(&str, &[u8])]) -> std::path::PathBuf {
        let file = File::create(path).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, content) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, name, *content)
                .expect("append");
        }
        builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip");
        path.to_path_buf()
    }

    #[test]
    fn detects_gzip_by_its_leading_bytes_and_anything_else_as_raw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz(dir.path(), &[("otelcol", b"binary")]);
        assert_eq!(detect(&archive).expect("detect"), Kind::TarGz);

        let raw = dir.path().join("plain");
        std::fs::write(&raw, b"#!/bin/sh\nexec sleep 1\n").expect("write");
        assert_eq!(detect(&raw).expect("detect"), Kind::Raw);

        // A file too short to carry a magic number is not an archive, and not an error either.
        let tiny = dir.path().join("tiny");
        std::fs::write(&tiny, b"x").expect("write");
        assert_eq!(detect(&tiny).expect("detect"), Kind::Raw);
    }

    /// The case this exists for: an upstream release lays the program under a directory, and the
    /// member is found by its file name rather than by a path someone has to know.
    #[test]
    fn extracts_the_named_member_wherever_the_archive_keeps_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz(
            dir.path(),
            &[
                ("LICENSE", b"text"),
                ("otelcol-contrib_0.157.0/otelcol-contrib", b"the-program"),
            ],
        );
        let out_path = dir.path().join("out");
        let mut out = File::create(&out_path).expect("create");
        let written = extract_tar_gz(&archive, "otelcol-contrib", &mut out).expect("extract");
        assert_eq!(written, b"the-program".len() as u64);
        assert_eq!(std::fs::read(&out_path).expect("read"), b"the-program");
    }

    #[test]
    fn a_missing_member_names_what_the_archive_does_hold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz(dir.path(), &[("LICENSE", b"text"), ("README", b"text")]);
        let mut out = File::create(dir.path().join("out")).expect("create");
        let err = extract_tar_gz(&archive, "otelcol", &mut out).expect_err("no such member");
        assert!(err.contains("otelcol"), "{err}");
        assert!(err.contains("LICENSE") && err.contains("README"), "{err}");
    }

    /// Builds a `.tar.gz` whose member name is written straight into the header, bypassing the
    /// `tar` crate's own refusal to *create* a `..` path — which is exactly what an archive from
    /// somewhere else may contain.
    fn tar_gz_with_raw_name(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        let path = dir.join("hostile.tar.gz");
        let file = File::create(&path).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Regular);
        {
            let raw = header.as_gnu_mut().expect("gnu header");
            raw.name[..name.len()].copy_from_slice(name.as_bytes());
        }
        header.set_cksum();
        builder.append(&header, content).expect("append");
        builder
            .into_inner()
            .expect("finish tar")
            .finish()
            .expect("finish gzip");
        path
    }

    /// A member whose stored path climbs out of any directory is written to *our* destination like
    /// any other: the archive never chooses where bytes land, so there is nothing to escape.
    #[test]
    fn an_escaping_member_path_still_lands_only_where_we_put_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = dir.path().join("outside-marker");
        let archive = tar_gz_with_raw_name(dir.path(), "../../outside-marker", b"evil");

        let out_path = dir.path().join("out");
        let mut out = File::create(&out_path).expect("create");
        extract_tar_gz(&archive, "outside-marker", &mut out).expect("extract");

        assert_eq!(std::fs::read(&out_path).expect("read"), b"evil");
        assert!(
            !outside.exists(),
            "nothing was written where the archive asked"
        );
    }

    /// Builds a `.7z`, encrypted when a key is given — the shape an operator produces for an
    /// artifact that must not be readable wherever it is stored.
    fn seven_z(dir: &Path, members: &[(&str, &[u8])], key: Option<&str>) -> std::path::PathBuf {
        let path = dir.join("release.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&path).expect("create");
        if let Some(key) = key {
            writer.set_content_methods(vec![
                sevenz_rust2::encoder_options::AesEncoderOptions::new(
                    sevenz_rust2::Password::from(key),
                )
                .into(),
                sevenz_rust2::EncoderMethod::LZMA2.into(),
            ]);
        }
        for (name, content) in members {
            writer
                .push_archive_entry(
                    sevenz_rust2::ArchiveEntry::new_file(name),
                    Some(std::io::Cursor::new(*content)),
                )
                .expect("push");
        }
        writer.finish().expect("finish");
        path
    }

    #[test]
    fn detects_a_7z_by_its_signature() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = seven_z(dir.path(), &[("otelcol", b"binary")], None);
        assert_eq!(detect(&archive).expect("detect"), Kind::SevenZ);
    }

    #[test]
    fn extracts_a_member_from_a_plain_7z() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = seven_z(
            dir.path(),
            &[("docs/LICENSE", b"text"), ("bin/otelcol", b"the-program")],
            None,
        );
        let out_path = dir.path().join("out");
        let mut out = File::create(&out_path).expect("create");
        let written = extract_7z(&archive, "otelcol", &mut out, None).expect("extract");
        assert_eq!(written, b"the-program".len() as u64);
        assert_eq!(std::fs::read(&out_path).expect("read"), b"the-program");
    }

    /// The reason `.7z` is supported at all: the artifact is unreadable without the key, and the
    /// key lives only on the Agent (ADR-0018).
    #[test]
    fn an_encrypted_7z_opens_with_the_key_and_not_without_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = seven_z(dir.path(), &[("otelcol", b"the-program")], Some("s3cr3t"));

        let out_path = dir.path().join("out");
        let mut out = File::create(&out_path).expect("create");
        let written = extract_7z(&archive, "otelcol", &mut out, Some("s3cr3t"))
            .expect("the right key opens it");
        assert_eq!(written, b"the-program".len() as u64);
        assert_eq!(std::fs::read(&out_path).expect("read"), b"the-program");

        // No key at all, and the wrong key: both must say what an operator has to fix.
        let mut out = File::create(dir.path().join("out2")).expect("create");
        let err = extract_7z(&archive, "otelcol", &mut out, None).expect_err("no key");
        assert!(err.contains("archive_key"), "{err}");

        let mut out = File::create(dir.path().join("out3")).expect("create");
        let err = extract_7z(&archive, "otelcol", &mut out, Some("wrong")).expect_err("wrong key");
        assert!(err.contains("archive_key"), "{err}");
    }

    #[test]
    fn a_member_past_the_limit_is_refused_rather_than_written() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz(dir.path(), &[("otelcol", &[0u8; 4096])]);
        let out_path = dir.path().join("out");
        let mut out = File::create(&out_path).expect("create");

        let err =
            extract_tar_gz_within(&archive, "otelcol", &mut out, 1024).expect_err("past the limit");
        assert!(err.contains("limit"), "{err}");
        // Whatever was written before the limit hit is left for the caller to discard; what
        // matters is that the copy stopped instead of running to the end.
        out.seek(std::io::SeekFrom::Start(0)).expect("seek");
        assert!(
            std::fs::metadata(&out_path).expect("stat").len() <= 1024 + 64 * 1024,
            "the copy stopped near the limit"
        );
    }

    // ── Trees (ADR-0023) ────────────────────────────────────────────────────────

    /// A release as agents actually ship: the program, the shared objects it loads, and a data
    /// file, all under one version-named directory.
    fn release(dir: &Path, wrapper: &str) -> std::path::PathBuf {
        tar_gz_at(
            &dir.join(format!("{wrapper}.tar.gz")),
            &[
                (&format!("{wrapper}/bin/fluent-bit"), b"the-program"),
                (&format!("{wrapper}/lib/libcrypto.so.3"), b"a-shared-object"),
                (&format!("{wrapper}/etc/parsers.conf"), b"parsers"),
            ],
        )
    }

    fn dest(dir: &Path) -> std::path::PathBuf {
        let dest = dir.join("unpacked");
        std::fs::create_dir_all(&dest).expect("create the destination");
        dest
    }

    /// The whole point: everything lands, keeping its place relative to the program.
    #[test]
    fn a_tree_lands_whole_with_the_wrapper_directory_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = release(dir.path(), "fluent-bit-3.1.0");
        let dest = dest(dir.path());

        let summary = extract_tree_tar_gz(&archive, Path::new("bin/fluent-bit"), &dest)
            .expect("unpack the tree");

        assert_eq!(summary.files, 3, "every member is written");
        assert_eq!(summary.skipped, 0);
        assert_eq!(
            std::fs::read(dest.join("bin/fluent-bit")).expect("the program"),
            b"the-program"
        );
        assert_eq!(
            std::fs::read(dest.join("lib/libcrypto.so.3")).expect("the shared object"),
            b"a-shared-object",
            "the libraries the program loads come with it — the reason a tree exists at all"
        );
        assert_eq!(
            std::fs::read(dest.join("etc/parsers.conf")).expect("the data file"),
            b"parsers"
        );
    }

    /// The next release renames the wrapper, and the configuration must not have to follow it —
    /// that drift is what ADR-0022 exists to make unspellable, and it applies here too.
    #[test]
    fn the_same_program_path_finds_the_program_under_any_wrapper() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program_path = Path::new("bin/fluent-bit");

        for wrapper in ["fluent-bit-3.1.0", "fluent-bit-4.0.0-linux-amd64"] {
            let archive = release(dir.path(), wrapper);
            let dest = dir.path().join(format!("out-{wrapper}"));
            std::fs::create_dir_all(&dest).expect("create");
            extract_tree_tar_gz(&archive, program_path, &dest).expect("unpack");
            assert_eq!(
                std::fs::read(dest.join("bin/fluent-bit")).expect("the program"),
                b"the-program",
                "{wrapper} resolved to the same layout"
            );
        }
    }

    /// A member beside the wrapper is not part of the tree. Dropping it is right; dropping it
    /// silently is not, so it is counted and the caller logs the count.
    #[test]
    fn members_outside_the_programs_own_directory_are_left_out_and_counted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz(
            dir.path(),
            &[
                ("LICENSE", b"text"),
                ("fluent-bit-3.1.0/bin/fluent-bit", b"the-program"),
            ],
        );
        let dest = dest(dir.path());

        let summary =
            extract_tree_tar_gz(&archive, Path::new("bin/fluent-bit"), &dest).expect("unpack");

        assert_eq!(summary.files, 1);
        assert_eq!(summary.skipped, 1);
        assert!(!dest.join("LICENSE").exists());
    }

    /// The property the single-member path had for free and this one has to earn: an archive that
    /// names a path outside the destination is refused *whole*, before anything is written.
    #[test]
    fn a_member_that_climbs_out_refuses_the_archive_before_writing_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz_with_raw_name(dir.path(), "../../etc/cron.d/x", b"pwned");
        let dest = dest(dir.path());

        let err = extract_tree_tar_gz(&archive, Path::new("bin/fluent-bit"), &dest)
            .expect_err("must be refused");

        assert!(err.contains("climbs out"), "{err}");
        assert_eq!(
            std::fs::read_dir(&dest).expect("read").count(),
            0,
            "nothing is written from an archive that tried"
        );
    }

    #[test]
    fn an_absolute_member_refuses_the_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz_with_raw_name(dir.path(), "/etc/cron.d/x", b"pwned");
        let dest = dest(dir.path());

        let err = extract_tree_tar_gz(&archive, Path::new("bin/fluent-bit"), &dest)
            .expect_err("must be refused");

        assert!(err.contains("absolute"), "{err}");
        assert_eq!(std::fs::read_dir(&dest).expect("read").count(), 0);
    }

    /// A link is the one member the path check cannot judge, because what it names is not where it
    /// goes — so its mere presence refuses the archive.
    #[test]
    fn a_link_member_refuses_the_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("linked.tar.gz");
        {
            let file = File::create(&path).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut program = tar::Header::new_gnu();
            program.set_size(11);
            program.set_mode(0o755);
            program.set_cksum();
            builder
                .append_data(&mut program, "app/bin/fluent-bit", &b"the-program"[..])
                .expect("append");
            let mut link = tar::Header::new_gnu();
            link.set_size(0);
            link.set_mode(0o777);
            link.set_entry_type(tar::EntryType::Symlink);
            builder
                .append_link(&mut link, "app/lib/passwd", "/etc/passwd")
                .expect("append the link");
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }
        let dest = dest(dir.path());

        let err = extract_tree_tar_gz(&path, Path::new("bin/fluent-bit"), &dest)
            .expect_err("must be refused");

        assert!(err.contains("not a file or a directory"), "{err}");
        assert_eq!(std::fs::read_dir(&dest).expect("read").count(), 0);
    }

    /// The other kind of link, which tar spells with its own entry type: it names an existing
    /// member rather than a path on the host, and is refused by the same rule for the same reason —
    /// where its bytes end up is not decided by where the member goes.
    #[test]
    fn a_hard_link_member_refuses_the_archive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hardlinked.tar.gz");
        {
            let file = File::create(&path).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut program = tar::Header::new_gnu();
            program.set_size(11);
            program.set_mode(0o755);
            program.set_cksum();
            builder
                .append_data(&mut program, "app/bin/fluent-bit", &b"the-program"[..])
                .expect("append");
            let mut link = tar::Header::new_gnu();
            link.set_size(0);
            link.set_mode(0o644);
            link.set_entry_type(tar::EntryType::Link);
            builder
                .append_link(&mut link, "app/lib/second-name", "app/bin/fluent-bit")
                .expect("append the hard link");
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }
        let dest = dest(dir.path());

        let err = extract_tree_tar_gz(&path, Path::new("bin/fluent-bit"), &dest)
            .expect_err("must be refused");

        assert!(err.contains("not a file or a directory"), "{err}");
        assert_eq!(std::fs::read_dir(&dest).expect("read").count(), 0);
    }

    /// The member bound (ADR-0023). An archive of a hundred thousand empty files is not an agent,
    /// and the count refuses it in the listing pass — before any path is turned into a write.
    #[test]
    fn an_archive_of_too_many_members_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("many.tar.gz");
        {
            let file = File::create(&path).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for i in 0..=MAX_TREE_MEMBERS {
                let mut header = tar::Header::new_gnu();
                header.set_size(0);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("app/lib/plugin-{i}.so"), &b""[..])
                    .expect("append");
            }
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }
        let dest = dest(dir.path());

        let err = extract_tree_tar_gz(&path, Path::new("bin/fluent-bit"), &dest)
            .expect_err("must be refused");

        assert!(err.contains("more than"), "{err}");
        assert_eq!(
            std::fs::read_dir(&dest).expect("read").count(),
            0,
            "the count is reached before anything is written"
        );
    }

    /// The byte bound is across the *whole* tree, not per member: three members no one of which is
    /// large enough to refuse on its own still stop at the budget. What was written by then is the
    /// caller's staging directory, which it removes — the point here is that the copy stopped.
    #[test]
    fn a_tree_that_outgrows_the_total_budget_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = tar_gz_at(
            &dir.path().join("fat.tar.gz"),
            &[
                ("app/bin/fluent-bit", &[0u8; 2048]),
                ("app/lib/libcrypto.so.3", &[0u8; 2048]),
                ("app/lib/libssl.so.3", &[0u8; 2048]),
            ],
        );
        let dest = dest(dir.path());

        let err = extract_tree_tar_gz_within(&archive, Path::new("bin/fluent-bit"), &dest, 4096)
            .expect_err("past the total limit");

        assert!(err.contains("limit"), "{err}");
        // No member is anywhere near the budget on its own, so a per-member bound would have let
        // the whole tree through.
        extract_tree_tar_gz_within(&archive, Path::new("bin/fluent-bit"), &dest, 16 * 1024)
            .expect("the same tree fits in a budget that admits all of it");
    }

    /// A `.7z` carries neither tar's entry types nor its modes, so the same two refusals are read
    /// out of Windows attributes instead: a reparse point is a link by another name, and an
    /// anti-item is a deletion wearing a member's clothes. Both refuse the whole archive.
    #[test]
    fn a_7z_member_that_is_a_link_or_an_anti_item_refuses_the_archive() {
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        let dir = tempfile::tempdir().expect("tempdir");

        let write = |path: &Path, mark: fn(&mut sevenz_rust2::ArchiveEntry)| {
            let mut writer = sevenz_rust2::ArchiveWriter::create(path).expect("create");
            writer
                .push_archive_entry(
                    sevenz_rust2::ArchiveEntry::new_file("app/bin/fluent-bit"),
                    Some(std::io::Cursor::new(&b"the-program"[..])),
                )
                .expect("push the program");
            let mut suspect = sevenz_rust2::ArchiveEntry::new_file("app/lib/passwd");
            mark(&mut suspect);
            writer
                .push_archive_entry(suspect, None::<std::io::Cursor<&[u8]>>)
                .expect("push the suspect member");
            writer.finish().expect("finish");
        };

        let linked = dir.path().join("linked.7z");
        write(&linked, |entry| {
            entry.has_stream = false;
            entry.has_windows_attributes = true;
            entry.windows_attributes = FILE_ATTRIBUTE_REPARSE_POINT;
        });
        let dest = dest(dir.path());
        let err = extract_tree_7z(&linked, Path::new("bin/fluent-bit"), &dest, None)
            .expect_err("a link must be refused");
        assert!(err.contains("holds a link"), "{err}");
        assert_eq!(std::fs::read_dir(&dest).expect("read").count(), 0);

        let anti = dir.path().join("anti.7z");
        write(&anti, |entry| {
            entry.has_stream = false;
            entry.is_anti_item = true;
        });
        let err = extract_tree_7z(&anti, Path::new("bin/fluent-bit"), &dest, None)
            .expect_err("an anti-item must be refused");
        assert!(err.contains("anti-item"), "{err}");
        assert_eq!(std::fs::read_dir(&dest).expect("read").count(), 0);
    }

    /// Both ways `program_path` can fail to name one member, each answered with what the operator
    /// needs to fix it.
    #[test]
    fn no_match_and_an_ambiguous_match_are_both_refused_by_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dest(dir.path());

        let archive = release(dir.path(), "fluent-bit-3.1.0");
        let err = extract_tree_tar_gz(&archive, Path::new("bin/fluentbit"), &dest)
            .expect_err("no such member");
        assert!(err.contains("bin/fluentbit"), "{err}");
        assert!(
            err.contains("bin/fluent-bit"),
            "names what it does hold: {err}"
        );

        let ambiguous = tar_gz_at(
            &dir.path().join("two.tar.gz"),
            &[
                ("a/bin/fluent-bit", b"one"),
                ("b/bin/fluent-bit", b"another"),
            ],
        );
        let err = extract_tree_tar_gz(&ambiguous, Path::new("bin/fluent-bit"), &dest)
            .expect_err("ambiguous");
        assert!(err.contains("matches 2 members"), "{err}");
        assert!(
            err.contains("a/bin/fluent-bit") && err.contains("b/bin/fluent-bit"),
            "{err}"
        );
    }

    /// A tar carries modes, and a tree needs them: a helper beside the program is executable only
    /// if the archive said so.
    #[cfg(unix)]
    #[test]
    fn a_tree_keeps_the_modes_the_archive_carried() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("modes.tar.gz");
        {
            let file = File::create(&path).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (name, mode, content) in [
                ("app/bin/fluent-bit", 0o755, &b"the-program"[..]),
                ("app/etc/parsers.conf", 0o644, &b"parsers"[..]),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(mode);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, content)
                    .expect("append");
            }
            builder
                .into_inner()
                .expect("finish tar")
                .finish()
                .expect("finish gzip");
        }
        let dest = dest(dir.path());

        extract_tree_tar_gz(&path, Path::new("bin/fluent-bit"), &dest).expect("unpack");

        let mode = |p: &str| {
            std::fs::metadata(dest.join(p))
                .expect("stat")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode("bin/fluent-bit"), 0o755);
        assert_eq!(mode("etc/parsers.conf"), 0o644);
    }

    /// The encrypted case: a whole tree, unreadable without the key wherever it was stored.
    #[test]
    fn an_encrypted_7z_tree_lands_whole_with_the_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = seven_z(
            dir.path(),
            &[
                ("fluent-bit-3.1.0/bin/fluent-bit", b"the-program"),
                ("fluent-bit-3.1.0/lib/libcrypto.so.3", b"a-shared-object"),
            ],
            Some("s3cret"),
        );
        let dest = dest(dir.path());

        let summary = extract_tree_7z(&archive, Path::new("bin/fluent-bit"), &dest, Some("s3cret"))
            .expect("unpack the tree");

        assert_eq!(summary.files, 2);
        assert_eq!(
            std::fs::read(dest.join("lib/libcrypto.so.3")).expect("the shared object"),
            b"a-shared-object"
        );

        // The wrong key opens nothing, and writes nothing.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).expect("create");
        assert!(
            extract_tree_7z(&archive, Path::new("bin/fluent-bit"), &other, Some("wrong")).is_err()
        );
        assert_eq!(std::fs::read_dir(&other).expect("read").count(), 0);
    }
}
