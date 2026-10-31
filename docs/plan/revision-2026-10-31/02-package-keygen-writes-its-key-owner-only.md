# Ticket: `package keygen` writes the signing key owner-only from the moment it exists

## What to build

`opamp-fleetctl package keygen` writes the package-signing private key with a plain write and
restricts it to the owner afterwards, so the key is readable by others between the two steps, and an
existing key file is overwritten without a word. `pki init` writes the same
`package-signing-key.pk8` with `create_new` and mode 0600 from creation. The tool has two
private-key writers and the weaker one guards the operator's most valuable key.

Build one owner-only writer in `fleet-tools` that both commands use: the file is created new with
mode 0600 on Unix and refuses to replace an existing key unless the operator asks for it.

## Acceptance criteria

- [ ] Given no key file, when `package keygen` runs, then the file is created with mode 0600 and
  never exists with a wider mode.
- [ ] Given an existing key file, when `package keygen` runs without an explicit overwrite, then it
  refuses and leaves the file unchanged.
- [ ] Given `pki init` and `package keygen`, when either writes a private key, then both go through
  the same writer.

## Read first

- ADR-0032 and ADR-0014 (header, Decision, Enforcement).
- `crates/fleet-tools/src/package.rs` `write_private_key`; `crates/fleet-tools/src/pki.rs`
  `write_file`.
- `docs/HARDENING.md`, where the finding may also belong.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. A refusal to overwrite changes what an operator sees; the human decides that
flag.
