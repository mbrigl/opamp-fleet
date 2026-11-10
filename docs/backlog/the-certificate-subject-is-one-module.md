# A certificate's subject is one module, and the Host register a module of its own

**Kind:** idea, structure.

**Observed.** The code that reads and writes a certificate's subject, its OIDs among it, is spread
over `crates/fleet-server/src/bootstrap.rs`, `ca.rs` and `revocation.rs`, and the Host register
lives inside `revocation.rs` (near line 155 on `main`).

**What the old branch had.** Revision ticket 14 of `feat/revision-2026-10-31`, commits 741c6bb3 and
b39e7e11. A module `subject` with `subject/name` held the subject and wrote its OIDs once, its name
functions crate-private, and `revocation/hosts.rs` held the Host register. It was not carried: the
three files have been reworked since, and the move needs a fresh cut.
