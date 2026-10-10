# ADR-0026: Telegraf gets a kind of its own that reloads it the platform's way, and its package is upstream's single-file archive as published

- **Status:** 🟢 accepted
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/telegraf.rs, the `telegraf` feature of crates/fleet-agent/Cargo.toml, telegraf_plans and TELEGRAF_PLATFORMS in opamp-fleetctl package fetch, config/examples/telegraf-conf.toml, docs/artifacts/telegraf.md

## Context

The fleet manages InfluxData's Telegraf beyond the Collector. It should be an ordinary Managed
Process: a package the Client installs into its own Supervisor directory, updates with a health
gate and rolls back, a Configuration rolled out from the Server, a restart issued from it. A kind
knows its own agent
([ADR-0016](0016-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)): what
follows from the artifact and the platform is compiled in, and a block holds only what an operator
decides.

Telegraf is a single-file program. Its whole invocation is Telegraf's own: the program name,
`--config`, `--version`, and `SIGHUP` to re-read its configuration. The reload is the sharpest case
for a kind: a reload signal is Unix-only, so a block naming one is refused on Windows and no one
block could serve a mixed fleet.

## Decision

We will supervise Telegraf with a `telegraf` kind whose block is `type` and `name` alone, reload it
the way its platform allows, and deliver it as upstream's own archives, installed as published.

1. **The `telegraf` kind knows the invocation and has no settings.** Program `telegraf` plus
   `EXE_SUFFIX`; arguments `--config ${config_dir}/telegraf-conf`; `--version` as version probe
   and preflight, read as strict SemVer; `service_name = "telegraf"`. The block is
   `type = "telegraf"` and `name`; a block carrying `args`, `version_args`, `reload_signal`, `env`
   or `working_dir` fails at startup naming what supplies the value.

2. **Telegraf's reload is the kind's, platform-correct by construction:** `SIGHUP` on Unix, the
   Runner's restart on Windows. Neither is written in a block, so one Supervisor set serves a mixed
   fleet.

3. **Telegraf is a single-file package, installed as published.** Assets are
   `telegraf-<version>_<os>_<arch>.tar.gz` (`.zip` for Windows, opened as
   [ADR-0024](0024-packages-signed-deployments-offered-downloads-and-verified-delivery.md) clause 66
   decides) from `https://dl.influxdata.com/telegraf/releases/`, verified against the `.DIGESTS`
   file beside each asset, its line looked up by name. Versions are the
   `v<major>.<minor>.<patch>` tags of `influxdata/telegraf`. The CDN has no listing, so the
   platform list is the tool's own (`TELEGRAF_PLATFORMS`: linux amd64/arm64/386, darwin
   amd64/arm64, windows amd64/arm64; upstream spells `386` as `i386`). The member is found by its
   file name, so where it sits in the archive does not matter and the kind has no `program_path`.

4. **Telegraf does not speak OpAMP to the Client.** The kind refuses an `endpoint_port`; its
   Endpoint is bound and nothing connects.

5. **The Configuration name is part of the contract.** `opamp-fleetctl package fetch` uploads
   `telegraf-conf` (body `config/examples/telegraf-conf.toml`) beside the package when the Server
   holds none of that name, never overwriting an existing one and distributing nothing. The name is
   the file name the kind points its agent at, so renaming it on one side only leaves the agent
   reading a file that does not exist.

6. **The kind has an artifact document held by two tests.** `docs/artifacts/telegraf.md` states
   source, assets, integrity, treatment, form in the tree, what the Client derives, and what goes
   red when upstream moves something — one test on the packing side, one on the client side,
   `cfg`-gated per platform where the facts differ.

**Out of scope:** nothing beyond the sentence above.

## Alternatives considered

- **A `command` recipe per platform.** Rejected: the block cannot be written as one block for a
  mixed fleet at all, because its reload signal is refused on Windows.
- **One block with placeholders** (`${exe_suffix}` and friends). Rejected: it turns the operator's
  file into a template of the artifact's layout.
- **Let the kind accept `args`** as an escape hatch. Rejected: a kind that needs one does not know
  its agent; an operator's choices belong in the agent's own configuration, and `command` remains
  for a genuinely different invocation.
- **A generic "single-file agent" kind** parameterised by name. Rejected: its parameters would be
  the keys the `telegraf` kind removes — `command` under a shorter name.
- **A kind that drives the native service managers** (`systemctl`, `sc.exe`). Rejected: supervising
  a process the Client did not spawn means no watchdog, no health gate, no bounded stop.

## Sources / Prior art

- [Telegraf configuration](https://docs.influxdata.com/telegraf/v1/configuration/) — `--config`,
  and reload on `SIGHUP`.
- InfluxData's release archives at `dl.influxdata.com` with their `.DIGESTS` files.

## Consequences

- Positive: one block for every platform and for a mixed fleet; the reload is right everywhere
  without the operator choosing, and keeps Telegraf's state across a configuration change where
  signals exist.
- Positive: the artifact travels exactly as upstream published it, verified against upstream's own
  digest.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests.

## Enforcement

- `crates/fleet-agent/src/supervisor/telegraf.rs`: `the_block_has_no_settings`,
  `the_recipes_keys_are_refused_by_name`, `the_invocation_points_at_the_delivered_configuration`,
  `the_defaults_are_the_artifacts` (the client side of clause 6).
- `crates/fleet-agent/tests/wrapped_supervisors.rs`:
  `a_two_line_telegraf_block_runs_reports_and_applies` — the two-line block runs, reports its
  version, receives a Configuration, and finds its directories made.
- `crates/fleet-tools/src/fetch.rs`:
  `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` (the packing side of
  clause 6), `a_published_checksum_is_read_from_either_shape`,
  `every_agent_carries_a_storable_default_configuration`,
  `a_default_configuration_the_server_already_holds_is_not_written_over`.

**Not mechanically decidable:** whether upstream still reloads Telegraf on `SIGHUP` is a run-time
property no repository test can see; the artifact document names this row as a warning.
