# Ticket: The Dev Container lints the Client for Windows

## What to build

`scripts/check-client-platform.sh` lints the Client for Windows through cargo-xwin. Without
cargo-xwin or the target `x86_64-pc-windows-msvc` it says so and skips, and the pre-push hook
passes. The Dev Container installs `llvm` for that check, and its comment calls the check "not
optional in practice". It installs neither cargo-xwin nor the target, so in the container the
check always skips. `scripts/check-all.sh` printed on 2026-11-27:

> Skipped: linting the Client for Windows needs cargo-xwin and its target

After this ticket, a Dev Container built from `.devcontainer/devcontainer.json` carries cargo-xwin
and the target. `scripts/check-all.sh` there runs the Windows lint instead of skipping it, and a
`#[cfg(unix)]` helper that a Windows build does not use fails before the push, not in CI.

Evidence: ff170c6de, in PR #231. The Windows build of the Client's library tests failed in CI on
two helpers in `crates/fleet-agent/src/supervisor/host.rs` that only Unix tests use. Every local
run of `scripts/check-all.sh` on the branch had skipped the lint that finds them.

## Acceptance criteria

- [ ] Given a Dev Container built from `.devcontainer/devcontainer.json`, when
      `scripts/check-all.sh` runs, then "Client on its platform" lints the Client for Windows and
      does not print "Skipped".
- [ ] Given a test-only helper under `#[cfg(test)]` that only a `#[cfg(unix)]` test calls, when
      `scripts/check-client-platform.sh` runs in that container, then it fails on the unused
      function.
- [ ] Given `scripts/check-devcontainer.sh` and its self-test, then both pass with the new tools.

## Read first

- Touches: `.devcontainer/devcontainer.json`, `scripts/check-client-platform.sh`,
  `scripts/check-devcontainer.sh`, README.md → "Build, Test & Run"
- ADR: what `scripts/sensor-binding-adrs.sh --sections` lists with
  `--path .devcontainer/devcontainer.json`; DEV-six-crates-tokio-axum for the toolchain.
- Golden path: how the container installs `shellcheck` and `llvm` in `postCreateCommand`.

## Blocked by

- None (can start immediately)

## Mode

with the human: cargo-xwin downloads the Microsoft CRT and Windows SDK, under Microsoft's licence
terms, which the human accepts or declines for the project.
DEV-maintainer-decides-agents-implement §5 caps open unattended agent work at zero, too.
