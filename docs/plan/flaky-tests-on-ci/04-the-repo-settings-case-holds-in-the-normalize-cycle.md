# Ticket: The repository settings case holds in the normalize cycle

## What to build

The *Normalize map self-test* (`scripts/test-normalize-remap.sh`) failed once in a local
`scripts/check-all.sh` run on `feat/multi-instance-stages-2-3`, at 8470b0838, on 2026-11-03. That
self-test copies the tree, maps every ADR and criterion one step on, and then runs every other
`scripts/test-*.sh` of the copy side by side. One of them, `test-sensor-repo-settings.sh`, failed
one of its 27 cases:

```
FAIL: test-sensor-repo-settings.sh passes after a cycle of the real tree
    FAIL: two rulesets whose strictest rules match the README report nothing (expected exit 0 with "nothing to report", got exit 0)
        Repository settings sensor: owner/name differs from README → Repository settings in 1 point(s):
          - main: the required check 'docs' is not among those README names
```

The sensor read a required check `docs` that the README of the copy did not name, or did not read
that name where the case answers it. The case before it, in the same file, uses the same fake
answers. Run alone at the same tree, `test-normalize-remap.sh` (52 cases) and
`test-sensor-repo-settings.sh` (27 cases) both passed. The change under test touched no script and
no README, so the cycle's map, the parallel run of the self-tests, or the fake `gh` answers the
case shares with the others are the suspects.

It failed the same way a second time, at e0896085d on 2026-11-04, then naming the required check
`shell`, and passed when the run was repeated unchanged. The name varies, so the case reads a
required check from somewhere other than its own fake answers.

Find which of them made the sensor read a check the README does not name, and make the case
decide what it asserts whatever runs beside it.

## Acceptance criteria

- [ ] Given the cause, when it is named, then the hand-off shows what the sensor read, from where,
  and why only the run inside the normalize cycle saw it
- [ ] Given `scripts/test-normalize-remap.sh` on a loaded host, when it runs thirty times, then the
  repository settings case passes each time
- [ ] Given a fix in the sensor or its fake rather than in the test, when that fix is removed, then
  a test fails

## Read first

- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done), which counts a flaky
  test as a defect
- `scripts/test-normalize-remap.sh`, the cycle and the parallel run of the self-tests in the copy
- `scripts/test-sensor-repo-settings.sh` and `scripts/sensor-repo-settings.sh`, the fake answers
  and how the README's checks are read
- Seam: the self-test's copy of the tree and its fake `gh` answers

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
