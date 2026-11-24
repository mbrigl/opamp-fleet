# Ticket: Drop the convention on where `fleet-tools` keeps its parsers

## What to build

`docs/CONVENTIONS.md` gained one entry since the design revision of 2026-11-19, with 02c604e17:
"A parser of `fleet-tools` that reads network bytes lives in a module that names nothing of the
crate". A rule in prose names the mistake it prevents, or it is a candidate to drop
([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)). This entry names none:

- Its first half is a check already. The fuzz targets `checksum_file` and `release_listing`
  compile `crates/fleet-tools/src/fetch/upstream.rs` by `#[path]`, and the fuzz crate's build
  fails on a `crate::` path there (02c604e17). `scripts/check-rust.sh` checks `fuzz/Cargo.toml`
  on every `scripts/check-all.sh` run, and `scripts/check-fuzz.sh` builds each target in the
  fuzz workflow. For a parser no target includes yet, the check follows only through Q-2, which
  asks a fuzz target of every parser of network bytes: the target's `#[path]` then brings the
  build failure. A convention a check can decide is the check, not an entry
  (`docs/CONVENTIONS.md`, its preamble).
- Its second half, the `#[path]` include, follows from DEV-six-crates-tokio-axum §22 and Q-2.
  `fetch::upstream` and its two fuzz targets show it, the module comment of `fetch::upstream`
  says why, and the overview names it under `fleet-tools`. What the golden path already shows
  needs no entry.

The entry goes. No ADR or criterion cites it.

## Acceptance criteria

- [ ] Given the change, when `docs/CONVENTIONS.md` is read, then it holds no entry on the parsers
  of `fleet-tools`.
- [ ] Given the change, when the tree is searched for the entry's title, then nothing cites it.
- [ ] Given the change, when `scripts/check-all.sh` runs, then it passes.

## Read first

- Touches: `docs/CONVENTIONS.md`
- `docs/ARCHITECTURE.md`, the entry **`fleet-tools`**; `fuzz/fuzz_targets/checksum_file.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: the human decides whether a convention goes, and
DEV-maintainer-decides-agents-implement §5 keeps every part of the system with the human.
