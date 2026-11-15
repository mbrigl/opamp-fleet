# The operator tool takes from the Client only what an ADR names

**Kind:** ADR text, decision taken.

**Decided.** The human chose that a superseding ADR names both functions, with the allowlist test
(2026-11-15). It is proposed after the next normalization, against the restated ADR that carries
ADR-0008 clause 22; the normalization carries the gap unchanged.

**What was ticketed.**

ADR-0008 clause 22 names what `fleet-tools` uses of the Client. It does not name two functions the
tool calls: `fleet_agent::nofollow::read_privileged` and
`fleet_agent::config_init::read_password_file` (`crates/fleet-tools/src/pki.rs`). No test checks
what the tool takes from the Client.

The human decides one of two:

- a superseding ADR through `propose-adr` names both functions; or
- `fleet-tools` stops using them.

Either way, a test lists the `fleet_agent::` paths that `fleet-tools` uses against an allowlist.
`fleet_agent::tls::pkcs12_identity` stays on that list, since ADR-0053 clauses 5 and 24 name it.

This must be settled before the next normalization of the ADR record.

**Effort.** S.

**Its acceptance criteria.**

- [ ] Given the change, when `fleet-tools` is searched for `fleet_agent::`, then every path is one
  an accepted ADR names.
- [ ] Given a new `fleet_agent::` path in `fleet-tools` outside the allowlist, when the tests run,
  then a test fails and names the path.

**Its reading.**

- Touches: `crates/fleet-tools/src/pki.rs`, `crates/fleet-core/tests/dependency_direction.rs`
- ADR-0008 clause 22; ADR-0053 clauses 5 and 24.

**Its mode.**

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero. It may need a superseding ADR, which only the human decides.

**From.** Ticket
`docs/plan/revision-2026-11-15/04-the-operator-tool-takes-from-the-client-what-adr-0008-names.md`,
moved to the backlog on the human's decision of 2026-11-15.
