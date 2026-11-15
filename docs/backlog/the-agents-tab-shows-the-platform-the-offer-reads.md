# The Agents tab shows the platform the offer reads

**Kind:** defect.

**Observed.** The fleet search reads an Agent's platform as `Platform::reported` folds it
(ADR-0050 clauses 6 and 14), so an Agent that reports `linux` and `x86_64` is found by
`linux-amd64`. The OS column of the Agents tab still builds the platform itself, in `platformOf`
in `crates/fleet-server/static/index.html`: raw, and from the non-identifying attributes only. The
row shows `linux-x86_64`, and a search for that text no longer finds the row. An Agent that
reports its platform only among its identifying attributes shows "no platform reported", though
it is offered a package. The comment above `platformOf` says the pair is the one an artifact is
stored under, which holds only for an Agent that reports the canonical tokens.

**Idea.** `AgentView` carries the platform `Platform::reported` reads, and `platformOf` shows it.
That adds a field to the response of `GET /api/v1/agents` and its OpenAPI document (ADR-0031,
ADR-0032). It joins the dedup of `listing::derive` and `AgentView` in
[`fleet-and-api-are-split-in-four-slices.md`](fleet-and-api-are-split-in-four-slices.md), or goes
first.

**From.** Revision 2026-11-15 ticket 02 (the fleet search reads the platform the offer reads).
