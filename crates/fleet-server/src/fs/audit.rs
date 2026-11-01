//! The audit record on the filesystem (ADR-0023): the default adapter behind
//! [`AuditStore`](crate::audit_log::AuditStore).

use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use crate::audit_log::{line_hash, AuditStore, Held, Tail};

/// `audit-<first seq>.jsonl` files in a directory only the Server's account can enter, each
/// owner-only, appended to whole lines at a time.
pub struct FsAuditStore {
    dir: PathBuf,
    current: Option<(std::fs::File, u64)>,
}

impl FsAuditStore {
    /// Opens the store in the data directory `root`, creating `audit/` owner-only (ADR-0029). The
    /// newest file is continued.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be created or read, or holds anything but audit
    /// files.
    pub fn open(root: &Path) -> Result<Self, String> {
        let dir = root.join(super::AUDIT);
        super::create_private_dir(&dir)?;
        // The directory's own name, durable before the first file in it is (ADR-0023).
        super::sync_dir_of(&dir)?;
        for (name, path) in super::entries(&dir)? {
            if !path.is_file() || seq_in(&name).is_none() {
                return Err(super::unknown(&path));
            }
        }
        let mut store = FsAuditStore { dir, current: None };
        if let Some(newest) = store.files()?.pop() {
            let (mut file, mut size) = open_append(&newest)?;
            // A crash mid-write leaves a last line without its newline; the next entry must start
            // on a line of its own, or it would be glued to the torn one.
            if size > 0 && !ends_with_newline(&newest)? {
                file.write_all(b"\n")
                    .map_err(|e| format!("cannot repair {}: {e}", newest.display()))?;
                size += 1;
            }
            store.current = Some((file, size));
        }
        Ok(store)
    }

    /// The record's files, oldest first.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be read.
    pub fn files(&self) -> Result<Vec<PathBuf>, String> {
        files_in(&self.dir)
    }
}

/// The audit files in `dir`, oldest first — ordered by the sequence number in their name.
///
/// # Errors
/// Returns an error when the directory cannot be read.
pub fn files_in(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter_map(|path| {
            let seq = seq_in(path.file_name()?.to_str()?)?;
            Some((seq, path))
        })
        .collect();
    files.sort();
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

/// The sequence number an audit file's name carries: `audit-<first seq>.jsonl`.
fn seq_in(name: &str) -> Option<u64> {
    name.strip_prefix("audit-")?
        .strip_suffix(".jsonl")?
        .parse()
        .ok()
}

fn open_append(path: &Path) -> Result<(std::fs::File, u64), String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true).read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .len();
    // A new file's name is durable only once its directory is: without that, a crash can take the
    // file and every entry synced into it (ADR-0023).
    if size == 0 {
        super::sync_dir_of(path)?;
    }
    Ok((file, size))
}

fn ends_with_newline(path: &Path) -> Result<bool, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    file.seek(SeekFrom::End(-1))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(last[0] == b'\n')
}

