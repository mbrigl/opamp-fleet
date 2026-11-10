# A stored record is loaded by the rules it was written by

**Kind:** defect.

**Observed.** A store loads some records by looser rules than the write that made them. A Package
reference with an unknown field is refused by the PostgreSQL store and not by the filesystem store.
A stored Deployment with a second Package of one Agent type loads with the last one kept, where the
write refuses it. A stored assignment's names and pinned revisions are not checked against the name
grammar and the content-hash form (ADR-0023). Where: the load paths of
`crates/fleet-server/src/fs/` and `crates/fleet-server/src/postgres/`.

**What the old branch had.** Revision ticket 30 of `feat/revision-2026-10-31`, commits 31ab40b3 and
f8071ac7, on top of ticket 05 (see `both-store-adapters-load-through-the-core.md`). It refused each
case on load in the words of the write's refusal, on both stores, and planted each record in tests.
Whether a pinned revision is still retained was not asked. It was not carried, for the reason of
ticket 05.
