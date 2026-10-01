# Walkthrough: rolling a Foreign Agent out from the Server

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md)

This is the end-to-end path for putting a program on every host in the fleet from one place: a
Foreign Agent that speaks no OpAMP, delivered as a package, configured over the protocol, and
updated the same way from then on. It ends with the Server holding both the binary and the
configuration, and no step that has to be repeated per host.

The example is `promtail` — one static binary, which is the requirement (see
[Limits](#limits-worth-knowing-first)). Substitute any single-file agent.

- [Limits worth knowing first](#limits-worth-knowing-first)
- [1. Configure the Supervisor](#1-configure-the-supervisor)
- [2. Build the artifact](#2-build-the-artifact)
- [3. Sign it](#3-sign-it-optional-but-decide-fleet-wide)
- [4. Give it to the Server](#4-give-it-to-the-server)
- [5. Put it in a deployment and sign it there](#5-put-it-in-a-deployment-and-sign-it-there)
- [6. Roll it out](#6-roll-it-out)
- [7. Send its configuration](#7-send-its-configuration)
- [8. Watch it land](#8-watch-it-land)
- [9. Ship an update](#9-ship-an-update)
- [Troubleshooting](#troubleshooting)

## Limits worth knowing first

Three properties of package delivery decide whether this path is open to a given agent.
Reading them first is cheaper than discovering them at rollout time.

- **One file by default, a whole tree when you say so.** A statically linked single binary —
  Promtail, Vector, a Go or Rust agent — needs nothing extra. An agent that is an executable *plus*
  the shared objects it loads, such as Fluent Bit, needs `program_path` in its block, which unpacks
  the whole archive instead of one member; see
  [Agents that are more than one file](client.md#agents-that-are-more-than-one-file). Everything
  below applies to both.
- **`.tar.gz`, `.7z`, and `.zip` are opened.** The Client decides what an artifact is by its
  **leading bytes**, not its name, and anything that is none of the three is taken to *be* the
  program. Only `.7z` may be encrypted; an encrypted `.zip` is refused rather than opened. A
  `.tar.gz` is the right container for a tree that runs on Unix, because it is the one that
  carries file modes.
- **The member name must match the configured program.** The Client looks inside the archive for
  the file name its `[[supervisor]]` block names, wherever the archive keeps it.

## 1. Configure the Supervisor

The program's *written form* is the whole of this host's consent to being updated. A
bare file name puts the program in a directory the Client owns, and that is what makes this Agent
declare `AcceptsPackages`:

```toml
[[supervisor]]
type = "command"
name = "promtail"
command = "promtail"                  # bare: <supervisor_dir>/promtail/program/promtail
args = ["-config.file=${config_dir}/promtail-conf"]
version_args = ["--version"]
```

Two things this block gets right that are easy to get wrong:

- `command = "promtail"` is **not** looked up in `$PATH`. It names a file in that Supervisor's own
  `program/` directory — which is exactly why the Server may write there. An absolute path is
  refused at startup; a bare file name is the only shape a block accepts.
- `${config_dir}` is a placeholder, so the argument cannot drift when `supervisor_dir`
  moves or the Supervisor is renamed. `promtail-conf` is the *name of the Configuration on the
  Server*, which is what the written entry file is called.

**Nothing has to be installed on the host first.** Start the Client with the program absent and the
Agent comes up, connects, and reports `no process installed` — a Supervisor with no process, said
plainly, rather than a spawn error. It declares `AcceptsPackages` all the same, so the first
version arrives the same way every later one does.

**The block does not have to be written on the host either.** A Configuration typed
`supervisor` — the Client's own Agent type — whose body carries this `[[supervisor]]` block rolls
the block itself out to every matching Client, which writes it into its own `supervisor.toml` and
starts the Supervisor — the walkthrough's remaining steps are the same either way. A
Server-delivered block may name its program **only by a bare file name** — one this Client owns and
updates from signed packages; a block that names an absolute path is refused, because
that would let the Server spawn a machine binary that never passed through package signing. An
absolute-path Supervisor is the operator's to write in `supervisor.toml` on the host, not the Server's
to push.

## 2. Build the artifact

**For four agents this step and the next two are one command.**
[`opamp-package-fetch`](tools.md#opamp-package-fetch) knows where the OpenTelemetry Collector
(`otelcol`, `otelcol-contrib`), the GLPI Agent, and Telegraf publish, and asks what it cannot
know — which agent, which of the last five versions, which platforms, and where to upload:

```console
$ opamp-package-fetch
```

It verifies every download against the SHA-256 upstream published, leaves the artifact untouched
wherever upstream's container is one a Client can open — so the hash the fleet verifies is the
one on the release page — and repacks only where upstream ships no installable archive. It never
rolls anything out: reaching a host is still [step 5](#6-roll-it-out). The full
option list is in [the tools page](tools.md#opamp-package-fetch); if you take that route, read
on at [step 5](#6-roll-it-out).

For anything else — your own agent, or a project that tool has never heard of — the rest of this
step is how an artifact is built by hand.

`opamp-package-sign pack` writes the two containers worth writing, with the member named the
way the Supervisor will look for it, and prints the artifact's SHA-256:

```console
$ opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail
wrote promtail-3.0.0.tar.gz holding "promtail" — a Supervisor whose program is named "promtail" installs it
sha256 (hex):
84736f3e2d7d3dc260f172df23063bb044aaa7d576dd8f7b8021a58d6c772461
```

| Option | Meaning |
|---|---|
| `--out <path>` | Where to write the artifact. Required. |
| `--format tar.gz\|7z` | The container. `tar.gz` (the default) is what upstream releases ship and needs no key. |
| `--program-name <name>` | The member name inside the archive. Defaults to the packed file's own name — set it when an upstream build is called `promtail-3.0.0-linux-amd64` but the Supervisor's program is `promtail`. |
| `--archive-key <key>` | AES-256-encrypts a `7z`. The value goes into `[packages] archive_key` on every Client that receives the package; the Server never learns it. |

Only the hex hash goes to stdout, so it composes: `sha=$(opamp-package-sign pack …)`.

**A `.tar.gz` is reproducible.** Modification time, owner, and group are zeroed, so packing the
same program twice gives the same bytes and the same hash. That matters in a system where a hash
decides whether anything is distributed: an artifact differing only by when it was built would be a
rollout nobody asked for. A `.7z` is **not** reproducible — encryption draws a fresh salt each
time, so take the hash from the artifact you actually upload, which is what the command prints.

An encrypted artifact instead:

```console
$ opamp-package-sign pack --format 7z --archive-key "$FLEET_ARCHIVE_KEY" \
      --out promtail-3.0.0.7z ./promtail
```

```toml
# on every Client that receives it
[packages]
archive_key = "…the same value…"
```

Already have an upstream release? Then it is already a `.tar.gz` and needs no repacking — that is
the point of the design. The OpenTelemetry Collector, for instance, publishes its distributions on
the [collector releases page](https://github.com/open-telemetry/opentelemetry-collector-releases/releases),
one artifact per platform named `<distribution>_<version>_<os>_<arch>.tar.gz`:

```console
$ curl -LO https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.109.0/otelcol-contrib_0.109.0_linux_amd64.tar.gz
```

Check what it holds and whether the member name matches your block:

```console
$ tar tzf otelcol-contrib_0.109.0_linux_amd64.tar.gz
$ opamp-package-sign sha256 otelcol-contrib_0.109.0_linux_amd64.tar.gz
```

The member is named after the **distribution** — `otelcol-contrib` in the Contrib archive,
`otelcol` in the core one — so the receiving `[[supervisor]]` block must name its program the
same way (`command = "otelcol-contrib"`, or `binary = …` for a `collector` block), or the
install fails with *"the archive holds no member named …"*. Repack with `--program-name` when
you want a different name on disk.

## 3. Sign it (optional, but decide fleet-wide)

```console
$ opamp-package-sign keygen --out fleet-signing.pk8     # prints the public key (hex)
$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
```

Put the public key in every Client's `[packages] verification_key`. This is fleet-wide by nature:
with a key configured, an **unsigned** package is refused; without one, a **signed** package is
refused. Decide once, for all hosts.

The signature matters more than it looks: the package download route is unauthenticated by design,
so the content hash and this signature — not access control — are what protect an
installed binary.

## 4. Give it to the Server

Create the **Package** — its identity is the Agent type and the version, and nothing else — then
either upload the artifact as an entry, or point the Server at one hosted elsewhere. Nothing in
this step reaches any host, and nothing in the next one does either: a stored Package aims at
nobody at all.

```console
$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \
       http://127.0.0.1:4321/api/v1/packages/promtail/3.0.0
```

The **Agent type** (the first path segment) is compared raw against the `service.name` the Agents
report — for a Supervisor that is its block's `service_name`, or the program's file name when the
block states none. A typo here is a rollout that reaches nobody, which the channel's reach count in
step 6 makes visible. There is no separate package name: what a Package *is* and which release it
belongs to are the whole of its identity.

**Upload** — the artifact is the body; the platform is the path. The Server hashes what it stores,
so no hash is passed here. `os` and `arch` say which machines this artifact runs on: the Server
offers an Agent only the entry built for the platform it reported.

```console
$ curl -X PUT --data-binary @promtail-3.0.0.tar.gz \
       "http://127.0.0.1:4321/api/v1/packages/promtail/3.0.0/entries/linux/amd64"
```

No signature here — it goes on the deployment in step 5. Passing `?signature=` is refused with a
`400` naming the route that takes it, rather than dropped, because a signature silently lost is an
unsigned rollout nobody notices.

Their values are what an Agent reports as `os.type` and `host.arch` — `linux`, `darwin`, `windows`
and `amd64`, `arm64`. The tokens off an upstream release file name work too (`macos` is `darwin`,
`x86_64` is `amd64`), and the response says which canonical pair was stored.

**A fleet on several platforms is still one Package.** Store each build as its own entry, and every
host is offered its own binary:

```console
$ curl -X PUT --data-binary @promtail-3.0.0-linux-arm64.tar.gz \
       "http://127.0.0.1:4321/api/v1/packages/promtail/3.0.0/entries/linux/arm64"
```

**Or reference** — the Server stores the address and *your* SHA-256, offers them verbatim, and
never downloads the artifact. This is where the hash from step 2 is required, because nothing else
stands between the mirror and the fleet:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d "{\"url\": \"https://mirror.example/promtail-3.0.0.tar.gz\", \"sha256\": \"$sha\"}" \
       http://127.0.0.1:4321/api/v1/packages/promtail/3.0.0/entries/linux/amd64/source
```

The Agent type has nothing to do with the member name inside the archive — only the *member* has
to match the configured program.

## 5. Put it in a deployment and sign it there

A **Deployment** is the channel this release goes to: a name, a Selector, the Packages it offers, and
each artifact's signature. It is the only thing that is rolled out.

```console
$ curl -X PUT -H 'Content-Type: application/json' -d '{"selector": {"channel": "beta"}}' \
       http://127.0.0.1:4321/api/v1/deployments/canary
$ curl -X PUT http://127.0.0.1:4321/api/v1/deployments/canary/packages/promtail/3.0.0
$ curl -X PUT -H 'Content-Type: application/json' -d "{\"signature\": \"$sig\"}" \
       http://127.0.0.1:4321/api/v1/deployments/canary/signatures/promtail/3.0.0/linux/amd64
```

Each Selector pair must equal an attribute the Agent reported — `channel` here comes from
`[attributes]` in `supervisor.toml`, or from a label you set on the Server. **The Server prescribes
no key**: `channel` says which stream of versions a host follows, `region` where it runs, `tenant`
whose it is, and all three are yours to invent (see the Server manual, *Channels are a partition*). **The Selector may not
be empty**: an Agent belongs to at most one Deployment, so a channel matching everybody would collide
with every other one. That is the price of the model and it is worth saying out loud — **there is
no "everyone" shortcut.** A host that carries no such value belongs to no Deployment and waits.

The signature belongs here rather than to the artifact because what you are signing off on is a
release to a set of machines. The same Package in two channels is signed in each; a platform left
unsigned is offered unsigned, which a Client with `verification_key` set refuses on arrival.

## 6. Roll it out

**The rollout act is what distributes** — nothing before this press changed any host:

```console
$ curl -X POST http://127.0.0.1:4321/api/v1/deployments/canary/rollout
{"assigned_agents": 3}
```

Check the channel's counts first — they are on `GET /api/v1/deployments` and on its row in the UI.
`claiming_agents` is who is in the channel: `0` means the Selector missed. `targeted_agents` is who
the act would actually move, which is what the press changes. `conflicting_agents` is who this channel
cannot reach because a second Deployment claims them too — resolve that by narrowing a Selector,
because the Server will not pick between two channels.

The act assigns the channel's Package to every Agent it claims, **as the fleet is now**. A host that
enrols (or is labelled into the channel) afterwards is not touched: its row on the Agents tab shows
the Package *waiting*, with a per-Agent **roll out** control — press that, or repeat the channel's
act, when it should follow. To try one host first, use the per-Agent act:

```console
$ curl -X POST -H 'Content-Type: application/json' \
       -d '{"deployment": "canary"}' \
       http://127.0.0.1:4321/api/v1/agents/<instance_uid>/rollout
```

It must be the Deployment that actually claims that Agent, and it is refused while a second one
claims it too — naming one here would sidestep the conflict rather than fix it.

**Finishing the rollout** is giving the stable channel the same version and pressing again. Widening
the canary channel instead is *not* how it ends: both channels would then claim the same hosts, which is
a conflict, not a rollout.

## 7. Send its configuration

The binary and the configuration travel independently — the Supervisor writes every matching
Configuration into its `config/` directory under that Configuration's own name, and the program is
pointed at it by the `-config.file=${config_dir}/promtail-conf` argument from step 1:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "promtail", "selector": {"env": "canary"}, "body": "server:\n  http_listen_port: 9080\n"}' \
       http://127.0.0.1:4321/api/v1/configurations/promtail-conf
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/promtail-conf/rollout
```

The first call only stores — saving never distributes; the second is the rollout act that
releases it to every matching Agent, and the `service_name` keeps the body away from every Agent
that is not a promtail, whatever the Selector says. The Configuration's name and the file name in
the argument are the same string. Change the Configuration and roll it out again: the Supervisor
rewrites the file and restarts the process so it re-reads it. An Agent keeps the exact revision
it was rolled out — an edit waits, visible per Agent on the fleet view, until the next act.

## 8. Watch it land

```console
$ curl -s http://127.0.0.1:4321/api/v1/agents | jq '.[] | select(.service_name=="promtail")'
```

What to look for, in the order it happens:

| Field | What it tells you |
|---|---|
| `capabilities` contains `AcceptsPackages` | The block's program is named the way step 1 describes. If it is missing, nothing else below will happen. |
| `packages[].status` | `Downloading` — with `download_percent` and `download_bytes_per_second`, re-reported every 5 s — then `Installing`, then `Installed` or `InstallFailed`. |
| `packages[].error` | Why an install failed: a missing member, a hash mismatch, a wrong archive key. |
| `packages[].version` | The version the Agent has installed; empty until the first one lands. |
| `service_version` | What `version_args` probed from the program — but **probed once, when the Supervisor started**. After an in-place package update it still shows the version that was running then; `packages[].version` above is the field that tracks what was installed. A Collector reporting through its own `opampextension` is the exception: it re-reports for itself. |
| `healthy`, `health_status` | `no process installed` before the first package; healthy once it runs. |
| `remote_config_status`, `effective_config` | The configuration half of step 6. |

Behind that, on the host: the artifact is streamed to `<supervisor_dir>/promtail/packages/`,
verified, unpacked, swapped over `program/promtail`, made executable, restarted, and **health-gated
on `apply_grace_secs`** — a version that will not stay up is rolled back to its predecessor.

## 9. Ship an update

An update is steps 2, 4, 5 and 6 again with a new version — a new version is a new Package, and the
old one stays in the store beside it. The channel is the constant; **what it holds is what changes**,
which is why the swap is explicit:

```console
$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \
       http://127.0.0.1:4321/api/v1/packages/promtail/3.1.0
$ curl -X PUT --data-binary @promtail-3.1.0.tar.gz \
       "http://127.0.0.1:4321/api/v1/packages/promtail/3.1.0/entries/linux/amd64"
$ curl -X PUT "http://127.0.0.1:4321/api/v1/deployments/canary/packages/promtail/3.1.0?replace=true"
$ curl -X PUT -H 'Content-Type: application/json' -d "{\"signature\": \"$sig\"}" \
       http://127.0.0.1:4321/api/v1/deployments/canary/signatures/promtail/3.1.0/linux/amd64
$ curl -X POST http://127.0.0.1:4321/api/v1/deployments/canary/rollout
```

Without `?replace=true` the third line answers `409` naming 3.0.0 — a channel holds one Package per
Agent type, and swapping the version it runs is meant to be something you say rather than
something that happens.

The channel is untouched, and 3.0.0 stays in the store beside 3.1.0 — but **putting 3.0.0 back in the
channel is not how you take 3.1.0 back**. A Package reaches an Agent only as an upgrade over the version
that Agent reports installed
([ADR-0076](../adr/0076-a-set-reaches-an-agent-only-as-an-upgrade.md)), so once the channel reports
3.1.0 the older one reaches nobody: the channel's act answers `{"assigned_agents": 0}` and the
per-Agent act answers `409`. An Agent that reports no version for the package is measured by the
version it reports *running* instead
([ADR-0079](../adr/0079-the-version-an-agent-runs-stands-in-for-an-unreported-package-version.md)),
so this holds on a host the fleet has never installed anything on — and where an Agent reports both,
the version it reports *running* decides and the package status is not read beside it
([ADR-0083](../adr/0083-what-reaches-an-agent.md) points 2 and 3), so a record left behind by an
install that did not take cannot strand the host.

What takes a bad version back is the host: the version 3.1.0 superseded is kept for
`retain_previous_secs`, and a binary that will not stay up past `apply_grace_secs` is put back
automatically — step 8's health gate. If the new version is bad in a way the health gate cannot
see, publish the old content as a **new, greater version** (`3.1.1`) and roll that out; the fleet
only moves forward.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The Agent never shows `AcceptsPackages` | Every Supervisor declares it, so the Agent is not the one you think it is — check which Supervisor the row belongs to. The startup log states, per Supervisor, what it resolved and what it decided. |
| `InstallFailed`, "holds no member named …" | The archive's member name does not match the configured program. The error lists what the archive *does* hold; repack with `--program-name`. |
| `InstallFailed`, "holds no member at …" | A tree package whose `program_path` names nothing in the archive. The error lists what it holds — check the path from its end, not from the archive root. |
| `InstallFailed`, "matches N members" | `program_path` is ambiguous; write more of the path. |
| `InstallFailed`, "climbs out" / "is an absolute path" / "not a file or a directory" | The archive carries a member this Client will not write — a `..` path, an absolute one, or a link. Nothing was unpacked and the running tree is untouched. |
| An agent shows a package version it is not running | Its record outlived the binary it describes — a version switch that did not take effect, or an older Client reinstalled on top of the state. The fleet reads what the agent reports *running* beside the claim and offers that version again (ADR-0081); the Client drops such a record when it starts. |
| The agent stops starting right after a successful install | The artifact was some container the Client does not open — not gzip, 7z, or zip — so it was installed as if it *were* the program. Repack as `.tar.gz`. |
| `InstallFailed`, "holds an encrypted member" | An encrypted `.zip`. Encryption is the `.7z` format's job, where `[packages] archive_key` opens it; repack, or publish the zip unencrypted. |
| `InstallFailed`, wrong archive key | `[packages] archive_key` is missing or not the one the `.7z` was packed with. |
| A signed package is refused | No `verification_key` on that Client — a Client without one refuses *signed* packages, not only unsigned ones. |
| An Agent that accepts packages is offered nothing | Two Deployments claim it, and an Agent belongs to at most one; `package_conflict` on its fleet row names both. Narrow one Selector. |
| An Agent shows no deployment at all | It carries no attribute any channel's Selector matches. That is the ordinary state right after an enrolment — label it into a channel (`PUT /api/v1/agents/<uid>/labels`) or set `channel` in its `[attributes]`. |
| A package shows `in no deployment` | Stored and unreachable: nothing aims it. Put it in a channel. |
| A channel shows `aims at nobody` | Its Selector names an attribute nobody reports, or a value nobody carries. Compare it against the attributes on the Agents tab. |
| An upload answers `400` about a signature | Signatures belong to the deployment that offers the bytes; the message names the route. |
| The package routes answer `404` | Package delivery is not configured on the Server (`packages_dir`). |
| An upload answers `400`, "invalid platform" | `os`/`arch` are required and must be file-name-safe: lowercase letters, digits and `_`, at most 16 characters. |
| An Agent that accepts packages is offered nothing, and there is no conflict | No artifact for its platform. Check its `os.type` and `host.arch` on its fleet row against the platforms the package holds — this is the case the whole mechanism exists to make visible rather than fatal. |
