# Accepted ADRs name the files the use-case layers left

**Kind:** record.

**Observed.** The branch `refactor/use-case-layers` moved code out of `transport.rs` and `api.rs`
into `agent_cases.rs` and `operator_cases.rs` (SRV-two-use-case-layers). The *Applies to* of five
accepted ADRs still names the old files for that code:

- SEC-admission-by-client-certificate: the download route in `transport.rs` and the Operator
  plane's guard in `api.rs`;
- SEC-tls13-two-planes-bounded-input: "`Fleet::enrol` and the download guard `admit_download` in
  `transport.rs`";
- SEC-bootstrap-certificates-and-enrolment: the queue's answer in `transport.rs`;
- SRV-instances-share-one-postgresql: "the sessions and their guard in `transport.rs`";
- PKG-signed-packages-and-delivery: "download routes of `api.rs` with the handler
  `download_package`".

`scripts/sensor-binding-adrs.sh --path` on either new file lists none of the five, so a change made
in `agent_cases.rs` or `operator_cases.rs` alone is not shown the ADRs that bind it. An accepted
ADR's text cannot be edited in place (NUC-decisions-are-proposed-then-accepted §6).

**Idea.** The next normalization moves the five *Applies to* to the new files. Until then a
session that touches either file reads the five by hand.

**Decided.** The human chose the next normalization on 2026-11-21, over a superseding ADR, which
would restate five ADRs for their paths alone.

**From.** The review of `refactor/use-case-layers` at e821d8d15 (2026-11-21).
