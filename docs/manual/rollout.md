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
- [Troubleshooting](#troubleshooting)

## Limits worth knowing first

Reading them first is cheaper than discovering them at rollout time.

- **One file by default, a whole tree when you say so.** A statically linked single binary —
  Promtail, Vector, a Go or Rust agent — needs nothing extra. An agent that is an executable *plus*
  the shared objects it loads, such as Fluent Bit, needs `program_path` in its block, which unpacks
  [Agents that are more than one file](client.md#agents-that-are-more-than-one-file). Everything
  below applies to both.
- **The member name must match the configured program.** The Client looks inside the archive for
  the file name its `[[supervisor]]` block names, wherever the archive keeps it.

## 1. Configure the Supervisor

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
that would let the Server spawn a machine binary that never passed through package signing. An
absolute-path Supervisor is the operator's to write in `supervisor.toml` on the host, not the Server's
to push.

## 2. Build the artifact

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

```console
$ tar tzf otelcol-contrib_0.109.0_linux_amd64.tar.gz
$ opamp-package-sign sha256 otelcol-contrib_0.109.0_linux_amd64.tar.gz
```

## 3. Sign it (optional, but decide fleet-wide)

```console
$ opamp-package-sign keygen --out fleet-signing.pk8     # prints the public key (hex)
$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
```

Put the public key in every Client's `[packages] verification_key`. This is fleet-wide by nature:
with a key configured, an **unsigned** package is refused; without one, a **signed** package is
refused. Decide once, for all hosts.

installed binary.

## 4. Give it to the Server

either upload the artifact as an entry, or point the Server at one hosted elsewhere. Nothing in

```console
$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \
```



```console
$ curl -X PUT --data-binary @promtail-3.0.0.tar.gz \
```

Their values are what an Agent reports as `os.type` and `host.arch` — `linux`, `darwin`, `windows`
and `amd64`, `arm64`. The tokens off an upstream release file name work too (`macos` is `darwin`,
`x86_64` is `amd64`), and the response says which canonical pair was stored.

host is offered its own binary:

```console
$ curl -X PUT --data-binary @promtail-3.0.0-linux-arm64.tar.gz \
```

       -d "{\"url\": \"https://mirror.example/promtail-3.0.0.tar.gz\", \"sha256\": \"$sha\"}" \
```




```console
```


**The rollout act is what distributes** — nothing before this press changed any host:

```console
{"assigned_agents": 3}
```


```console
$ curl -X POST -H 'Content-Type: application/json' \
       http://127.0.0.1:4321/api/v1/agents/<instance_uid>/rollout
```


The binary and the configuration travel independently — the Supervisor writes every matching
Configuration into its `config/` directory under that Configuration's own name, and the program is
pointed at it by the `-config.file=${config_dir}/promtail-conf` argument from step 1:
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



```console
$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \
$ curl -X PUT --data-binary @promtail-3.1.0.tar.gz \
```

that Agent reports installed
per-Agent act answers `409`. An Agent that reports no version for the package is measured by the
version it reports *running* instead
([ADR-0014](../adr/0014-rollout-and-what-reaches-an-agent.md)),
so this holds on a host the fleet has never installed anything on — and where an Agent reports both,
the version it reports *running* decides and the package status is not read beside it
([ADR-0014](../adr/0014-rollout-and-what-reaches-an-agent.md) points 2 and 3), so a record left behind by an
install that did not take cannot strand the host.

What takes a bad version back is the host: the version 3.1.0 superseded is kept for
`retain_previous_secs`, and a binary that will not stay up past `apply_grace_secs` is put back
see, publish the old content as a **new, greater version** (`3.1.1`) and roll that out; the fleet
only moves forward.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `InstallFailed`, "holds no member named …" | The archive's member name does not match the configured program. The error lists what the archive *does* hold; repack with `--program-name`. |
| `InstallFailed`, "holds no member at …" | A tree package whose `program_path` names nothing in the archive. The error lists what it holds — check the path from its end, not from the archive root. |
| `InstallFailed`, "matches N members" | `program_path` is ambiguous; write more of the path. |
| `InstallFailed`, "climbs out" / "is an absolute path" / "not a file or a directory" | The archive carries a member this Client will not write — a `..` path, an absolute one, or a link. Nothing was unpacked and the running tree is untouched. |
| An agent shows a package version it is not running | Its record outlived the binary it describes — a version switch that did not take effect, or an older Client reinstalled on top of the state. The fleet reads what the agent reports *running* beside the claim and offers that version again (ADR-0014); the Client drops such a record when it starts. |
| `InstallFailed`, wrong archive key | `[packages] archive_key` is missing or not the one the `.7z` was packed with. |
| A signed package is refused | No `verification_key` on that Client — a Client without one refuses *signed* packages, not only unsigned ones. |
| The package routes answer `404` | Package delivery is not configured on the Server (`packages_dir`). |
| An upload answers `400`, "invalid platform" | `os`/`arch` are required and must be file-name-safe: lowercase letters, digits and `_`, at most 16 characters. |
| An Agent that accepts packages is offered nothing, and there is no conflict | No artifact for its platform. Check its `os.type` and `host.arch` on its fleet row against the platforms the package holds — this is the case the whole mechanism exists to make visible rather than fatal. |
