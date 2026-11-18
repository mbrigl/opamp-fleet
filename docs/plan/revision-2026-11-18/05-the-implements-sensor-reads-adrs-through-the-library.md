# Ticket: The Implements sensor reads ADRs through the library

## What to build

`scripts/sensor-implements-adrs.sh` sources only `scripts/lib/git.sh` and writes again what
`scripts/lib/adr.sh` holds: the ADR identifier (near line 73; `ADR_REF_RE`), the file of an
ADR number (near line 58; `ADR_FILE_RE`) and the status line (near line 60;
`adr_field_line` and `adr_field_value`, which read stdin as `-`). Check 18 had the same
copy of a library helper and counted clauses inside a fence until 0d314e881 took
`ADR_FENCE_AWK` instead.

The sensor takes the three from the library and reports as before.

## Acceptance criteria

- [ ] Given the change, when `scripts/test-sensor-implements-adrs.sh` runs, then it passes
  unchanged.
- [ ] Given the change, when the sensor is read, then it writes no ADR regular expression and no
  status parsing of its own.

## Read first

- Touches: `scripts/sensor-implements-adrs.sh`
- `scripts/lib/adr.sh`; `docs/CONVENTIONS.md`: "A rule two paths apply is one function".

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
