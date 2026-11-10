# server migrate checks every store the Server opens

**Kind:** defect.

**Observed.** `server migrate` checks the migrated data directory with a list of its own:
`check_opens` in `crates/fleet-server/src/migrate.rs` (near line 766) opens some of the stores the
Server's startup opens, not all, and not through the startup's constructors. A migrated directory
whose floor or enrolment file the first start would refuse passes the migration.

**What the old branch had.** Revision ticket 06 of `feat/revision-2026-10-31`, commits e67a8152 and
d5cd42fb. `Records::open_every_store` opened every store through the constructors the startup
uses, and the audit record with its chain's tail; `check_opens` called it at a clock at which
nothing has ended, so the check forgot nothing the report said was carried (ADR-0023 clause 26). It
was not carried: the store opening has been reworked since. See also
`the-server-opens-its-stores-from-one-list.md`.
