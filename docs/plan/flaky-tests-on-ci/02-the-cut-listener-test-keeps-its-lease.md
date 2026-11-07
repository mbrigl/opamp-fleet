# Ticket: The cut-listener test keeps its lease

## What to build

`a_revocation_committed_while_the_listening_connection_was_cut_ends_its_session_once_back` in
`crates/fleet-server/tests/instances.rs` was seen closing the session with `1001` "shutting
down" instead of `1008`: the cut of B's listening connection also took B's lease, so B closed its
listeners instead of ending the revoked session. The test leaves no margin between the cut and the
lease's lapse. It decides what it asserts whatever the host's load: the cut cannot cost B its
lease, or the lease's times give the cut room.

## Acceptance criteria

- [ ] Given the test on a loaded host, when it runs thirty times, then it closes the session with
  `1008` every time
- [ ] Given the revocation's end of the session broken on purpose, when the test runs, then it fails

## Read first

- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done), which counts a flaky
  test as a defect
- ADR-0048 clauses 3 and 10
- Seam: the test's cut of the listening connection

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
