# The archive installs a raw program too

**Kind:** idea, structure.

**Observed.** `fleet_util::archive::extract` and `extract_tree` choose the container themselves,
but refuse a raw artifact as no archive. So each caller calls `archive::detect` first to install a
raw program its own way: `crates/fleet-agent/src/install.rs` (near line 97, which also names
`TarGz`, `SevenZ` and `Zip`) and `crates/fleet-agent/src/supervisor/process.rs` (near lines 2110
and 2166). An artifact is detected twice, and "raw" is known in three files. The entry points take
positional parameters (`extract_tree` five) where the entry this follows proposed one request with
the program path, the kept links and the key.

**Idea.** The entry points take a raw artifact as the program itself, and the callers stop calling
`detect`; or one request type carries what both entry points need. Either may change what the
operator tool's tests and the fuzz targets call.

**From.** The rest of the backlog entry `the-archive-chooses-its-format-itself.md`, whose entry
points landed on `feat/fleet-util` (2026-11-16); named by the review of that branch at 88320f851.
