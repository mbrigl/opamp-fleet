# Ticket: Decide whether the shared crates are cut again by cohesion

## What to build

A decision, not code: whether to draft an ADR that supersedes ADR-0008 clause 16 and cuts
`fleet-core` and `fleet-util` by cohesion instead of by who shares an item. The architecture
review of 2026-11-17 raised it; this revision weighed it and leaves it to the human.

For a re-cut, from the work since 2026-11-15:

- `fleet-util` holds about 2 600 lines of unpackers in `archive` because the operator tool names
  `archive::unix_mode_attributes` in shipped code; its other calls of the unpacker are in its
  tests.
- Membership by who shares moves items when a caller comes or goes: PR #197 moved `lstat_at`
  and `rmdir` from `fleet_util::nofollow::at` to the Client by hand, and clause 16 moves an item
  of `fleet-util` the Server comes to use into `fleet-core`.
- The test that decides membership needed three rounds of fixes (ticket 04 of this revision).

Against it:

- A re-cut moves paths that many accepted ADRs and the overview cite, so it belongs with a
  normalization (ticket 10).
- `fleet-core` is 2 700 lines and `fleet-util` 4 000; ADR-0008 clause 16 already says nothing
  enters a crate to make it look less small, and the dependency rule it keeps (the Server links
  nothing it does not use, the tool links neither end) holds and is tested.
- Ticket 04 may remove the cost of the membership test without a new decision.

Splitting `fleet-agent` (65 000 lines, one crate) into crates was weighed too and is not filed:
nothing in the work since 2026-11-15 went wrong at its crate boundary, and the module split of
the Server's `fleet` (PR #202) showed the cheaper step first. A test-only crate `fleet-e2e`
stays rejected (ADR-0008: `CARGO_BIN_EXE_*` resolves only in the declaring crate).

## Acceptance criteria

- [ ] Given the human's decision, when this ticket closes, then either a draft ADR exists under
  `docs/adr/` through the skill `propose-adr`, or the reason not to is recorded in the closing
  commit.

## Read first

- Touches: `docs/adr/` (only if the human asks for a draft)
- ADR-0008 clauses 11, 16 and 17; `crates/fleet-util/src/`; `crates/fleet-core/src/`.

## Blocked by

- docs/plan/revision-2026-11-18/04-the-who-shares-test-decides-per-item-on-a-syntax-tree.md, so
  the decision sees what a per-item test finds.

## Mode

with the human: it is a decision that would supersede an accepted ADR
([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 5).
