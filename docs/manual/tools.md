# The operator command-line tools

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[Rollout walkthrough](rollout.md)

Two programs ship beside the Server and the Client, for the operator who has to get software
*into* the fleet. Neither is part of the running system: nothing the Server or a Client does
depends on them, and a fleet can be operated entirely through the REST API without them.

| Tool | Use it to |
|---|---|
| **[`opamp-package-fetch`](#opamp-package-fetch)** | fetch a release of a known agent — the OpenTelemetry Collector, the GLPI Agent, Telegraf — verify it, and hand it to the Server |
| **[`opamp-package-sign`](#opamp-package-sign)** | build, hash, and sign a package artifact out of *any* program you have |

Reach for `opamp-package-fetch` first: for the agents it knows, it replaces every step of
building an artifact by hand. `opamp-package-sign` is what serves everything else — your own
agent, an internal build, a project the other tool has never heard of — and it is also where
signing lives, which `opamp-package-fetch` deliberately does not do.

## Running them

Both live in their own crate, `package-tools`, so neither is part of what runs on a managed host
([ADR-0065](../adr/0065-the-operator-package-tools-live-in-their-own-crate.md)). From a source
checkout:

```console
$ cargo run --bin opamp-package-fetch -- --agent telegraf --no-upload
$ cargo run --bin opamp-package-sign -- pack --out promtail-3.0.0.tar.gz ./promtail
```

`--bin` resolves across the workspace, so naming the crate is optional; `-p package-tools` is
there when you want to be explicit.

**A release does not ship them.** The `.7z` artifacts and the `.deb`/`.rpm`/`.msi` installers
carry the Client and nothing else, because these tools run wherever an operator works rather than
on a managed host. Build them once from a checkout and put them on your `PATH`:

```console
$ cargo build --release --bin opamp-package-fetch --bin opamp-package-sign
$ ./target/release/opamp-package-fetch
```

Everything below writes its **status to stderr** and its **result to stdout**, so a hash or a
signature composes into a shell variable while the running commentary stays on the terminal:

```console
$ sha=$(opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail)
```

## `opamp-package-fetch`

### What it does

For four agent types it knows where the release lives, what the assets are called, and which
checksum file goes with them:

| `--agent` | Where it fetches from | What arrives |
|---|---|---|
| `otelcol` | `open-telemetry/opentelemetry-collector-releases` | the core Collector's `.tar.gz`, as published |
| `otelcol-contrib` | the same repository | the Contrib Collector's `.tar.gz`, as published |
| `glpi-agent` | `glpi-project/glpi-agent` | Windows: the portable `.zip`, as published · Linux: a `.tar.gz` repacked from the AppImage |
| `telegraf` | `dl.influxdata.com` (versions from `influxdata/telegraf`) | the `.tar.gz`, or the `.zip` on Windows, as published |

Three things it does on every run:

- **It verifies the download against the SHA-256 upstream published**, before the artifact is
  used for anything. A mismatch stops that platform and leaves the file for inspection.
- **It leaves the artifact alone** wherever upstream's container is one a Client can open — so
  the hash the fleet verifies is the hash on the release page, and the line from the release to
  the host is unbroken ([ADR-0018](../adr/0018-packages-imported-from-a-url.md)).
- **It never distributes anything.** Uploading stores a Set; reaching a host is the rollout act,
  which stays yours ([step 5 of the walkthrough](rollout.md#5-aim-it-then-roll-it-out)).

### Interactive

Run it with no arguments and it asks for what it cannot know:

```console
$ opamp-package-fetch
Which agent› otelcol-contrib
Reading the last releases of open-telemetry/opentelemetry-collector-releases …
Which version› 0.158.0
Which platforms (space to select, enter to confirm)
  [x] linux/amd64
  [x] windows/amd64
  [ ] darwin/arm64
Upload these to a fleet Server? yes
Server base URL› http://127.0.0.1:4321
```

The version list is the **five most recent releases**, newest first — enough to reach the one
before last when a release turns out badly. Release candidates and the tags a repository keeps
for other things (the Collector repository tags its builder alongside) are filtered out.

The platform list is what *that release actually publishes*, read from its assets rather than
from a table that could go stale — so a platform upstream added appears without this tool being
changed, and one it dropped simply is not offered. A platform this fleet has no name for
(`linux/s390x`, say) is left out, because a package entry no Agent can match is one nobody would
ever be offered.

### Non-interactive

Every prompt has a flag; give all of them and nothing is asked:

```console
$ opamp-package-fetch --agent telegraf --version 1.39.3 \
      --platform linux/amd64 --platform windows/amd64 \
      --out-dir ./artifacts --server http://127.0.0.1:4321
```

| Option | Meaning |
|---|---|
| `--agent <name>` | `otelcol`, `otelcol-contrib`, `glpi-agent`, or `telegraf`. |
| `--version <v>` | The version **as upstream numbers it** — `0.158.0`, `1.19` — never the tag (`v0.158.0`). Omitted, the last five are offered. |
| `--platform <os/arch>` | Repeatable. `linux/amd64`, `windows/amd64`, `darwin/arm64`, … A platform the release does not publish is refused with the list of those it does. |
| `--out-dir <path>` | Where artifacts are written. Created if missing. Defaults to the working directory. |
| `--server <url>` | Create the Set and upload each artifact as its platform's entry. Cannot be combined with `--no-upload`. |
| `--no-upload` | Write the artifacts and stop, without the upload question. |

`--version` names the *release to fetch*, not this tool's own version — `--version 1.19`, not a
bare `--version`.

### What it prints at the end

The block each artifact needs, because that differs per agent and platform and is the next thing
you need:

```
Done. What a Supervisor needs to install these:
  linux/amd64  ./artifacts/telegraf-1.39.3_linux_amd64.tar.gz
      command = "telegraf"
  windows/amd64  ./artifacts/telegraf-1.39.3_windows_amd64.zip
      command = "telegraf.exe"
```

With `--no-upload` (or a declined prompt) it also prints the two `curl` calls that would have
uploaded the artifacts, so the step can be done later or from somewhere else.

### When an upload is refused

The Server decides some uploads before it reads a byte of the artifact, and says which:

| What it answers | What to do |
|---|---|
| `409 … immutable` | The Set is already rolled out to an Agent, so its entries are fixed ([ADR-0061](../adr/0061-a-rollout-is-an-explicit-act.md)). Fetch under a new version, or delete the Set first. |
| `507 … max_total_package_bytes` | The package store is at its ceiling. Delete a Set you no longer roll out, or raise the limit ([the Server's configuration reference](server.md#top-level)). |
| `404 … not configured` | The Server has no package store: `packages_dir` is unset in `server.toml` ([Packages](server.md#packages-distributing-software)). |
| `413 …` | The artifact is past `max_package_size_bytes`. |

Because the refusal arrives while the artifact is still being sent, the connection can reset before
the answer is read; the tool then asks the Server once more with an empty body to recover the reason,
so what you see is the status and message above rather than a bare connection error. A message that
does still begin `cannot reach` is what it says — the Server was not answering — and it carries the
underlying cause (DNS, refused connection, TLS) rather than only the request that failed.

### What it does not do

- **It does not sign.** Signing needs a key, and where that key lives is a decision a tool should
  not make silently — use [`opamp-package-sign sign`](#opamp-package-sign) and pass the signature
  on the upload's `signature` query parameter.
- **It does not roll out.** See above; that act is the operator's.
- **It does not aim.** A Set arrives with no Selector, which means *every* Agent of its type once
  rolled out. Narrow it first if that is not what you want
  ([step 5](rollout.md#5-aim-it-then-roll-it-out)).

### Prerequisites and limits

- **Network egress** to `api.github.com`, to the release asset host, and — for Telegraf — to
  `dl.influxdata.com`.
- **GitHub rate-limits unauthenticated requests to 60 per hour** per address. A run costs two
  requests (one to list versions, one to read a release), so this is only reached by scripting.
  The error says so rather than leaving you guessing.
- **The GLPI Linux repack needs Linux x86_64**, because extracting an AppImage means running it.
  Every other agent, platform, and step runs anywhere the tool builds. Fetch the Windows artifact
  wherever you like and the Linux one on a Linux host.
- **Only amd64 and arm64 (and 386 where published)** are named, matching what Agents report as
  `os.type` and `host.arch`.

## `opamp-package-sign`

For a program the other tool does not know: it builds the container, and it owns signing.

```console
$ opamp-package-sign keygen --out fleet-signing.pk8   # prints the public key (hex)
$ sha=$(opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail)
$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
```

| Command | What it prints | Use it for |
|---|---|---|
| `keygen --out <file>` | the **public** key (hex) | the value for every Client's `[packages] verification_key`; the private key goes in the file, ideally not on the Server host |
| `pack <program> --out <file>` | the artifact's SHA-256 (hex) | building a one-file artifact; `--format tar.gz\|7z`, `--program-name <name>`, `--archive-key <key>` |
| `sign --key <file> <artifact>` | the signature (hex) | the `signature` query parameter of the upload |
| `public-key --key <file>` | the public key (hex) | recovering it from an existing private key |
| `sha256 <artifact>` | the SHA-256 (hex) | the `sha256` of a *referenced* entry, for an artifact the Server never holds |

The options of `pack`, and why a `.tar.gz` packed twice gives the same bytes, are in
[step 2 of the rollout walkthrough](rollout.md#2-build-the-artifact), which runs the whole path
from packing to a running agent.

**Signing is fleet-wide, and it cuts both ways.** With `[packages] verification_key` configured,
a Client refuses an *unsigned* package; without it, it refuses a *signed* one. Decide once, for
all hosts.
