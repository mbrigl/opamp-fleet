# ADR-0008: TOML configuration files for the Server and the Client

- **Status:** 🟢 accepted
- **Date:** 2026-07-22
- **Deciders:** Markus Brigl

## Context

Both binaries need on-disk configuration an operator edits by hand: the Server its listen address
and TLS material, the Client its Server endpoint, transport choice, poll interval, and state
directory — and later the Supervisor definitions of [ADR-0003](0003-client-modes-and-connection-multiplexing.md).
A format has to be chosen once, before the first config file exists, because changing it later
breaks every deployed installation.

Forces:

- **Operators edit these files by hand**, often over SSH on a fleet machine; the format's failure
  modes matter more than its expressiveness. YAML's indentation sensitivity and implicit typing
  (`no` → `false`) are classic sources of silent fleet misconfiguration.
- **The Rust ecosystem is TOML-native**: Cargo itself, `rust-toolchain.toml`, and mature
  first-class `serde` support via the `toml` crate. The project's contributors already read and
  write TOML daily.
- **What the Server *distributes* is not affected.** Remote configuration payloads are the managed
  process's own format (a Collector config stays YAML) — the specification explicitly refuses to
  abstract over those. This decision covers only this project's *own* two config files.
- The Go-side prior art (`opampsupervisor`) uses YAML, as does the wider OpenTelemetry ecosystem —
  a real argument for operator familiarity that has to be weighed.

## Decision

We will use **TOML** for the Server's and the Client's own configuration files, parsed with the
`toml` crate into `serde`-derived structs; each binary takes the file path from a `--config` CLI
flag with a sensible default (`server.toml` / `client.toml` next to the working directory), every
setting has a documented default, and unknown keys are rejected (`deny_unknown_fields`) so a typo
fails loudly at startup instead of silently applying a default.

## Alternatives considered

- **YAML** — the OpenTelemetry ecosystem's format, and what the `opampsupervisor` uses. Rejected
  for *our* files: indentation and implicit-typing failure modes in hand-edited fleet files, and a
  heavier parsing stack (`serde_yaml` is deprecated/unmaintained since 2024). Collector configs the
  Server distributes remain YAML regardless — this decision does not touch them.
- **JSON** — no comments, trailing-comma brittleness; hostile to hand-editing.
- **Environment variables / CLI flags only** — fine for a container one-off, unmanageable for the
  Client's coming per-Supervisor structure; a file is the natural unit an installer lays down and
  an operator diffs. Env-var *overrides* can be layered later if containerized deployments need
  them.

## Sources / Prior art

- [TOML v1.0](https://toml.io/) — explicit typing, comments, no indentation semantics; designed
  exactly for hand-edited configuration.
- [`toml` crate](https://crates.io/crates/toml) — first-class serde integration, actively
  maintained (1.x current on crates.io, checked 2026-07-22).
- [`serde_yaml` deprecation notice](https://github.com/dtolnay/serde-yaml) — the maintained-parser
  situation that weakens the YAML option in Rust.
- [`opampsupervisor` configuration](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the YAML prior art considered and not followed for this project's own files.

## Consequences

- Positive: hand-editable, comment-friendly config with loud failure on typos; one config stack
  (`serde`) shared with the REST API; format familiar to every Rust contributor.
- Positive: TOML's table syntax maps cleanly onto the Client's future growth (`[[supervisor]]`
  blocks per Supervisor, `[gateway]` for Gateway Mode) without a format change.
- Negative / trade-offs: operators coming from the OpenTelemetry ecosystem expect YAML and get a
  second format for the fleet tooling itself. Accepted: the files are small, and the payloads they
  care about daily stay in the agent's own format.
- Follow-ups: where installed services look for the file by default on each OS (path conventions,
  packaging) is decided together with the OS-service work; environment-variable overrides for
  container deployments are a possible later addition.
