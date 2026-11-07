# Ticket: The Enforcement names the tests there are

## What to build

The Enforcement sections of ADR-0030, ADR-0023 and ADR-0012 name the tests that decide their
behaviours. Two behaviours have no test of their own, and one test lacks its marker:

- ADR-0030: a restart requested through one instance while another instance delivers one; the
  tests run the two in sequence, never as a race.
- ADR-0023 clause 23: an instance takes its lease before it first opens a listener.
- ADR-0012: `plaintext_off_the_loopback_is_refused_before_connecting` carries no marker.

The two behaviours get their tests, and the test gets its marker. The traceability check reads
the names an Enforcement section lists, so a name no test has cannot stand again. The sentences
of the three ADRs that say a test or a marker is missing stay until the next normalization.

## Acceptance criteria

- [ ] Given a restart requested through one instance, when another instance delivers one at the
  same time, then a test decides that the request stays until it is sent
- [ ] Given an instance on PostgreSQL that starts, when it opens its listeners, then a test
  decides that it held its lease before
- [ ] Given `plaintext_off_the_loopback_is_refused_before_connecting`, when this lands, then it
  carries `Verifies: ADR-0012`
- [ ] Given an Enforcement section that names a test no file defines, when the traceability check
  runs, then it reports it
- [ ] Given the next normalization, when it restates ADR-0023, ADR-0030 and ADR-0012, then it
  names the new tests and drops the sentences that say a test or a marker is missing

## Read first

- ADR-0002 and `scripts/check-traceability.sh`; the Enforcement sections of ADR-0030 and
  ADR-0012 and of ADR-0023
- [`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules), rule 5, on changing accepted text only by a
  normalization

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
