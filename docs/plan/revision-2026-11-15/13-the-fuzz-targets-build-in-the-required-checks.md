# Ticket: The fuzz targets build in the required checks

## What to build

`fuzz/` is a workspace of its own. `check-rust.sh` builds `--workspace` only, and
`check-deny.sh` resolves `fuzz/Cargo.toml` without compiling it. The one build is
`cargo +nightly fuzz` in `.github/workflows/fuzz.yml`. That workflow is not a required check. It
runs on pull requests touching `crates/**` or `fuzz/**` and daily, and has no `timeout-minutes`.
Locally `scripts/check-fuzz.sh` skips without cargo-fuzz and a nightly toolchain, which neither
the Dev Container nor `install_tools.sh` installs, so in `check-all.sh` it prevented nothing. The
`archive_tree` target stopped building when `extract_tree_tar_gz` gained kept links, and only the
non-required Fuzz run saw it (#179, bd1b5bbe1). Q-2 asks for a fuzz target per parser "that runs
in CI".

A stable `cargo check --manifest-path fuzz/Cargo.toml --locked` joins `check-rust.sh` and so the
required Rust job, if the targets compile without libFuzzer's nightly configuration; that was not
verified. The local `check-fuzz.sh` step either gets its toolchain in the Dev Container or leaves
`check-all.sh`, the CI fuzz run staying where fuzzing happens. `fuzz.yml` gets a timeout.

## Acceptance criteria

- [ ] Given a change to a workspace function a fuzz target calls that breaks the target, when
  `check-rust.sh` runs, then it fails.
- [ ] Given the Dev Container, when `check-all.sh` runs, then no fuzz step has a skip as its only
  outcome.
- [ ] Given `fuzz.yml`, when it is read, then its job has a `timeout-minutes`.

## Read first

- Touches: `scripts/check-rust.sh`, `scripts/check-fuzz.sh`, `scripts/check-all.sh`,
  `.github/workflows/fuzz.yml`, `.github/workflows/checks.yml`, `fuzz/Cargo.toml`
- Q-2; ADR-0008 clause 15.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
