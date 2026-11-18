# The tests-kept sensor reads a ticket as a test file

**Kind:** defect, sensor.

**Observed.** `scripts/sensor-tests-kept.sh` reports a deleted file as a deleted test file when a
segment or the basename of its path contains `test` or `spec` (`is_test_path`). The change that
lands a ticket deletes its file under `docs/plan/`, so a ticket whose slug holds the word is
reported. On `plan/revision-2026-11-18-frontier` it reported
`04-the-who-shares-test-reads-a-syntax-tree.md` and
`09-the-listeners-test-bound-is-one-parameter.md`, neither a test.

**Idea.** The sensor leaves out the paths under `docs/plan/`, or reads only the paths of a
toolchain's sources. Its self-test, `scripts/test-sensor-tests-kept.sh`, gains a fixture with a
deleted ticket whose slug says `test`.

**From.** The review of `plan/revision-2026-11-18-frontier` at 6d7b2732f (2026-11-18).
