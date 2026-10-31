# Ticket: Fold ADR-0036 into the ADR that decides the second service

## What to build

`propose-adr` is needed, or the next normalization. ADR-0036 has one clause, which restates ADR-0037
clause 19 for the Windows DACLs on the second service's process and threads, and the two are read
together for every change to `supervisor/channel/windows.rs`; ADR-0037 is 1521 lines and also
decides the no-link discipline of clause 11 and the channel of clauses 17 to 23. ADR-0036's clause
text is itself muddled: it says that ADR-0037 clause 19 restates ADR-0020 clause 19 for its
decision, one more reason to fold it. Decisions that have grown into one are a candidate for one
consolidating ADR (docs/adr/README.md, Mechanics).

Propose to the human which way it goes: a superseding ADR that consolidates ADR-0036 with the
second-service clauses of ADR-0037, or nothing until the next normalization, which may also split
ADR-0037 into the accounts, the no-link discipline and the channel. No decision changes.

## Acceptance criteria

- [ ] Given the human's choice, when it is made, then either a proposed consolidating ADR exists in
  commits of ADRs only, or this ticket is deleted with the choice recorded in its merge message.

## Read first

- ADR-0036, ADR-0037, `docs/adr/README.md` Mechanics, the `normalize` and `propose-adr` skills.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. Only the human decides on the record.
