# Ticket: The next normalization re-points what the fleet split and axum-server 0.9 moved

## What to build

Accepted ADRs are immutable outside a normalization
([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 5), so two drifts wait for the next one:

- **`fleet.rs` for code now in `fleet/*.rs`.** PR #202 moved the Server's `fleet` into
  submodules. Fifteen accepted ADRs still cite `crates/fleet-server/src/fleet.rs` for code and
  tests now in `fleet/session.rs`, `fleet/held.rs`, `fleet/rollout.rs`, `fleet/views.rs`,
  `fleet/delivery.rs` and `fleet/approval.rs`:
  - in their text: ADR-0012, -0013, -0018, -0019, -0023, -0025, -0026, -0028, -0029, -0032,
    -0033 and -0037 (for example ADR-0028's `unavailable_tells_the_agent_when_to_retry`,
    ADR-0029's `forget_agent`, ADR-0019's `offer`);
  - in their *Applies to* alone: ADR-0016 (the host and predecessor a CSR is issued to, now in
    `fleet/approval.rs`), ADR-0027 (the approval and renewal paths, `fleet/approval.rs`) and
    ADR-0030 (the queries the fleet answers for the lists, `fleet/views.rs`).
  `scripts/check-traceability.sh` finds a named test in any file and
  `scripts/sensor-binding-adrs.sh` routes `fleet/` to `fleet.rs`'s ADRs, so nothing fails; a
  reader who opens `fleet.rs` does not find what the ADR names. The `pub use` lines in
  `fleet.rs` that keep moved types at their old paths (`fleet::PackageOffering`, the view types)
  stay; they are the public paths `api` and the suites use.
- **ADR-0012 clause 15** names an HTTP/1 header-read timeout of 30 seconds and a timer on the
  HTTP/1 builder, and its parenthesis on what axum applies describes axum-server 0.8. Since
  axum-server 0.9 (PR #195) the library turns the timeout off unless the listener sets it, which
  `bounded` in `crates/opamp/src/server/listen.rs` does. The HTTP/2 bounds stay with clause 20,
  which decides them; the restated clause 15 does not repeat them.

The normalization rewrites each citation to the file that holds the code, and corrects clause
15's account of axum-server. Neither changes a decision.

**Open question for the human, before the normalization:** since the silent-peer fix (PR #196),
the same 30-second value also bounds the time to a connection's first request
(`first_request_timeout`). No clause names that bound today. Writing it into clause 15 records
what #196 chose, which a normalization does not do, since it changes no decision
([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 5). The human either:

- counts it as the implementation of ADR-0012's bounded input, so the restated clause names it;
- or has it proposed as a decision of its own through the skill `propose-adr`, before or apart
  from the normalization.

## Acceptance criteria

- [ ] Given the normalized record, when it is searched for `fleet.rs`, then each citation names
  code that `fleet.rs` holds.
- [ ] Given the normalized ADR that carries clause 15, when it is read against `bounded`, then
  what it says of axum-server and the header-read timeout holds, and it names the first-request
  bound only as the human answered the open question.

## Read first

- Touches: `docs/adr/`
- The skill `normalize`; ADR-0012 clauses 15 and 20; `crates/fleet-server/src/fleet.rs` and
  `crates/fleet-server/src/fleet/`.

## Blocked by

None (can start immediately)

## Mode

with the human, and not claimable on its own. It waits for a human's instruction to normalize
([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 5) and is worked as part of that
normalization. ADR-0007 clause 5 keeps every part of the system with the human.
