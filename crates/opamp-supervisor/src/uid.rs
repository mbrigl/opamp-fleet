//! The Agent's OpAMP **Instance UID**: 16 bytes that stay stable across restarts.
//!
//! The specification requires the instance UID to be 16 bytes and to remain unchanged for the lifetime
//! of the Agent, so the Server recognises a restarted Agent as the same one rather than registering a
//! new fleet member on every bounce. We therefore persist it to a file (the same reason the sidecar
//! keeps its supervisor storage on a named volume, ADR-0003) and generate a fresh one only when none
//! exists yet.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Loads the 16-byte instance UID from `path`, generating and persisting a new one if the file does
/// not exist or does not hold exactly 16 bytes.
pub fn load_or_create(path: &Path) -> io::Result<[u8; 16]> {
    match fs::read(path) {
        Ok(bytes) if bytes.len() == 16 => {
            let mut uid = [0u8; 16];
            uid.copy_from_slice(&bytes);
            Ok(uid)
        }
        // A file that is not 16 bytes is corrupt; replace it rather than send a malformed UID.
        Ok(_) => persist_new(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => persist_new(path),
        Err(e) => Err(e),
    }
}

fn persist_new(path: &Path) -> io::Result<[u8; 16]> {
    let uid = generate();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, uid)?;
    Ok(uid)
}

/// Generates a **UUIDv7** instance UID: a 48-bit big-endian Unix-millisecond timestamp followed by
/// random bits, with the version (7) and variant (RFC 4122) fields set. Time-ordering is what the spec
/// asks for; the randomness comes from the OS CSPRNG (`/dev/urandom`, always present on the Linux Dev
/// Container target), so we need no UUID crate.
fn generate() -> [u8; 16] {
    let mut uid = [0u8; 16];
    let mut urandom = fs::File::open("/dev/urandom").expect("open /dev/urandom");
    urandom
        .read_exact(&mut uid)
        .expect("read 16 bytes from /dev/urandom");

    // Bytes 0..6: Unix time in milliseconds, big-endian (UUIDv7 `unix_ts_ms`).
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    uid[0..6].copy_from_slice(&millis.to_be_bytes()[2..8]);
    // Byte 6 high nibble: version 7. Byte 8 top two bits: variant 0b10.
    uid[6] = 0x70 | (uid[6] & 0x0F);
    uid[8] = 0x80 | (uid[8] & 0x3F);
    uid
}

/// Formats a 16-byte instance UID as a canonical UUID string (`8-4-4-4-12`), so a UUIDv7 shows as a
/// UUID rather than a bare hex blob. A value that is not 16 bytes falls back to plain hex.
pub fn format(uid: &[u8]) -> String {
    let hex = hex::encode(uid);
    if hex.len() == 32 {
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    } else {
        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(tag: &str) -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("opamp-uid-{}-{}-{}", std::process::id(), tag, n))
    }

    #[test]
    fn generated_uid_is_a_valid_uuidv7() {
        let uid = generate();
        // Version 7 in the high nibble of byte 6.
        assert_eq!(uid[6] >> 4, 0x7, "version must be 7");
        // Variant 0b10 in the top two bits of byte 8.
        assert_eq!(uid[8] >> 6, 0b10, "variant must be RFC 4122");
        // The timestamp bytes are non-zero for any real clock (guards against an all-zero UID).
        assert_ne!(&uid[0..6], &[0u8; 6]);
    }

    #[test]
    fn format_renders_a_canonical_uuid() {
        let uid = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0x7c, 0xde, 0x8f, 0x01, 0x23, 0x45, 0x67, 0x89,
            0xab, 0xcd,
        ];
        assert_eq!(super::format(&uid), "01234567-89ab-7cde-8f01-23456789abcd");
    }

    #[test]
    fn generates_then_reloads_the_same_uid() {
        let path = temp_path("stable");
        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();
        assert_eq!(first, second, "a persisted UID must be stable across loads");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn replaces_a_corrupt_uid_file() {
        let path = temp_path("corrupt");
        fs::write(&path, b"too short").unwrap();
        let uid = load_or_create(&path).unwrap();
        assert_eq!(uid.len(), 16);
        // And it now round-trips.
        assert_eq!(load_or_create(&path).unwrap(), uid);
        let _ = fs::remove_file(&path);
    }
}
