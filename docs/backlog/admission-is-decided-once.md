# The Agent plane admits a certificate by one rule, for OpAMP and the package download

**Kind:** defect, security.

**Observed.** `/v1/opamp` and the package download each decide admission with a copy of their own,
and the copies differ (ADR-0040 clauses 6, 18, 31 and 33). On `main` the download's guard is
`admit_download` in `crates/fleet-server/src/transport.rs` (near line 295) and the OpAMP route's is
`admit` (near line 576). The download turned a certificate whose facts cannot be read into no facts,
and without a revocation list admitted it as a member, where `/v1/opamp` refuses it. The two also
ask about revocation through different calls.

**What the old branch had.** Revision ticket 01 of `feat/revision-2026-10-31`, commit 9f2e5e64.
It made the rules every route holds a presented certificate to one `Checks` value, shared by
`Admission` and the download guard: present where one is required, readable, not ended, not
revoked. Each route kept its own message and audit events, and the download added what only it
asks: a certificate of the client CA, and the sender's rate. A test presented a certificate with a
21-octet serial and saw it refused on both routes. It was not carried: `transport.rs` has been
reworked since by the stages of several Server instances, and the change needs a fresh cut.
