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
//! - **The archive never chooses a path.** Exactly one member is extracted, to a destination this
//!   Client picked, so a member named `../../etc/cron.d/x` writes to that destination like any
//!   other — there is no traversal to defend against because no archive path is ever used.
//! - **The output is bounded.** Extraction stops at [`MAX_UNPACKED_BYTES`], so an archive that
//!   expands without end fills a temporary file up to that limit and then fails, rather than a
//!   disk.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

/// The largest member this Client will write out of an archive. An agent binary is hundreds of
/// megabytes; anything past this is not a program but a way to fill a disk.
const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

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
    let mut reader = sevenz_rust2::ArchiveReader::open(archive, password).map_err(|e| match e {
        // The two failures an operator actually causes, named as such rather than as a codec
        // complaint: `archive_key` is missing, or it is not the one the archive was packed with.
        sevenz_rust2::Error::PasswordRequired => {
            "the archive is encrypted; set [packages] archive_key in client.toml".to_string()
        }
        sevenz_rust2::Error::MaybeBadPassword(_) => {
            "cannot open the archive — [packages] archive_key is wrong, or the archive is damaged"
                .to_string()
        }
        other => format!("cannot read the archive: {other}"),
    })?;

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
        let path = dir.join("release.tar.gz");
        let file = File::create(&path).expect("create");
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
        path
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
}
