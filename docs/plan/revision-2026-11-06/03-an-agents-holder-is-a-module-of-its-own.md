# Ticket: What an instance holds of its Agents is a module of its own

## What to build

`crates/fleet-server/src/fleet.rs` is 6 600 lines without its tests, and `AppState`'s one `impl`
is 4 500 lines with about 165 methods. Beside the report processing that
`docs/plan/revision-2026-10-31/13-the-servers-report-processing-has-a-module.md` on
`feat/revision-2026-10-31` moves out, it now carries a second concern of its own: what one
instance holds of the Agents it carries on a shared store. That is the take and the release of
an Agent (`Took`, `Taking`, `Carried`, `Settled`, `release_taken`), the presence it writes
(`PresenceWritten`, `write_presence`), the re-keys whose answer was lost (`PendingRekey`,
`UnsureRekey`), the restarts kept for retry (`unput_restarts`), the take-up of what other
instances announced (`take_up_agents`, `take_up_restarts`, `place_again_for`), and the per-Agent
locks. Four of the take-up functions share one parameter, `Option<&HashSet<InstanceUid>>`,
whose `None` means "a notification was missed, so every Agent".

Move that concern into a module of its own behind the same `AppState` entry points, with the
notice it acts on as one type, and update the overview in the same change.

## Acceptance criteria

- [ ] Given `fleet.rs`, when the change lands, then the take, release, presence and take-up of
  the Agents an instance carries live in one module, and `dependency_direction.rs` gives it its
  role.
- [ ] Given a take-up function, when it is called, then a notice naming Agents and a missed
  notification are two values of one type, not `Some` and `None` of a set.
- [ ] Given the existing `fleet.rs` tests, `crates/fleet-server/tests/` and the store checks, when
  the change lands, then they pass unchanged.

## Read first

- ADR-0009;
  ADR-0048
  clauses 8 to 10.
- `docs/plan/revision-2026-10-31/13-the-servers-report-processing-has-a-module.md` on
  `feat/revision-2026-10-31`: whichever of the two lands second takes the other's module up.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. The cut between the two modules is the human's to settle with
revision-2026-10-31's ticket 13.
