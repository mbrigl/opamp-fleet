# Ticket: The next normalization re-points what the fleet split and axum-server 0.9 moved

## What to build

Accepted ADRs are immutable outside a normalization
([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 5), so two drifts wait for the next one:

- **`fleet.rs` for code now in `fleet/*.rs`.** PR #202 moved the Server's `fleet` into
  submodules. ADR-0012, -0013, -0018, -0019, -0023, -0025, -0026, -0028, -0029, -0032, -0033 and
  -0037 still cite `crates/fleet-server/src/fleet.rs` for code and tests now in
  `fleet/session.rs`, `fleet/held.rs`, `fleet/rollout.rs`, `fleet/views.rs`,
  `fleet/delivery.rs` and `fleet/approval.rs` (for example ADR-0028's
  `unavailable_tells_the_agent_when_to_retry`, ADR-0029's `forget_agent`, ADR-0019's `offer`).
  `scripts/check-traceability.sh` finds a named test in any file and
  `scripts/sensor-binding-adrs.sh` routes `fleet/` to `fleet.rs`'s ADRs, so nothing fails; a
  reader who opens `fleet.rs` does not find what the ADR names. The `pub use` lines in
  `fleet.rs` that keep moved types at their old paths (`fleet::PackageOffering`, the view types)
  stay; they are the public paths `api` and the suites use.
- **ADR-0012 clause 15** names an HTTP/1 header-read timeout of 30 seconds and a timer on the
  HTTP/1 builder. Since axum-server 0.9 (PR #195) and the silent-peer fix (PR #196) the same
  value also bounds the time to a connection's first request, and HTTP/2 has its own timer with
  bounds on streams and pings (`bounded` in `crates/opamp/src/server/listen.rs`). The clause's
  parenthesis on what axum applies describes axum-server 0.8.

The normalization rewrites each citation to the file that holds the code, and restates clause 15
with the bounds as they stand. It changes no decision.

## Acceptance criteria

- [ ] Given the normalized record, when it is searched for `fleet.rs`, then each citation names
  code that `fleet.rs` holds.
- [ ] Given the normalized ADR that carries clause 15, when it is read against `bounded`, then it
  names the first-request bound and the HTTP/2 bounds.

## Read first

- Touches: `docs/adr/`
- The skill `normalize`; ADR-0012 clause 15; `crates/fleet-server/src/fleet.rs` and
  `crates/fleet-server/src/fleet/`.

## Blocked by

- A human's instruction to normalize ([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 5).
  It is worked as part of that normalization, not alone.

## Mode

with the human: only a human starts a normalization, and ADR-0007 clause 5 keeps every part of
the system with the human.
