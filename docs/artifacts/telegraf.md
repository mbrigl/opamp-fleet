# Artifact: Telegraf

[← User Manual](../manual/README.md) · [The Client](../manual/client.md) ·
[Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how Telegraf is packed, and whoever changes what the `telegraf`
kind knows. It is the one place both sides state the same facts, so a release that moves something
can be answered on both at once ([ADR-0091](../adr/0091-a-kind-knows-its-own-agent.md) clause 9,
[ADR-0094](../adr/0094-telegraf-gets-a-kind-of-its-own.md)).

It is the thinnest of the three artifact documents, because the artifact is installed exactly as
InfluxData published it: there is no repack to keep in step, and no tree whose internal layout
anybody could get wrong.

| | |
|---|---|
| Packed by | `telegraf_plans` in `crates/package-tools/src/bin/opamp-package-fetch.rs` |
| Run by | `crates/client/src/supervisor/telegraf.rs` |
| Agent type | `telegraf` |

## 1. Source

Versions come from the GitHub tags of **`influxdata/telegraf`**, spelled `v<major>.<minor>.<patch>`
— three numeric parts and nothing else, which is what keeps release candidates (`v1.21.0-rc1`) out
of the list an operator picks from.

The **binaries do not live there**. They are served from
`https://dl.influxdata.com/telegraf/releases/`, a CDN with no directory listing, so the platform
list cannot be read from anywhere and is this tool's own (`TELEGRAF_PLATFORMS`).

## 2. Assets per platform

`telegraf-<version>_<os>_<arch>.tar.gz`, and `.zip` for Windows. Upstream spells 32-bit `i386`
where this fleet says `386` ([ADR-0031](../adr/0031-per-platform-package-variants.md)); the mapping is the
third and fourth column of `TELEGRAF_PLATFORMS`:

| This fleet | Upstream |
|---|---|
| `linux/amd64`, `linux/arm64`, `linux/386` | `linux/amd64`, `linux/arm64`, `linux/i386` |
| `darwin/amd64`, `darwin/arm64` | the same |
| `windows/amd64`, `windows/arm64` | the same, as `.zip` |

A URL that has gone away is found by the download failing, not by a check — which is the price of a
list nobody publishes.

## 3. Integrity

`<asset-url>.DIGESTS`, beside each artifact: `sha256sum`-style `<hash>  <name>` lines, and the
artifact's line is looked up **by name** rather than by position.

## 4. Treatment

**As published.** The bytes InfluxData serves are the bytes this fleet distributes; nothing is
unpacked, gathered or rewritten.

## 5. Form in the delivered tree

A **single-file package** ([ADR-0015](../adr/0015-package-delivery-for-managed-processes.md)). The archive wraps
everything in a version-named directory, and the program sits at `usr/bin/telegraf` on Unix but at
the archive root on Windows — and **neither matters**, because the Client finds the member by its
*file name*. That is the whole reason this kind has no `program_path`.

The installed program therefore lands in this Supervisor's own `program/` directory
([ADR-0021](../adr/0021-supervisor-directory-and-path-implied-package-consent.md)), which is also where the process
starts ([ADR-0091](../adr/0091-a-kind-knows-its-own-agent.md)).

## 6. What the Client derives

| | Unix | Windows |
|---|---|---|
| program | `telegraf` | `telegraf.exe` |
| `program_path` | — (single file) | — |
| `service_name` | `telegraf` | `telegraf` |
| arguments | `--config <config_dir>/telegraf-conf` | the same |
| version | `--version`, read as strict SemVer | the same |
| preflight | `--version` against the staged program | the same |
| reload | `SIGHUP` | restart |
| `endpoint_port` | refused — Telegraf speaks no OpAMP to us | refused |

The **reload** is why this is a kind at all. `SIGHUP` is Telegraf's documented way of re-reading its
configuration — the same signal `systemctl reload` sends — and as a block key it was refused on
Windows at parse time, so one Supervisor set could not serve a mixed fleet. Here the signal is used
where signals exist and the Runner's restart stands in where they do not, with nothing said in the
block.

The **version arguments serve twice**: as the probe that gives the Agent its `service.version`, and
as the preflight run against a *staged* program before the running one is stopped
([ADR-0068](../adr/0068-icinga-2-is-supervised-by-a-kind-of-its-own.md)). What makes them a version
probe is what makes them a safe check — cheap, and touching no state.

## 7. Configurations

| Name | Aimed at | Body |
|---|---|---|
| `telegraf-conf` | every Agent of type `telegraf` | `config/examples/telegraf-conf.toml` |

`opamp-package-fetch` uploads it beside the package when the Server has none of that name yet. The
Configuration's name is also the file name its entry gets in the Supervisor's `config/` directory,
which is what `--config` points at — so **the name is not decoration**: renaming it on the Server
without renaming `CONFIG_ENTRY` here leaves Telegraf pointed at a file that does not exist.

## 8. What can change, and what goes red

| An upstream that… | breaks | caught by |
|---|---|---|
| renames its assets or changes the `_<os>_<arch>` spelling | the download | `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` (packing side) |
| moves or renames `.DIGESTS` | the checksum | the same test's `ChecksumSource` assertion |
| renames the program inside the archive | the install, silently — no member matches | `the_defaults_are_the_artifacts` (client side) |
| stops reloading on `SIGHUP` | every apply, invisibly: the process keeps its old configuration | nothing here — it is a documented property, and this row is the warning |
| stops printing a plain SemVer version | `service.version`, and the preflight | `the_defaults_are_the_artifacts` does not cover it; the Agent reports no version |

The last two rows are honest rather than reassuring: a test can pin what this repository writes
down, not what a program does at run time.
