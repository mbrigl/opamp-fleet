# A proof a CSR carries is read one way, and a request with both is refused at a Gateway too

**Kind:** defect, security.

**Observed.** `fleet-core` has no reader of the proof a certificate signing request carries. The
Server reads the renewal proof and the bootstrap proof in `crates/fleet-server/src/ca.rs` (near
lines 169, 228 and 264), the bootstrap proof by hand more than once. A request that carries both
proofs is refused by the Server's renewal path, but `bootstrap_proof_certificate` and the Gateway's
check of an enrolling peer let it pass (ADR-0016 clause 21).

**What the old branch had.** Revision ticket 04 of `feat/revision-2026-10-31`, commits ce3a7db2 and
4dff4359. `fleet_core::csr::proof` read the proof of a kind from a request's DER and refused a
request that carried the other beside it. The Server's three readers and the Gateway's check used
it. A Gateway's own request with both proofs stayed on its route and was refused with
`BOTH_PROOFS`. A table test gave the same requests to the Gateway's reader and the Server's. It was
not carried: the enrolment and the Gateway code have been reworked since, and the change needs a
fresh cut.
