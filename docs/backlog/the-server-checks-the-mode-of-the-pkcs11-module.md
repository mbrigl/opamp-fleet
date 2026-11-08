# The Server checks the mode of the PKCS#11 module it loads

**Kind:** hardening.

**Observed.** `[pkcs11] module` names a library that `crates/fleet-server/src/pkcs11.rs` loads into
the Server's process through `cryptoki` (`Pkcs11::new`). The Server checks the mode of its PIN file
and of its other secret files, but not of this library. A module, or a directory above it, that an
account other than root or the Server's own may write lets that account run code inside the
Server, with the token's session at hand.

**Idea.** At startup, refuse a module that others may write, or whose group may write it, as a
secret file is refused, naming `[pkcs11] module`. Check the directories above it the same way.

**From.** The review of stage 3, ticket 12 (2026-11-08).
