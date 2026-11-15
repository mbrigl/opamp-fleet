# The contract pair is dropped

**Kind:** schema, release.

**Observed.** The Deployments' mirror columns `packages` and `configurations` and the Hosts'
`instance_uids` arrays are written by no release since 0.8.1. Ticket
`docs/plan/the-contract-pair-is-removed/01-the-server-stops-naming-the-contract-pair.md` is release
X: it gives the columns defaults, and the code stops naming them.

**Idea.** Release X+1 drops the columns by a migration, together with the comments that keep them.
The migration names X as the oldest Server (ADR-0023 clause 22). It cannot come earlier, since a
Server of 0.8.2 beside an instance still names the columns.

**From.** The triage of the plan by the human (2026-11-15); replaces ticket 02 of
`the-contract-pair-is-removed`.
