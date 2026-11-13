# An Agent downloads a Package from the bucket directly

**Kind:** idea, open question.

**Observed.** Every artifact reaches an Agent through a Server instance, or through its Gateway's
cache, which fetched it from one (ADR-0034, G-19: "the fleet never reaches a store itself"). With
several instances on one S3-compatible bucket, an instance streams each download it serves. The
feature spec of several Server instances left open whether an Agent should fetch a Package from
the bucket directly instead, by a link the instance signs. No ADR decides it, and it moves G-19,
so it needs a specification change and an ADR before anyone builds it.

**Idea.** Measure first what the downloads of a rollout cost an instance at the G-20 scale; take it
to the human only if that cost is what limits a rollout.

**From.** The *Out of scope* of `docs/plan/several-server-instances/spec.md`, closed by the design
revision of 2026-11-12.
