# The Implements sensor misreads an ADR over 64 KiB

**Kind:** defect.

**Observed.** `scripts/sensor-implements-adrs.sh` reports "no status line" for every ADR in force
whose file is larger than a pipe's buffer, 64 KiB on Linux. Its `state` (near line 59) reads the
status as `git show "$head:$file" | adr_field_line - Status`. `adr_field_line`
(`scripts/lib/adr.sh`, line 51) is `grep -m1`, which exits at the first match. `git show` then
dies of SIGPIPE on the rest of the file, with exit 141. The sensor runs under `set -o pipefail`,
so the pipeline fails, the `if` drops the line it found, and the status stays empty. Thirteen
ADRs are over 64 KiB at ebaeec448, among them `SRV-instance-opens-chosen-planes` (90 KiB) and
`SRV-state-stores-contract-pair` (96 KiB).

Reproduction, at ebaeec448:

```sh
bash -c 'set -o pipefail; source scripts/lib/adr.sh
  git show HEAD:docs/adr/SRV-instance-opens-chosen-planes.md | adr_field_line - Status
  echo "exit $?"'
# - **Status:** 🟡 proposed
# exit 141
scripts/sensor-implements-adrs.sh "$(git merge-base main ebaeec448)" ebaeec448
# ... — SRV-instance-opens-chosen-planes: no status line
```

The sensor fails nothing, so the false findings only cost the reader's trust in it.

**Idea.** The sensor reads the file to the end before it matches, for example into a variable or
a temporary file, or the pipeline's status is taken from `adr_field_line` alone. A test of the
sensor names an ADR larger than 64 KiB. Other callers of `adr_field_line` on a pipe under
`pipefail` need the same check.

**From.** The review of `feat/planes-per-instance` at ebaeec448 (2026-11-21).
