# ADR-0094: Telegraf gets a kind of its own

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Applies [ADR-0091](0091-a-kind-knows-its-own-agent.md) to the third documented agent. The rule, the
`Plugin` seam and the artifact-document requirement are stated there.

## Context

**Telegraf's block is five keys, and every one of them is a property of Telegraf.**
`config/supervisor.toml` documents it as a `command` Supervisor:

```toml
command = "telegraf"
args = ["--config", "${config_dir}/telegraf-conf"]
version_args = ["--version"]
reload_signal = "HUP"
```

The program name is what InfluxData's archive contains; `telegraf-conf` is the Configuration name
`opamp-package-fetch` uploads; `--version` is how Telegraf answers for itself; and `SIGHUP` is
Telegraf's documented way of re-reading its configuration.

**One of those five is worse than transcribed — it is platform-wrong whichever way it is written.**
`reload_signal` is Unix-only: on Windows the key is refused at parse time, so the same block cannot
serve both platforms, and a fleet-wide Supervisor set carrying it would be rejected by every Windows
host. That is the general argument of ADR-0091 in its sharpest form: a value the operator cannot get
right on both platforms is not a value the operator should be writing.

**The package is a single file**, so unlike Icinga 2 and the GLPI Agent there is no tree, no
`program_path`, and nothing about an internal layout to keep in step — the Client finds the member
by its file name and installs the upstream archive as published.

## Decision

We will add a **`telegraf` kind**, and its block will be two lines.

1. **The kind knows the invocation**: the program name (`telegraf` + `EXE_SUFFIX`), `--config`
   against `telegraf-conf`, `--version` as the way it is asked for its version, and
   `service_name = "telegraf"`.

2. **The reload is the kind's, and platform-correct by construction**: `SIGHUP` on Unix, and a
   restart on Windows, where no signal exists. Neither is written in a block, so one Supervisor set
   serves both platforms — which the current shape cannot.

3. **It has no settings of its own**, exactly as `glpi` has none: the strict parse accepts an empty
   table and refuses every key, naming what supplies the value now.

   ```toml
   [[supervisor]]
   type = "telegraf"
   name = "telegraf"
   ```

4. **Its artifact document is `docs/artifacts/telegraf.md`**, in ADR-0091 clause 9's shape. It is
   the thinnest of the three — the artifact is installed as published, so what the document pins is
   the asset naming (`telegraf-<version>_<os>_<arch>.{tar.gz,zip}`), the `.DIGESTS` checksum source,
   and the fact that the program sits at `usr/bin/telegraf` on Unix and at the archive root on
   Windows, which does not matter to a single-file package precisely because the member is found by
   name. Both tests ADR-0091 requires still apply: an upstream that renames its assets or moves its
   digests should turn a test red, not a rollout.

## Alternatives considered

- **Leave Telegraf as a `command` recipe.** Cheapest, and it works today on Unix. Rejected on the
  Windows argument above: the recipe is not merely repetitive, it is unwritable as one block for a
  mixed fleet.

- **Keep the kind but let it accept `args`.** A Telegraf someone wants to run with `--watch-config`
  or a second `--config` would then need no code change. Rejected for the reason ADR-0091 gives for
  every wrapper: an escape hatch in a kind that claims to know its agent is an admission that it
  does not — and Telegraf's own configuration is where an operator's choices belong. `command`
  remains for an installation that genuinely needs a different invocation.

- **Fold Telegraf into a generic "single-file agent" kind** parameterised by name, alongside future
  ones. Tempting, and it is what `command` already is. Rejected: the parameters would be exactly the
  five keys this ADR removes, so the kind would be `command` with a shorter name.

## Sources / Prior art

- **Telegraf configuration** — the configuration is loaded from the path in `--config` (or
  `TELEGRAF_CONFIG_PATH`, or a platform default), and *"configuration can be reloaded by sending
  SIGHUP"* — the same signal `systemctl reload` sends
  ([InfluxData docs](https://docs.influxdata.com/telegraf/v1/configuration/)). Both facts are what
  this kind compiles in, and both are Telegraf's, not this fleet's.
- **InfluxData's release archives** — `telegraf-<version>_<os>_<arch>.{tar.gz,zip}` from
  `dl.influxdata.com`, with the `.DIGESTS` file beside each: the asset naming and checksum source
  the artifact document pins.
- **`crates/package-tools/src/bin/opamp-package-fetch.rs`** — `telegraf_plans` (the asset naming, the
  digest source, the `AsPublished` action, and the `block_hint` this ADR shrinks) and the `AgentKind`
  entry naming the Configuration `telegraf-conf`.
- **`config/supervisor.toml`, block 4** — the five-key block quoted in the Context, including the
  note that `reload_signal` is unix-only.

## Consequences

- **Positive: one block for a mixed fleet.** The reload becomes right on both platforms without the
  operator choosing, which no `command` block can achieve.
- **Positive: the cheapest wrapper of the three.** Nothing about an internal tree, so the document
  and its tests cover the packaging end almost alone.
- **Negative: a third plugin to keep.** Small, but it is code, a document and two tests — the price
  ADR-0091 states for every wrapper, paid here for an agent whose block was already short.
- **Negative: an operator wanting a different Telegraf invocation drops back to `command`** and
  writes the whole thing again, including the parts this kind got right.
