# The Server checks its PKCS#11 token and module

**Kind:** hardening.

**Observed.** Two checks are missing where the Server uses a token
(`crates/fleet-server/src/pkcs11.rs`):

- **How a key is protected.** `Token::key` loads a CA key or the audit key by its label and reads
  only its public half. It does not read `CKA_SENSITIVE`, `CKA_EXTRACTABLE` or
  `CKA_NEVER_EXTRACTABLE`. A key imported from the old key file, or one made extractable, is used
  without a word. `opamp-fleetctl pki token-init` makes every key sensitive and never extractable,
  so the gap is a key made by other tools.
- **The mode of the module.** `[pkcs11] module` names a library that the Server loads into its
  process through `cryptoki` (`Pkcs11::new`). The Server checks the mode of its PIN file and of its
  other secret files, but not of this library. A module, or a directory above it, that another
  account may write lets that account run code inside the Server, with the token's session at hand.

**Idea.** At startup, the Server checks both: a key that is extractable, not sensitive, or was
extractable once, named by its label; and a module or a directory above it that others or its group
may write, named by `[pkcs11] module`. A refusal at startup needs a note or an ADR on
ADR-0026; a warning needs neither. Which of the two is the human's.

**Effort.** S.

**From.** The review of stage 3, ticket 12 (2026-11-08); merged into one entry from two by the
human (2026-11-15).
