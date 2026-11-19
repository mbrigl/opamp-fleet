# Ticket: The project's own document rules leave the template's scripts

## What to build

The project has added rules of its own inside scripts the template ships:
- `scripts/check-docs.sh` holds `docs/named-sections` and `docs/clause-citations` (about 266 lines
  differ from NUC v0.11.1);
- `scripts/sensor-tests-kept.sh` holds the timed-wait section and `is_test_file` (about 165 lines);
- `check-all.sh`, `check-shell.sh` and `check-traceability.sh` hold smaller additions, and so do
  their self-tests.

Each take-up of NUC v0.9.1, v0.10.0, v0.11.0 and v0.11.1 merged those files by hand (eacdf43a9,
58ba56482, 7eb038c1c, e31a4ea44). A project fixture broke on the way and was fixed in 16676bf2c.
The project's own sensors (`sensor-changelog.sh`, `sensor-implements-adrs.sh`) show the other
shape: a script of the project's own, called from `check-all.sh`.

The project's rules in `check-docs.sh` and `sensor-tests-kept.sh` move into scripts of the
project's own, each with its self-test, called from `check-all.sh`, so the template's files are
the template's again. A moved script reads the documents again for itself, since the template's
scripts offer no point where a project's rule runs on what they have parsed. A rule that cannot
move at that cost stays, and the hand-off names it. The smaller additions to `check-shell.sh` and
`check-traceability.sh` stay, and the hand-off names them.

## Acceptance criteria

- [ ] Given the change, when `check-docs.sh` and `sensor-tests-kept.sh` are compared with NUC
  v0.11.1, then they differ only where the hand-off names a reason.
- [ ] Given a document that breaks a moved rule, when `scripts/check-all.sh` runs, then it fails
  as before, decided by the moved self-tests, unchanged in number of cases.
- [ ] Given the change, when `scripts/check-all.sh` runs, then it passes.

## Read first

- Touches: `scripts/check-docs.sh`, `scripts/test-check-docs.sh`, `scripts/sensor-tests-kept.sh`,
  `scripts/test-sensor-tests-kept.sh`, `scripts/check-all.sh`
- NUC-checks-are-bash; NUC-template-learns-from-projects.

## Blocked by

- None (can start immediately)

## Mode

with the human: DEV-maintainer-decides-agents-implement §5, and where a rule cannot move, the human
decides whether it waits for the template.
