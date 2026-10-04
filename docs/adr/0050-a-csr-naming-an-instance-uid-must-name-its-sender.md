# ADR-0050: A CSR that names an instance_uid is signed only when it names its sender's

- **Status:** 🟡 proposed
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** the reading of a CSR in `crates/fleet-server/src/ca.rs`, the CSR handling on a member connection in `crates/fleet-server/src/fleet.rs` and on an enrolment connection in `crates/fleet-server/src/transport.rs`

## Context

The Baseline, in *Using instance_uid in the CSR*: *"The Server MUST verify that the instance_uid
field in AgentToServer message matches the instance_uid in the CSR fields"*, so an Agent cannot ask
for a certificate in another Agent's name. It prescribes no field: the `instance_uid` may sit in
*"one of the CSR fields (or part of the field)"*.

[ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) left this out of
scope, and its clause 7 calls the subject descriptive: the Server does not require it to match
`instance_uid` and drops requested SANs. That stays. What it leaves unmet is the conditional MUST:
a CSR that *does* carry an `instance_uid` is signed whatever it carries.
[`CONFORMANCE.md`](../CONFORMANCE.md#mutual-tls-and-the-two-fields-still-refused) records the gap.

This project's own Client puts its `service.instance.name` in the common name and no
`instance_uid` anywhere, so it never triggers the check. A peer implementation may, and
interoperability with one is a stated target
([ADR-0010](0010-protocol-baseline-and-conformance.md)). The specification's G-17 asks that the
Server accept only authenticated Agent identities.

The difficulty is recognising a claim without reading an ordinary descriptive subject as one. The
Baseline's `instance_uid` is 16 bytes and SHOULD be a UUID v7; its textual form is the canonical
8-4-4-4-12 hexadecimal form of RFC 9562, which a host name or a product name does not take by
accident.

## Decision

We will treat every canonical UUID text in a CSR's subject or requested SANs as a claim to an
`instance_uid`, and answer a CSR whose claims are not all the sender's own with a
`ServerErrorResponse` of type `BadRequest`.

1. **A claim is a canonical UUID in text.** Thirty-two hexadecimal digits in groups of 8, 4, 4, 4
   and 12 joined by hyphens, in either case, standing alone or as part of a longer value — the
   Baseline's "part of the field". The `urn:uuid:` prefix of RFC 9562 is read the same way. A run
   of 32 hex digits without hyphens is not a claim: it is what a fingerprint or a hash looks like.

2. **Every value is searched.** Each attribute of the subject, read as text whichever string type
   encodes it, and each requested SAN of a type that carries text — DNS name, URI, e-mail address
   — is searched for claims. A SAN is read for
   this check only; ADR-0039 clause 7 still drops it from the certificate.

3. **The sender is the message's `instance_uid`.** The `instance_uid` of the `AgentToServer` that
   carries the CSR, as the message states it and before any re-key the same message causes, is
   compared byte for byte with each claim. On an enrolment connection it is the
   same field.

4. **A mismatch is refused, a match or no claim is signed.** A CSR with one claim that differs from
   the sender's is answered `BadRequest`, naming the mismatch without echoing the CSR, and never
   reaches the signer or the enrolment queue. A CSR whose claims all match is signed as any other.
   A CSR with no claim is signed exactly as today; that is this project's own Client.

**Out of scope:** binding the issued certificate to the `instance_uid`
([ADR-0039](0039-admission-requires-both-proofs-and-enrolment-is-approved.md) clause 7 stands);
the Client putting its `instance_uid` into its own CSR; claims in CSR extensions other than the
SAN.

## Alternatives considered

- **Stripping a mismatched claim and signing anyway.** Rejected: it does not meet the Baseline's
  MUST, and a peer that made a false claim learns nothing about why its identity does not hold.
- **Recognising a claim only in one agreed field, such as a `urn:uuid:` SAN.** Rejected: the
  Baseline names no field, and a peer that put its `instance_uid` in the common name would pass
  unchecked.
- **Recognising any 32 hex digits.** Rejected: key fingerprints and hashes take that form, and a
  descriptive subject carrying one would be refused for nothing.
- **Leaving H3 undone.** Rejected: the MUST is conditional but unmet, and the cost is a parser
  over values the Server already reads.

## Sources / Prior art

- OpAMP specification v0.20.0, *Using instance_uid in the CSR*
  ([opamp-spec](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md)).
- [RFC 9562 §4](https://www.rfc-editor.org/rfc/rfc9562#section-4): the UUID string representation and the
  `urn:uuid:` namespace.
- [`HARDENING.md`](../HARDENING.md), measure H3, and the observable it names.

## Consequences

- Positive: the Baseline's conditional MUST is met; a peer cannot ask for a certificate in another
  Agent's name; nothing changes for this project's own Client.
- Negative / trade-offs: a descriptive subject that happens to contain a canonical UUID other than
  the sender's is refused. An operator who names hosts after UUIDs must name them after the
  Agent's own.
- Follow-ups: running the check against opamp-go in the interoperability suite.

## Enforcement

- `crates/fleet-server/src/ca.rs` tests: `a_canonical_uuid_anywhere_in_a_value_is_a_claim`,
  `hex_without_hyphens_is_no_claim`, `a_san_is_read_for_claims`,
  `a_claim_in_a_bmp_or_universal_string_is_read` (clauses 1, 2).
- `crates/fleet-server/tests/mutual_tls.rs` tests:
  `a_csr_claiming_another_instance_uid_is_a_bad_request`,
  `a_csr_claiming_its_own_instance_uid_is_signed`, `a_csr_claiming_nothing_is_signed_as_before`,
  and `an_enrolment_csr_claiming_another_instance_uid_never_reaches_the_queue` (clauses 3, 4).
