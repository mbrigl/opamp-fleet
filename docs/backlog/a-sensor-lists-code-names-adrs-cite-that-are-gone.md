# A sensor lists the code names an ADR cites that the tree no longer has

**Kind:** idea, sensor.

**Observed.** A drafted restatement of an ADR kept describing a constant (`SHUTDOWN_DRAIN`) after
the code on its branch had removed it; only a fresh review caught it. An ADR in force that names a
constant, a function or a type the tree no longer has is stale or worked around, and no check sees
it. `main` has no such sensor under `scripts/`.

**What the old branch had.** Revision ticket 21 of `feat/revision-2026-10-31`, open, not worked: a
sensor that reads the identifiers in backticks of each ADR in force and each draft, those shaped
like Rust constants, types or paths, and lists every one that no file under `crates/` defines. It
reports to the reviewer and fails nothing, as the other sensors.
