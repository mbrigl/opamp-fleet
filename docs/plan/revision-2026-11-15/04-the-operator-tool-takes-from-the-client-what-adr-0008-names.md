# Ticket: The operator tool takes from the Client only what ADR-0008 names

## What to build

ADR-0008 clause 22 names what `fleet-tools` uses of the Client: `fleet_agent::archive`, beside
`opamp::tls` and the core. ADR-0053 adds `fleet_agent::tls::pkcs12_identity`. The tool also
calls `fleet_agent::config_init::read_password_file` and `fleet_agent::nofollow::read_privileged`
(`crates/fleet-tools/src/pki.rs`, near lines 797 and 824), which no ADR names, and no test
checks the arrow (`crates/fleet-core/tests/dependency_direction.rs` gives the tool's modules
roles only).

Two changes close it:

- `pkcs12_identity` reads no file any more and is used by an end and by the tool, so it moves
  beside `fleet_core::pkcs12::file` (ADR-0008 clause 16).
- The privileged read's context is a data clump: `read_privileged(path, roots, state_dir,
  named_by, limit)` in `crates/fleet-agent/src/nofollow.rs`, and in `config_init.rs`
  `read_certificates(path, roots, state_dir, named_by)`, `read_pkcs12(path, offered, (roots,
  state_dir))` and `read_password_file(path, flag, roots)`, repeated in
  `service/enrol.rs`'s `read_once`. One value holds the account's roots, state directory and
  naming, with one read method, and the tool reaches it through one narrow function.

What the tool may take from the Client is then either named by a superseding ADR through
`propose-adr`, which the human decides, or reduced to what clause 22 names. A test lists the
`fleet_agent::` paths `fleet-tools` uses against an allowlist.

## Acceptance criteria

- [ ] Given the change, when `fleet-tools` is searched for `fleet_agent::`, then every path is
  one an accepted ADR names, and a test fails on any other.
- [ ] Given a bootstrap PKCS#12 file, when the Client's install and `pki p12-extract` read it,
  then both read it through `fleet-core`.
- [ ] Given the existing enrolment and `privileged_steps_follow_no_link` tests, when the change
  lands, then they pass.

## Read first

- Touches: `crates/fleet-tools/src/pki.rs`, `crates/fleet-agent/src/nofollow.rs`,
  `crates/fleet-agent/src/config_init.rs`, `crates/fleet-agent/src/service/enrol.rs`,
  `crates/fleet-agent/src/tls.rs`, `crates/fleet-core/src/pkcs12.rs`,
  `crates/fleet-core/tests/dependency_direction.rs`
- ADR-0008 clauses 16, 17 and 22; ADR-0053; ADR-0021 (the administrator's paths).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero. It may need a superseding ADR, which only the human decides.
