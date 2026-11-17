//! What the Client and the operator tool share and the Server does not use
//! (ADR-0008 clause 16): the unpackers of a package's artifact and
//! the 7z Unix-mode convention (`archive`), the links a kind's tree package keeps (`tree`), the
//! file operations that follow no link (`nofollow`), the reader of a password file (`password`), and
//! a bootstrap identity opened from a PKCS#12 file (`identity`).
//!
//! An item the Server comes to use moves to `fleet-core`. Every crate names an item here by its
//! `fleet_util::` path; none re-exports one. `pub` means "another target in this workspace reaches
//! it", not a published interface.
//!
//! Not `forbid(unsafe_code)`: `nofollow` makes the `*at` system calls through `libc` on Unix.

pub mod archive;
pub mod identity;
pub mod nofollow;
pub mod password;
pub mod tree;
