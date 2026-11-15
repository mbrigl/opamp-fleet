# Ticket: A kind reports its version as the tool names its Package

## What to build

`fleet_core::version::written_as_semver` (`crates/fleet-core/src/version.rs`, near line 146)
says that `package fetch` names a Package by it "and a kind whose program prints such a number
reports its version by it, so the two agree". Only the tool calls it
(`crates/fleet-tools/src/fetch.rs`, near line 253). The Client's probe, `find_semver`
(`crates/fleet-agent/src/supervisor/process.rs`, near line 1888), accepts only a strict SemVer
token, and the hook `VersionProbe::parse` is `None` in every kind (`collector.rs`,
`command.rs`). An upstream `1.19` is uploaded as Package `1.19.0` while the probe reports no
version.

Either the probe folds a numeric token through `written_as_semver` (or a kind sets `parse` to
it), or the doc line is corrected and the unused `parse` field dropped. First check whether a
kind on `main` prints a version that is not SemVer; that was not verified.

## Acceptance criteria

- [ ] Given a kind whose program prints `1.19`, when the Client probes it, then it reports
  `1.19.0`, the version `package fetch` names its Package by; or the doc no longer claims so.
- [ ] Given the change, when `VersionProbe` is read, then it has no field every kind leaves
  `None`.

## Read first

- Touches: `crates/fleet-core/src/version.rs`, `crates/fleet-agent/src/supervisor/process.rs`,
  `crates/fleet-agent/src/supervisor/collector.rs`, `crates/fleet-agent/src/supervisor/command.rs`
- ADR-0035 clause 6, ADR-0054 (what an Agent reports), ADR-0051.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
