//! Artifact bytes, kept by their SHA-256 (ADR-0029 clause 13): the blob port.
//!
//! A Package entry names its artifact by the hash of its bytes and nothing else, so bytes held by
//! two entries are stored once, and a blob's name is its own integrity check. Which blobs are still
//! needed is the package store's to say: it deletes a blob when the last entry naming it goes, and
//! every blob no entry names when it opens.
//!
//! The filesystem adapter ([`FsBlobStore`](crate::fs::FsBlobStore)) keeps one file per blob in
//! `blobs/` under `data_dir`, and hashes every one of them again when it opens. An object store
//! would be another adapter of the same four operations.

use std::io::Read;

/// The blob port (ADR-0029 clause 13). A hash is the lowercase hex SHA-256 of the bytes.
pub trait BlobStore: Send + Sync {
    /// Stores the bytes `bytes` yields, hashing them on the way, and answers their hash and size.
    /// Bytes already stored are stored once. A stream that fails stores nothing.
    ///
    /// # Errors
    /// Returns an error when the stream fails or the bytes cannot be stored.
    fn put(&self, bytes: &mut dyn Read) -> Result<(String, u64), String>;

    /// The bytes of one blob, as a stream.
    ///
    /// # Errors
    /// Returns an error when no such blob is stored or it cannot be read.
    fn get(&self, sha256: &str) -> Result<Box<dyn Read + Send>, String>;

    /// Whether a blob of that hash is stored. A name that is no hash names no blob.
    ///
    /// # Errors
    /// Returns an error when the store cannot be asked: the caller must not take that for an
    /// answer either way.
    fn exists(&self, sha256: &str) -> Result<bool, String>;

    /// Deletes one blob; one that is not stored is not an error.
    ///
    /// # Errors
    /// Returns an error when it cannot be deleted.
    fn delete(&self, sha256: &str) -> Result<(), String>;

    /// Every stored blob as `(hash, size)` — what the package store reads once when it opens, to
    /// know each entry's size and to delete what no entry names.
    ///
    /// # Errors
    /// Returns an error when the store cannot be listed.
    fn list(&self) -> Result<Vec<(String, u64)>, String>;

    /// Deletes what uploads that were cut off left behind, of every writer that is no longer live
    /// (ADR-draft-the-servers-state-and-its-stores clause 16), and answers how many objects it
    /// deleted: what every sweep runs, under the blob store's lock. A store that stages no upload
    /// apart from its blobs, or whose one Server cleans up only at startup, deletes nothing here,
    /// which the default does.
    ///
    /// # Errors
    /// Returns an error when the leftovers cannot be listed or deleted.
    fn clean_staging(&self) -> Result<usize, String> {
        Ok(0)
    }
}

/// A hold on the blob store's lock
/// (ADR-draft-the-servers-state-and-its-stores clause 14), released when it is dropped. A store
/// that is its one Server's alone holds nothing.
pub struct BlobHold {
    #[allow(dead_code)]
    held: Option<Box<dyn Send>>,
    /// Set once the lock is gone while the hold lives, as when its session ended.
    lost: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The `statement_timeout` and `lock_timeout` of the hold's session, where it has one.
    #[cfg(test)]
    pub(crate) settings: Option<(String, String)>,
}

impl BlobHold {
    /// A hold on nothing: what a store with no other writer answers.
    #[must_use]
    pub fn none() -> Self {
        BlobHold {
            held: None,
            lost: None,
            #[cfg(test)]
            settings: None,
        }
    }

    /// A hold that `held` keeps until it is dropped, `lost` set once the lock is gone before.
    #[must_use]
    pub fn of(held: Box<dyn Send>, lost: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        BlobHold {
            held: Some(held),
            lost: Some(lost),
            #[cfg(test)]
            settings: None,
        }
    }

    /// The hold, its session's settings noted: what the tests read the wait from.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_settings(mut self, settings: (String, String)) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Whether the lock is gone while the hold lives.
    #[must_use]
    pub fn lost(&self) -> bool {
        self.lost
            .as_ref()
            .is_some_and(|lost| lost.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// Whether `value` is a blob's name: 64 lowercase hex characters. Nothing else names a blob, so
/// nothing else can name a path.
#[must_use]
pub fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
