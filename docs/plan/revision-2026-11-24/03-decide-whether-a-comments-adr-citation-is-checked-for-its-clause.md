# Ticket: Decide whether a comment's ADR citation is checked for its clause

## What to build

The convention "A comment that relies on a decision names its clause" (`docs/CONVENTIONS.md`)
asks for `CLT-accounts-service-runs-under §25`, not the ADR alone, so that a normalization can
re-cite it by its clause table. No check decides it, and the tree does not follow it:

- At 801f22de8, the comments of the Rust sources held about 4 400 citations of an ADR without a
  clause and 2 504 with one.
- At 82864e0c5, they hold about 4 400 without and 2 587 with one. The 48 changes since 2026-11-19
  added some 30 citations without a clause, net, and 83 with one; the count without a clause
  varies by a few dozen with the pattern used. Counted with
  `git grep -h -E '^\s*//' <commit> -- 'crates/*.rs' 'fuzz/*.rs'`, each identifier followed by
  ` §N` or not.

A rule that two of three citations break prevents little, and a convention a check can decide is
the check, not an entry (`docs/CONVENTIONS.md`, its preamble). The human chooses one:

1. A sensor of the project's own, beside `scripts/sensor-timed-waits.sh`, reports each citation of
   an ADR in force without a clause on the comment lines a change adds. A line that only moves is
   not reported. The convention's entry then names the sensor.
2. The entry goes. A citation of a whole ADR stays valid, and a normalization re-cites it by the
   ADR's identifier, as it does today.

## Acceptance criteria

- [ ] Given option 1 and a change that adds a comment citing `SRV-fleet-contexts-call-by-rank`
  without a clause, when the sensor runs on it, then it reports that line. Given a comment that
  cites `SRV-fleet-contexts-call-by-rank §4`, then it reports nothing.
- [ ] Given option 1 and a change that moves a commented function unchanged to another file,
  when the sensor runs on it, then it reports nothing.
- [ ] Given option 2, when `docs/CONVENTIONS.md` is read, then it holds no entry on clauses in
  comments, and nothing in the tree cites that entry.

## Read first

- Touches: `docs/CONVENTIONS.md`, and for option 1 `scripts/` (a sensor, its self-test, and its
  line in `scripts/check-all.sh`)
- `scripts/sensor-timed-waits.sh` and `scripts/test-sensor-timed-waits.sh`, the golden path of a
  sensor on added lines; the rule `docs/clause-citations` of `scripts/check-project-docs.sh`,
  which reads the form of a citation; NUC-checks-are-bash §4.

## Blocked by

- None (can start immediately)

## Mode

with the human: the human chooses between a sensor and dropping the convention.
DEV-maintainer-decides-agents-implement §5 keeps every part of the system with the human.
