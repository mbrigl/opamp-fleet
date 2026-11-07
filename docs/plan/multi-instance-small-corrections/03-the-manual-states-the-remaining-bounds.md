# Ticket: The manual states the remaining bounds

## What to build

Behaviours of the Server the manual does not state, each a sentence or a row where the manual
already treats the subject:

- on shutdown, `presence.stop()` waits up to one call bound after `drain_secs` for the last
  presence batch (the shutdown paragraph of the Server's page)
- a `DELETE` of a Package set or entry answers only after its sweep, which may wait up to 60 s for
  the blob lock
- on S3 the staged upload is moved into place with one copy, which AWS limits to 5 GiB, while
  `max_package_size_bytes` has no upper bound: an upload past it is streamed whole and then
  fails; the configuration's check refuses a size past what the store can copy, or the manual
  states the limit beside the key
- a step forward of the database's clock deems live leases lapsed, so a serving instance's upload
  in the copy window fails; nothing is lost
- a chain with a gap at its cut is skipped and warned at every prune until the record is repaired
- the `[audit]` keys have no table, only prose in the audit record's section, while every other
  section of `server.toml` has one

## Acceptance criteria

- [ ] Given the manual, when an operator reads the shutdown, the Package deletion, the blobs, the
  leases and the audit record, then each bound above is stated where the subject is
- [ ] Given `max_package_size_bytes` past what the S3 store can copy, when the Server starts, then
  it fails naming the key, or the manual names the limit beside the key
- [ ] Given the manual check, when it runs, then it passes

## Read first

- The manual's conventions: no ADR cited, every configuration key in its table
- `docs/manual/server.md`, the sections named above; ADR-0044 clauses 12 and 13

## Blocked by

- docs/plan/multi-instance-small-corrections/02-package-errors-keep-their-status.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