/// The non-empty lines at the end of `path`.
fn tail_lines(path: &Path) -> Result<Vec<String>, String> {
    Ok(last_chunk(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// The last non-empty line of `path`, read from its end.
fn last_line(path: &Path) -> Result<Option<String>, String> {
    Ok(tail_lines(path)?.pop())
}

fn last_chunk(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let size = file
        .metadata()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?
        .len();
    // An entry is far below this; the tail of one is all that is ever needed.
    let start = size.saturating_sub(1024 * 1024);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The `seq` an entry line carries, if the line is a whole entry.
fn seq_of(line: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("seq")?
        .as_u64()
}

/// The number in an audit file's name.
fn first_seq_of(path: &Path) -> u64 {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("audit-"))
        .and_then(|name| name.strip_suffix(".jsonl"))
        .and_then(|seq| seq.parse().ok())
        .unwrap_or(0)
}

/// Where the record in `dir` left off, read and nothing else: the end of the newest file that
/// holds a line.
///
/// # Errors
/// Returns an error when the files cannot be read.
pub fn tail_in(dir: &Path) -> Result<Option<Tail>, String> {
    // The newest file that holds a line: a rotation that failed its first write leaves an
    // empty one behind, and the chain goes on from the file before it.
    for file in files_in(dir)?.iter().rev() {
        let lines = tail_lines(file)?;
        let Some(last_line) = lines.last().cloned() else {
            continue;
        };
        // The highest seq near the end; a torn last line carries none, and the entries before
        // it still say where the count stands. A file whose tail holds no whole entry counts
        // on from its name.
        let last_seq = lines
            .iter()
            .filter_map(|line| seq_of(line))
            .max()
            .unwrap_or_else(|| first_seq_of(file).saturating_sub(1) + lines.len() as u64);
        let torn = seq_of(&last_line).is_none() || !ends_with_newline(file)?;
        let last_whole = if torn {
            last_whole_in(dir, file, &lines)?
        } else {
            None
        };
        return Ok(Some(Tail {
            last_line,
            last_seq,
            torn,
            last_whole,
        }));
    }
    Ok(None)
}

/// The hash of the last whole entry of the record up to the end of `file`, whose tail is `lines`:
/// in `file`, or else the last line of the newest file before it that holds one.
fn last_whole_in(dir: &Path, file: &Path, lines: &[String]) -> Result<Option<String>, String> {
    if let Some(line) = lines.iter().rev().find(|line| seq_of(line).is_some()) {
        return Ok(Some(line_hash(line)));
    }
    for earlier in files_in(dir)?
        .iter()
        .rev()
        .skip_while(|earlier| *earlier != file)
        .skip(1)
    {
        if let Some(line) = last_line(earlier)? {
            return Ok(seq_of(&line).is_some().then(|| line_hash(&line)));
        }
    }
    Ok(None)
}

/// The non-empty lines of `files` in order, each with its place (`<file>:<line number>`), read as
/// bytes: a line that is valid UTF-8 as it is, and one that is not, which only a torn piece cut
/// inside a character can be, decoded the lossy way the store reads its tail and hashes it.
///
/// # Errors
/// Returns an error when a file cannot be read, naming it.
pub fn read_lines(files: &[PathBuf]) -> Result<Vec<(String, String)>, String> {
    let mut placed = Vec::new();
    for file in files {
        let bytes =
            std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
        let name = file.display().to_string();
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = String::from_utf8_lossy(line).into_owned();
            if !line.trim().is_empty() {
                placed.push((format!("{name}:{}", index + 1), line));
            }
        }
    }
    Ok(placed)
}

/// The audit record in `dir` as [`verify`](crate::audit_log::verify) finds it: how many files it
/// read, and the chain's [`Held`] or its first break.
///
/// # Errors
/// Returns an error when the files cannot be read.
pub fn verify_in(dir: &Path) -> Result<(usize, Result<Held, String>), String> {
    let files = files_in(dir)?;
    let lines = read_lines(&files)?;
    Ok((
        files.len(),
        crate::audit_log::verify(
            lines
                .iter()
                .map(|(place, line)| (place.clone(), line.as_str())),
        ),
    ))
}

impl AuditStore for FsAuditStore {
    fn tail(&mut self) -> Result<Option<Tail>, String> {
        tail_in(&self.dir)
    }

    fn append(&mut self, seq: u64, lines: &[&str]) -> Result<(), String> {
        if self.current.is_none() {
            self.current = Some(open_append(&self.dir.join(format!("audit-{seq}.jsonl")))?);
        }
        let (file, size) = self.current.as_mut().expect("just opened");
        // One write for the lines and their newlines, so a crash leaves whole lines and at most a
        // torn last one, never two entries run together; one sync for all of them.
        let mut bytes = Vec::with_capacity(lines.iter().map(|line| line.len() + 1).sum());
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        if let Err(e) = file.write_all(&bytes).and_then(|()| file.sync_data()) {
            // A write that failed partway leaves a fragment, and lines that were not synced are
            // not written; cut the file back to the last whole line before them, so the next entry
            // does not land on them.
            let _ = file.set_len(*size);
            return Err(format!("cannot append to the audit record: {e}"));
        }
        *size += bytes.len() as u64;
        Ok(())
    }

    fn current_bytes(&self) -> u64 {
        self.current.as_ref().map_or(0, |(_, size)| *size)
    }

    fn rotate(&mut self, keep: usize) -> Result<Vec<(String, String)>, String> {
        self.current = None;
        let mut files = self.files()?;
        let mut deleted = Vec::new();
        while files.len() >= keep.max(1) {
            let oldest = files.remove(0);
            let last = last_line(&oldest)?.unwrap_or_default();
            if let Err(e) = std::fs::remove_file(&oldest) {
                // What was deleted already is still told; the rest stays for the next rotation.
                tracing::error!(file = %oldest.display(), error = %e, "cannot delete an audit file");
                break;
            }
            let name = oldest
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            deleted.push((name, last));
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies: ADR-0023
    #[test]
    fn the_record_is_owner_only_and_continues_where_it_ended() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("audit");
        {
            let mut store = FsAuditStore::open(dir.path()).expect("open");
            store
                .append(1, &["{\"seq\":1}", "{\"seq\":2}"])
                .expect("append");
        }
        let mut store = FsAuditStore::open(dir.path()).expect("reopen");
        assert_eq!(
            store.tail().expect("tail"),
            Some(Tail {
                last_line: "{\"seq\":2}".to_string(),
                last_seq: 2,
                torn: false,
                last_whole: None,
            })
        );
        store.append(3, &["{\"seq\":3}"]).expect("append");
        assert_eq!(
            store.files().expect("files").len(),
            1,
            "the newest file is continued"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file = &store.files().expect("files")[0];
            let mode = std::fs::metadata(file).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = std::fs::metadata(&root).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        let deleted = store.rotate(1).expect("rotate");
        assert_eq!(
            deleted,
            vec![("audit-1.jsonl".to_string(), "{\"seq\":3}".to_string())]
        );
        store.append(4, &["{\"seq\":4}"]).expect("append");
        assert_eq!(
            store.files().expect("files"),
            vec![root.join("audit-4.jsonl")]
        );
    }

    /// A record a crash cut mid-line goes on from its last whole entry, on a line of its own, and
    /// says it was torn; an empty newest file is passed over.
    /// Verifies: ADR-0023
    #[test]
    fn a_torn_record_goes_on_from_its_last_whole_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("audit");
        std::fs::create_dir_all(&root).expect("dir");
        std::fs::write(
            root.join("audit-1.jsonl"),
            "{\"seq\":1}\n{\"seq\":2}\n{\"seq\":3,\"pr",
        )
        .expect("write");
        std::fs::write(root.join("audit-9.jsonl"), "").expect("an empty newest file");
        let mut store = FsAuditStore::open(dir.path()).expect("open");
        let tail = store.tail().expect("tail").expect("a tail");
        assert_eq!(tail.last_seq, 2);
        assert!(tail.torn);
        assert_eq!(tail.last_line, "{\"seq\":3,\"pr");
        store.rotate(16).expect("rotate");
        store.append(3, &["{\"seq\":3}"]).expect("append");
        let mut reopened = FsAuditStore::open(dir.path()).expect("reopen");
        assert_eq!(reopened.tail().expect("tail").expect("tail").last_seq, 3);

        // The newest file torn: it is continued on a line of its own.
        let other = dir.path().join("other");
        std::fs::create_dir_all(other.join("audit")).expect("dir");
        std::fs::write(
            other.join("audit/audit-1.jsonl"),
            "{\"seq\":1}\n{\"seq\":2,\"pr",
        )
        .expect("write");
        let mut store = FsAuditStore::open(&other).expect("open");
        store.append(2, &["{\"seq\":2}"]).expect("append");
        let text = std::fs::read_to_string(other.join("audit/audit-1.jsonl")).expect("read");
        assert_eq!(text, "{\"seq\":1}\n{\"seq\":2,\"pr\n{\"seq\":2}\n");
    }

    /// A crash that cuts a line inside a two-byte character leaves bytes that are not UTF-8. The
    /// record still verifies: the torn line is read the lossy way the store hashed it when it
    /// repaired the record, and the entries after the repair hold.
    /// Verifies: ADR-0023, ADR-draft-an-append-only-audit-record
    #[test]
    fn a_line_torn_inside_a_character_verifies_as_the_store_hashed_it() {
        use crate::audit::{Audit as _, Entry};
        use crate::audit_log::{AuditLog, Limits};

        let record = |root: &Path, user: &str| {
            let audit = AuditLog::start(
                Box::new(FsAuditStore::open(root).expect("open")),
                Limits::default(),
                std::sync::Arc::new(crate::clock::SystemClock),
            )
            .expect("audit");
            audit
                .record(Entry::new("operator.act", "ok").with("user", user.to_string()))
                .expect("record");
            assert!(audit.drain(std::time::Duration::from_secs(10)));
        };
        let dir = tempfile::tempdir().expect("tempdir");
        record(dir.path(), "a");
        record(dir.path(), "jürgen");
        let file = files_in(&dir.path().join("audit"))
            .expect("files")
            .pop()
            .expect("a file");
        let bytes = std::fs::read(&file).expect("read");
        // Cut after the first byte of "ü".
        let cut = bytes
            .windows(2)
            .position(|pair| pair == "ü".as_bytes())
            .expect("the character")
            + 1;
        std::fs::write(&file, &bytes[..cut]).expect("tear");
        assert!(std::str::from_utf8(&std::fs::read(&file).expect("read")).is_err());
        record(dir.path(), "b");
        let (files, held) = verify_in(&dir.path().join("audit")).expect("read");
        assert_eq!(files, 1);
        let held = held.expect("the chain holds");
        assert_eq!(held.entries, 3, "a, the broken mark and b");
        assert!(held.unlinked.is_empty(), "{:?}", held.unlinked);
    }
}
