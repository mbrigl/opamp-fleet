# Ticket: The next normalization re-points what the Server's refactors moved

## What to build

Accepted ADRs are immutable outside a normalization
([NUC-decisions-are-proposed-then-accepted §6](../../adr/NUC-decisions-are-proposed-then-accepted.md)),
so three refactors since 2026-11-19 left citations that wait for the next one. Each moved code out
of a file an accepted ADR names, and changed no decision:

- **The use-case layers** (SRV-two-use-case-layers) moved code out of `transport.rs` and `api.rs`
  into `agent_cases.rs` and `operator_cases.rs`. The *Applies to* of five accepted ADRs still
  names the old files for it:
  - SEC-admission-by-client-certificate: the download route in `transport.rs` and the Operator
    plane's guard in `api.rs`;
  - SEC-tls13-bounded-input-chosen-planes: "`Fleet::enrol` and the download guard
    `admit_download` in `transport.rs`";
  - SEC-bootstrap-certificates-and-enrolment: the queue's answer in `transport.rs`;
  - SRV-instance-opens-chosen-planes: "the sessions and their guard in `transport.rs`";
  - PKG-signed-packages-and-delivery: "download routes of `api.rs` with the handler
    `download_package`".

  `scripts/sensor-binding-adrs.sh --path` on either new file lists none of the five, so a change
  made in `agent_cases.rs` or `operator_cases.rs` alone is not shown the ADRs that bind it. The
  review of `refactor/use-case-layers` at e821d8d15 found it; the human chose the next
  normalization over a superseding ADR on 2026-11-21.
- **The ranks of the fleet contexts** (SRV-fleet-contexts-call-by-rank) moved helpers between
  `fleet.rs` and `fleet/*.rs`. Its *Consequences* name three accepted ADRs that cite moved items
  by the file they left:
  - SEC-tls13-bounded-input-chosen-planes cites `unavailable` and `RETRY_AFTER` in
    `fleet/session.rs`, now in `fleet/approval.rs`;
  - SRV-fleet-record-agents-hosts cites the construction and the stored reads in `fleet.rs`, now
    in `fleet/placement.rs`;
  - SEC-connection-settings-never-plaintext cites the composition of the offer in
    `fleet/session.rs`, now in `fleet.rs`.

  The draft of that ADR first moved the issuing of certificates out of `fleet/approval.rs`
  (a644eb8ff), and the move was reverted (5a15144e4) because four accepted ADRs bind it by that
  file.
- **The Gateways' revocation list** moved its path and its body's types into
  `fleet_core::revocation_list` (ad40cc7bc, DEV-six-crates-tokio-axum §16), and its composition
  into `agent_cases.rs`.
  - DEV-six-crates-tokio-axum §17 lists what `fleet-core` holds, and does not name
    `revocation_list`. It does not name `pkcs11` either, which `fleet-core` has held since
    2026-11-14.
  - SEC-admission-by-client-certificate names `/v1/gateway/revocations` in
    `crates/fleet-server/src/transport.rs` alone. The route stays there; the list it answers is
    composed in `agent_cases.rs`, and its shape is in `crates/fleet-core/src/revocation_list.rs`.
    `scripts/sensor-binding-adrs.sh --path crates/fleet-core/src/revocation_list.rs` lists
    neither this ADR nor CLT-modes-and-gateway.

The normalization rewrites each citation to the file that holds the code, and adds the two modules
to the list of DEV-six-crates-tokio-axum §17. None of it changes a decision. The citations of the
three ADRs superseded on 2026-11-21 that accepted ADRs still carry move by the normalization's map
as usual.

## Acceptance criteria

- [ ] Given the normalized record, when `scripts/sensor-binding-adrs.sh --path` is run on
  `crates/fleet-server/src/agent_cases.rs`, `crates/fleet-server/src/operator_cases.rs` and
  `crates/fleet-core/src/revocation_list.rs`, then each lists the ADRs of the list above that bind
  it.
- [ ] Given the normalized record, when each item named above is searched in it, then every
  citation names the file that holds the item.
- [ ] Given the restated DEV-six-crates-tokio-axum, when its list of what goes where is read
  against `crates/fleet-core/src/`, then it names every module there.

## Read first

- Touches: `docs/adr/`
- The skill `normalize`; the *Consequences* of SRV-fleet-contexts-call-by-rank;
  DEV-six-crates-tokio-axum §16 and §17.
- `crates/fleet-server/src/agent_cases.rs`, `crates/fleet-server/src/operator_cases.rs`,
  `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/`,
  `crates/fleet-core/src/revocation_list.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human, and not claimable on its own. It waits for a human's instruction to normalize
([NUC-decisions-are-proposed-then-accepted §8](../../adr/NUC-decisions-are-proposed-then-accepted.md))
and is worked as part of that normalization. DEV-maintainer-decides-agents-implement §5 keeps
every part of the system with the human.
