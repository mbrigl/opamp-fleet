# What serves beside a Server of 0.8.0 goes

**Kind:** idea, cleanup.

**Observed.** Since migration 19 names 0.8.4 as the oldest Server, an instance of 0.8.5 refuses
to start beside a live lease of any release before 0.8.4. Comments and code paths that keep a
Server of 0.8.0 serving correctly beside this release no longer apply. The implementation of
release 0.8.5 named them in `crates/fleet-server/src/postgres/enrolment.rs` (rejected keys),
`postgres/agents.rs` (the count's lock), `transport.rs`, `postgres/notify.rs`,
`fleet/views.rs`, `postgres/deployments.rs` (near line 98) and `postgres/ledger.rs` (the
registration of a renewal).

**Idea.** Each path that exists only for a Server of 0.8.0 beside this one goes, with its comment,
and a path that also serves 0.8.4 keeps only that reason. Some of them are clauses of
SRV-state-stores-contract-pair, or of SRV-instances-share-one-postgresql,
SEC-bootstrap-certificates-and-enrolment and SEC-append-only-audit-record; those stay until a
supersession or the next normalization restates them.

**From.** The implementation of release 0.8.5 of the contract pair (2026-11-19).
