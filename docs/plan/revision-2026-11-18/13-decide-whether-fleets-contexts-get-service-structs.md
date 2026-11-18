# Ticket: Decide whether the contexts of `fleet` get service structs

## What to build

A decision on the next step after the split of PR #202, which closed with this revision: whether
a context of the Server's `fleet` gets a service struct that owns its own state, and which first.
The spec of the split left it to a later spec "once the boundaries can be seen in the code". They
can now: `fleet::views`, `delivery`, `rollout`, `approval` and `session` beside `held` and
`placement`, with `AppState` in the root.

What the human weighs:

- The backlog entry `docs/backlog/fleet-and-api-are-split-in-four-slices.md`: what is left
  of its slices 1 and 2 (report processing cut into named steps, the administration as a use
  case `api` calls) waits for this decision; its slices 3 and 4 are independent of it.
- The draft `draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md` on
  `feat/server-drafts`, which places such use cases in a layer of their own.
- Ticket 02 of this revision, which makes the dependencies between the contexts visible first.

## Acceptance criteria

- [ ] Given the human's decision, when this ticket closes, then a spec under `docs/plan/`
  exists through the skill `plan-feature`, or the backlog entry records that the step is not
  taken and why.

## Read first

- Touches: `docs/plan/`, `docs/backlog/fleet-and-api-are-split-in-four-slices.md`
- `crates/fleet-server/src/fleet.rs`; ADR-0009 (ports and adapters), ADR-0025 (instances).

## Blocked by

- docs/plan/revision-2026-11-18/02-fleets-contexts-name-what-they-take-from-each-other.md

## Mode

with the human: it is a decision on scope ([`AGENTS.md` §4](../../../AGENTS.md#4-working-style)).
