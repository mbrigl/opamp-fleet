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
- [5. Aim it](#5-aim-it)
- [6. Send its configuration](#6-send-its-configuration)
- [7. Watch it land](#7-watch-it-land)
- [8. Ship an update, and take it back](#8-ship-an-update-and-take-it-back)
- [Troubleshooting](#troubleshooting)

## Limits worth knowing first

Three properties of package delivery decide whether this path is open to a given agent (ADR-0018).
Reading them first is cheaper than discovering them at rollout time.

- **Exactly one file is installed.** An agent that is an executable *plus* the shared objects it
  loads cannot be delivered as a package. Fluent Bit is the example in
  [`config/client.toml`](../../config/client.toml) that deliberately uses an absolute path for this
  reason. A statically linked single binary — Promtail, Vector, a Go or Rust agent — is fine.
- **Only `.tar.gz` and `.7z` are opened.** The Client decides what an artifact is by its **leading
  bytes**, not its name, and anything that is neither gzip nor 7z is taken to *be* the program. A
  `.zip` is therefore not an unsupported format that gets rejected — it is installed over the
  binary unopened, and the agent no longer starts. There is no ZIP support to enable.
- **The member name must match the configured program.** The Client looks inside the archive for
  the file name its `[[supervisor]]` block names, wherever the archive keeps it.

## 1. Configure the Supervisor

The program's *written form* is the whole of this host's consent to being updated (ADR-0021). A
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
  `program/` directory — which is exactly why the Server may write there. An absolute path would
  make this the machine's program, supervised but never updated.
- `${config_dir}` is a placeholder (ADR-0022), so the argument cannot drift when `supervisor_dir`
  moves or the Supervisor is renamed. `promtail-conf` is the *name of the Configuration on the
  Server*, which is what the written entry file is called.

**Nothing has to be installed on the host first.** Start the Client with the program absent and the
Agent comes up, connects, and reports `no process installed` — a Supervisor with no process, said
plainly, rather than a spawn error. It declares `AcceptsPackages` all the same, so the first
version arrives the same way every later one does.

## 2. Build the artifact

`opamp-package-sign pack` writes the two containers the Client can open, with the member named the
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
the point of ADR-0018. Check what it holds and whether the member name matches your block:

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

The signature matters more than it looks: the package download route is unauthenticated by design
(ADR-0013), so the content hash and this signature — not access control — are what protect an
installed binary.

## 4. Give it to the Server

Either upload the artifact, or point the Server at one hosted elsewhere.

**Upload** — the artifact is the body, its metadata rides the query. The Server hashes what it
stores, so no hash is passed here:

```console
$ curl -X PUT --data-binary @promtail-3.0.0.tar.gz \
       "http://127.0.0.1:4320/api/v1/packages/promtail?version=3.0.0&signature=$sig"
```

**Or reference** (ADR-0018) — the Server stores the address and *your* SHA-256, offers them
verbatim, and never downloads the artifact. This is where the hash from step 2 is required, because
nothing else stands between the mirror and the fleet:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d "{\"url\": \"https://mirror.example/promtail-3.0.0.tar.gz\",
            \"sha256\": \"$sha\", \"version\": \"3.0.0\"}" \
       http://127.0.0.1:4320/api/v1/packages/promtail/source
```

The package **name** (`promtail` in the URL) is the Server's name for the package. It has nothing
to do with the member name inside the archive, and nothing to do with the Supervisor's name — only
the *member* has to match the configured program.

## 5. Aim it

A package with no Selector reaches every Agent that accepts packages. Start narrower:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"env": "canary"}}' \
       http://127.0.0.1:4320/api/v1/packages/promtail/selector
```

Each pair must equal an attribute the Agent reported — `env` here comes from `[attributes]` in
`client.toml`. Where several packages match one Agent the **most specific Selector wins**, which is
how a rollout widens: keep the canary package narrow, add the fleet-wide one, then drop the canary.
Two *equally* specific Selectors reaching one Agent leave it with no offer at all, and its fleet row
says so in `package_conflict`.

## 6. Send its configuration

The binary and the configuration travel independently — the Supervisor writes every matching
Configuration into its `config/` directory under that Configuration's own name, and the program is
pointed at it by the `-config.file=${config_dir}/promtail-conf` argument from step 1:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"env": "canary"}, "body": "server:\n  http_listen_port: 9080\n"}' \
       http://127.0.0.1:4320/api/v1/configurations/promtail-conf
```

The Configuration's name and the file name in the argument are the same string. Change the
Configuration and the Supervisor rewrites the file and restarts the process so it re-reads it.

## 7. Watch it land

```console
$ curl -s http://127.0.0.1:4320/api/v1/agents | jq '.[] | select(.service_name=="promtail")'
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

## 8. Ship an update, and take it back

An update is step 2 and step 4 again with a new version. The Server keeps **one** step of history
(ADR-0019), so a bad version can be re-offered as the old one:

```console
$ curl -X PUT --data-binary @promtail-3.1.0.tar.gz \
       "http://127.0.0.1:4320/api/v1/packages/promtail?version=3.1.0&signature=$sig"
# …and if 3.1.0 turns out badly:
$ curl -X POST http://127.0.0.1:4320/api/v1/packages/promtail/rollback
```

The rollback is an ordinary offer naming the older artifact; the Selector is untouched. A package
that has replaced nothing answers `409`. This is the Server-side undo — distinct from the Client's
own automatic rollback in step 7, which reacts to a binary that will not stay up on one host.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The Agent never shows `AcceptsPackages` | Its program is named by an absolute path, so it is the machine's and is never written to. The startup log states, per Supervisor, what it resolved and what it decided. |
| `InstallFailed`, "holds no member named …" | The archive's member name does not match the configured program. The error lists what the archive *does* hold; repack with `--program-name`. |
| The agent stops starting right after a successful install | The artifact was probably a `.zip` (or some other container): not being gzip or 7z, it was installed as if it *were* the program. Repack as `.tar.gz`. |
| `InstallFailed`, wrong archive key | `[packages] archive_key` is missing or not the one the `.7z` was packed with. |
| A signed package is refused | No `verification_key` on that Client — a Client without one refuses *signed* packages, not only unsigned ones. |
| An Agent that accepts packages is offered nothing | Two equally specific Selectors reach it; see `package_conflict` on its fleet row. |
| The package routes answer `404` | Package delivery is not configured on the Server (`packages_dir`). |
