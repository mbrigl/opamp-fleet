# Ticket: A change a deployment sees names itself in the changelog

## What to build

`CHANGELOG.md` lists what a running deployment has to be told about, but nothing reminds the
branch that makes such a change to say so. #196 changed what every listener does with a silent
peer, and no `CHANGELOG.md` line came with it. The entry followed a day later in #204, after a
human asked what had changed since the release.

A sensor, `scripts/sensor-changelog.sh`, reads the commits of a branch that `main` does not have.
It names the branch when both of these hold:
- one of those commits is a `fix` or `feat`, or carries `!` or `BREAKING CHANGE`, and touches the
  shipped code: the `src/` of `opamp`, `fleet-core`, `fleet-util`, `fleet-server`, `fleet-agent`
  or `fleet-tools`, or `packaging/`;
- no commit of the branch touches `CHANGELOG.md`.

It lists each such commit by hash and subject. Like every sensor, it reports and never fails. A
change no deployment sees answers the sensor in the pull request, under *Documents and
decisions*: the template gains a `CHANGELOG.md:` line, an entry or "none" and why, as it has
for the overview.

## Acceptance criteria

- [ ] Given a fixture branch with a `fix(opamp): …` commit touching `crates/opamp/src/` and no
  change to `CHANGELOG.md`, when the sensor runs, then it lists that commit.
- [ ] Given the same branch with a further commit that touches `CHANGELOG.md`, when the sensor
  runs, then it lists nothing.
- [ ] Given a branch whose commits are all `refactor`, `test`, `docs` or `chore`, or which touch
  only tests, `docs/` or `scripts/`, when the sensor runs, then it lists nothing.
- [ ] Given any branch, when the sensor runs, then it exits 0. Only a head or a base that does not
  resolve exits 2.
- [ ] Given the change, when `scripts/check-all.sh` runs, then it runs the sensor and its self-test,
  `scripts/test-sensor-changelog.sh`, beside the other sensors.
- [ ] Given the pull request template, when it is read, then *Documents and decisions* has a
  `CHANGELOG.md:` line.

## Read first

- Touches: `scripts/sensor-changelog.sh`, `scripts/test-sensor-changelog.sh`,
  `scripts/check-all.sh`, `.github/PULL_REQUEST_TEMPLATE.md`
- The golden path: `scripts/sensor-cited-commits.sh` and its self-test, which read a branch's
  history in fixtures through `--root`. `scripts/lib/git.sh` holds the git helpers.
- `CHANGELOG.md`, its head: what counts as operator-facing.
- ADR-0002 (decisions verified by tests, and the sensors the template ships).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
