# Ticket: The capability matrix reads all production code

## What to build

`crates/fleet-server/tests/conformance_matrix.rs` (`source`, near line 34) reads each source
file only up to its first `#[cfg(test)]`, wherever it stands. `fleet/held.rs` carries one at
line 265 and `fleet/placement.rs` at line 49, on items among production code, so the matrix
reads neither file past them. A capability declared there is unseen: a row listed as not
implemented then passes against code that declares it. During PR #202 a `#[cfg(test)] mod
fixtures;` at the top of `fleet.rs` cut the root before `SERVER_CAPABILITIES`, and the test
failed loudly; the opposite case fails nothing.

The matrix reads only production modules, and each of them in full but for its items under
`#[cfg(test)]`, wherever they stand. A module declared under `#[cfg(test)]`, such as
`#[cfg(test)] mod fixtures;` in `fleet.rs`, is test code: its file (`fleet/fixtures.rs`) is not
read as production code. A self-test of the reader pins both.

## Acceptance criteria

- [ ] Given a source with a `#[cfg(test)]` item before a capability's declaration, when the
  matrix reads it, then the declaration is found.
- [ ] Given a source whose test module is `#[cfg(test)] mod tests { … }`, when the matrix reads
  it, then nothing inside the module counts as declared.
- [ ] Given a test-only module declared as `#[cfg(test)] mod x;` whose file names a
  capability, when the matrix reads the crate, then the capability does not count as declared.
- [ ] Given the change, when the two matrix tests run on `main`, then they pass.
- [ ] Given the change, when `docs/CONVENTIONS.md` is read, then its entry on putting every
  `#[cfg(test)]` item after the production code of a `fleet` source file is gone.

## Read first

- Touches: `crates/fleet-server/tests/conformance_matrix.rs`, `docs/CONVENTIONS.md`
- `docs/CONFORMANCE.md`, the two capability tables; goal G-12.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
