# The operator tool

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[Rollout walkthrough](rollout.md)

One program, `opamp-fleetctl`, ships beside the Server and the Client for the operator who sets a
fleet up and gets software *into* it. It is not part of the running system: nothing the Server or a
Client does depends on it, and it belongs on the operator's own machine, not on the Server host —
the keys it makes are the ones to keep off that host
([ADR-0028](../adr/0028-the-operator-tools-are-one-program-released-for-linux-and-macos.md)).

| Command | Use it to |
|---|---|
| **[`package fetch`](#package-fetch)** | fetch a release of a known agent — the OpenTelemetry Collector or this fleet's own Client — verify it, and hand it to the Server with its default configuration |
| **[`package pack`, `sha256`, `keygen`, `sign`, `public-key`](#packing-and-signing)** | build, hash, and sign a package artifact out of *any* program you have, and make the package signing key |
| **[`pki init`, `server-cert`, `bootstrap-cert`, `status`](#pki)** | make the fleet's certificate authorities, the Server's or a Gateway's certificate and the bootstrap certificate, and say when they end |

Reach for `package fetch` first: for the agents it knows, it replaces every step of building an
artifact by hand. The other `package` commands serve everything else — your own agent, an internal
build, a project `fetch` has never heard of — and they are also where signing lives, which `fetch`
deliberately does not do.

## Getting it

Every release publishes it for Linux and macOS, on amd64 and arm64, as
`opamp-fleetctl_<version>_<os>_<arch>.tar.gz` beside the Client's assets and covered by the same
`SHA256SUMS`; compare the archive's hash with its line there (`sha256sum`, or `shasum -a 256` on
macOS). The archive holds the one program, executable:

```console
$ tar xzf opamp-fleetctl_<version>_linux_amd64.tar.gz
$ ./opamp-fleetctl --version
```

These archives are downloaded, never uploaded to a Server: they are not packages of the fleet, and
no Client installs one. There is no Windows build; on Windows, build it from a checkout:

```console
$ cargo build --release --bin opamp-fleetctl
$ ./target/release/opamp-fleetctl --help
```

`cargo run --bin opamp-fleetctl -- <command>` runs it from a checkout without installing it.

Every command writes its **status to stderr** and its **result to stdout**, so a hash, a key or a
signature composes into a shell variable while the running commentary stays on the terminal:

```console
$ sha=$(opamp-fleetctl package pack --out promtail-3.0.0.tar.gz ./promtail)
```

## `package fetch`

### What it does

For three agent types it knows where the release lives, what the assets are called, and which
checksum file goes with them:

| `--agent` | Where it fetches from | What arrives |
|---|---|---|
| `otelcol` | `open-telemetry/opentelemetry-collector-releases` | the core Collector's `.tar.gz`, as published |
| `otelcol-contrib` | the same repository | the Contrib Collector's `.tar.gz`, as published |
| `supervisor` | `mbrigl/opamp-fleet` — this project's own releases | the `.tar.gz` this fleet's Client is released as, one per platform, as published. It is the package a Client updates *itself* from ([ADR-0018](../adr/0018-the-client-updates-itself-under-its-own-agent-type.md)); the `.deb`, `.rpm` and `.msi` beside it are for installing a Client by hand and are passed over |

Four things it does on every run:

- **It verifies the download against the SHA-256 upstream published**, before the artifact is
  used for anything. A mismatch stops that platform and leaves the file for inspection.
- **It leaves the artifact alone** wherever upstream's container is one a Client can open — so
  the hash the fleet verifies is the hash on the release page, and the line from the release to
  the host is unbroken ([ADR-0026](../adr/0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md)).
- **It uploads the agent's default configuration with the package** — but only the ones the
  Server does not already have, see [below](#the-default-configuration).
- **It never distributes anything.** Uploading stores a Package and saves a Configuration; reaching a
  host is the rollout act, which stays yours
  ([step 6 of the walkthrough](rollout.md#6-roll-it-out)).

### Interactive

Run it with no arguments and it asks for what it cannot know:

```console
$ opamp-fleetctl package fetch
Which agent:
  otelcol          linux, darwin, windows — as the release publishes
> otelcol-contrib  linux, darwin, windows — as the release publishes
  supervisor       linux/amd64+arm64 darwin/arm64+amd64 windows/amd64
Reading the last releases of open-telemetry/opentelemetry-collector-releases …
Which version› 0.158.0
Which platforms (space to select, enter to confirm)
  [x] linux/amd64
  [x] windows/amd64
  [ ] darwin/arm64
Upload these to a fleet Server? yes
Server base URL› https://127.0.0.1:4321
```

The systems beside each agent are what it is published for — enough to see before the choice that
an agent offers nothing for the hosts you run. They are a hint, not the offer: the platform
question below shows what *that release* actually has. One needs a word:

- The **Collectors** add platforms between releases, so only their operating systems are named
  here; the architectures come from the release itself a step later.

The version list is the **three most recent release series**, newest first — a series being a
`major.minor` line, of which only its newest patch is offered. That is the difference between a
list you can go back through and one you cannot. An
older patch of a series already on the list is not a choice anyone makes, so it is not offered.
Release candidates and the tags a repository keeps for other things
(the Collector repository tags its builder alongside) are filtered out.

The platform list is what *that release actually publishes*, read from its assets rather than
from a table that could go stale — so a platform upstream added appears without this tool being
changed, and one it dropped simply is not offered. A platform this fleet has no name for
(`linux/s390x`, say) is left out, because a package entry no Agent can match is one nobody would
ever be offered.

### Non-interactive

Every prompt has a flag; give all of them and nothing is asked:

```console
$ opamp-fleetctl package fetch --agent otelcol-contrib --version 0.158.0 \
      --platform linux/amd64 --platform windows/amd64 \
      --out-dir ./artifacts --server https://127.0.0.1:4321
```

| Option | Meaning |
|---|---|
| `--agent <name>` | `otelcol`, `otelcol-contrib`, or `supervisor` (this fleet's own Client). |
| `--version <v>` | The version **as upstream numbers it** — `0.158.0`, `1.19` — never the tag (`v0.158.0`). Omitted, the last five are offered. |
| `--platform <os/arch>` | Repeatable. `linux/amd64`, `windows/amd64`, `darwin/arm64`, … A platform the release does not publish is refused with the list of those it does. |
| `--out-dir <path>` | Where artifacts are written. Created if missing. Defaults to the working directory. |
| `--server <url>` | The Operator plane's `https://` URL. Create the Package and upload each artifact as its platform's entry. It stops there: putting the Package in a deployment is an operator act, and the tool prints the call that does it. Cannot be combined with `--no-upload`. |
| `--no-upload` | Write the artifacts and stop, without the upload question. |

`--version` names the *release to fetch*, not this tool's own version — `--version 1.19`, not a
bare `--version`.

### What it prints at the end

The block each artifact needs, because that differs per agent and platform and is the next thing
you need:

```
Done. What a Supervisor needs to install these:
  linux/amd64  ./artifacts/otelcol-contrib_0.158.0_linux_amd64.tar.gz
      command = "otelcol-contrib"
  windows/amd64  ./artifacts/otelcol-contrib_0.158.0_windows_amd64.tar.gz
      command = "otelcol-contrib.exe"
```

For `--agent supervisor` there is no block to print — nothing supervises a Client — so the hint is
the consent that lets it take the package over itself, which is also its default
([ADR-0018](../adr/0018-the-client-updates-itself-under-its-own-agent-type.md)):

```
Done. What a Client needs to take these:
  linux/amd64  ./artifacts/supervisor_1.2.3_linux_amd64.tar.gz
      [packages] self_update = true  (the default)
```

With `--no-upload` (or a declined prompt) it also prints the two `curl` calls that would have
uploaded the artifacts, so the step can be done later or from somewhere else, and names the
default configuration that was not uploaded either.

### The default configuration

A package alone leaves an Agent with nothing to run: a Supervisor holds at *awaiting
configuration* until a Configuration of the name its block reads arrives. So an upload carries
the agent's default with it — the same bodies as
[`config/examples/`](../../config/examples/), compiled into the tool so they travel with the
released binary:

| `--agent` | Configuration | Aimed at |
|---|---|---|
| `otelcol` | `otelcol-conf` | Selector `service.name = otelcol` |
| `otelcol-contrib` | `otelcol-contrib-conf` | Selector `service.name = otelcol-contrib` |
| `supervisor` | none | — a Client is configured by `supervisor.toml` on its own host, and the fleet owns only its `[[supervisor]]` blocks ([ADR-0017](../adr/0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md)) |

Two rules keep this from surprising anyone:

- **An existing Configuration is never written over.** The name is asked for first, and one the
  Server already holds is left exactly as you left it, edits and all. That is what makes a second
  upload of a newer version safe.
- **Nothing is distributed.** Saving only saves
  ([ADR-0025](../adr/0025-rollout-and-what-reaches-an-agent.md)), so the default reaches
  no host until you roll it out. Read it first: these bodies carry example values.

```console
  stored as the linux/amd64 entry — not distributed: press the rollout when it should reach hosts
  storing the default configuration otelcol-contrib-conf …
  saved — read it over and press its rollout when it should reach hosts, since it carries example values
```

and on the next upload of the same agent:

```console
  otelcol-contrib-conf is already on the Server — left as it is
```

### When an upload is refused

The Server decides some uploads before it reads a byte of the artifact, and says which:

| What it answers | What to do |
|---|---|
| `409 … immutable` | The Package is already rolled out to an Agent, so its entries are fixed ([ADR-0025](../adr/0025-rollout-and-what-reaches-an-agent.md)). Fetch under a new version, or delete the Package first. |
| `507 … max_total_package_bytes` | The package store is at its ceiling. Delete a Package no channel holds, or raise the limit ([the Server's configuration reference](server.md#top-level)). |
| `404 … not configured` | The Server has no package store: `packages_dir` is unset in `server.toml` ([Packages](server.md#packages-and-deployments-distributing-software)). |
| `413 …` | The artifact is past `max_package_size_bytes`. |

Because the refusal arrives while the artifact is still being sent, the connection can reset before
the answer is read; the tool then asks the Server once more with an empty body to recover the reason,
so what you see is the status and message above rather than a bare connection error. A message that
does still begin `cannot reach` is what it says — the Server was not answering — and it carries the
underlying cause (DNS, refused connection, TLS) rather than only the request that failed.

### What it does not do

- **It does not sign.** Signing needs a key, and where that key lives is a decision a tool should
  not make silently — use [`package sign`](#packing-and-signing) and record the
  signature on the deployment that offers the bytes
  ([step 5](rollout.md#5-put-it-in-a-deployment-and-sign-it-there)).
- **It does not aim, and it does not roll out.** A Package reaches a host only once a Deployment
  holds it, and which channel that is is not a decision a fetch tool should make. What it stores sits
  in the store, visible as `in no deployment`, until you put it somewhere
  ([step 5](rollout.md#5-put-it-in-a-deployment-and-sign-it-there)); the tool prints the call.

### Prerequisites and limits

- **Network egress** to `api.github.com` and to the release asset host.
- **The upload speaks TLS to the Operator plane**, so the Server's URL is `https://`; type it so
  at the prompt, which suggests `http://`. The tool has no CA option and trusts the Server's
  certificate through this machine's own trust store, so a Server whose certificate comes from a
  private CA needs that CA trusted here. Off the loopback, carry the
  [`[rest.auth]`](server.md#the-operator-plane-restauth) credential in the URL:
  `https://fleet-admin:secret@fleet.example:4321`.
- **GitHub rate-limits unauthenticated requests to 60 per hour** per address. A run costs two
  requests (one to list versions, one to read a release), so this is only reached by scripting.
  The error says so rather than leaving you guessing.
- **Only amd64 and arm64 (and 386 where published)** are named, matching what Agents report as
  `os.type` and `host.arch`.

## Packing and signing

For a program `package fetch` does not know: these commands build the container, and they own
signing.

```console
$ opamp-fleetctl package keygen --out fleet-signing.pk8   # prints the public key (hex)
$ sha=$(opamp-fleetctl package pack --out promtail-3.0.0.tar.gz ./promtail)
$ sig=$(opamp-fleetctl package sign --key fleet-signing.pk8 --agent-type promtail --version 3.0.0 \
      promtail-3.0.0.tar.gz)
```

| `package` command | What it prints | Use it for |
|---|---|---|
| `keygen --out <file>` | the **public** key (hex) | the value for every Client's `[packages] verification_key`; the private key goes in the file, ideally not on the Server host |
| `pack <program> --out <file>` | the artifact's SHA-256 (hex) | building a one-file artifact; `--format tar.gz\|7z`, `--program-name <name>`, `--archive-key <key>` |
| `sign --key <file> --agent-type <type> --version <version> <artifact>` | the signature (hex) | the body of the Deployment's signature route, `PUT /api/v1/deployments/{name}/signatures/{agent_type}/{version}/{os}/{arch}`. It covers the type, the version and the artifact's SHA-256 together, so it holds for that Package alone (ADR-0026) |
| `public-key --key <file>` | the public key (hex) | recovering it from an existing private key |
| `sha256 <artifact>` | the SHA-256 (hex) | the `sha256` of a *referenced* entry, for an artifact the Server never holds |

The options of `pack`, and why a `.tar.gz` packed twice gives the same bytes, are in
[step 2 of the rollout walkthrough](rollout.md#2-build-the-artifact), which runs the whole path
from packing to a running agent.

**Signing is required, fleet-wide.** A Client takes a package only while `[packages]
verification_key` is set, and only when the artifact's signature verifies against it. The Server
offers no entry its Deployment has not signed, and refuses to roll out a Deployment with an
unsigned entry ([ADR-0026](../adr/0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md)). Give
every Client the same public key, and sign every entry of every Deployment.

## `pki`

The fleet's certificates, made without `openssl` or any other tool
([ADR-0027](../adr/0027-the-operator-tool-makes-the-fleets-certificates-and-the-server-says-before-they-end.md)).
[Setting up a fleet](setup.md) walks through them in order; in short:

| `pki` command | What it does |
|---|---|
| `init --server-dir <dir> --offline-dir <dir> --name <dns\|ip>…` | makes the three CAs, the Server's certificate and the bootstrap certificate, split into what the Server host needs and what stays offline, with a configuration fragment for each side |
| `server-cert --offline-dir <dir> --name <dns\|ip>… --out <dir>` | makes another server certificate from the offline server CA, for the Server or a Gateway |
| `bootstrap-cert --offline-dir <dir> --out <dir>` | makes another bootstrap certificate from the offline bootstrap CA and prints its serial |
| `status --offline-dir <dir>` | says when the certificates in the offline directory end; exits `0`, `1` when one is ending, `2` when one has ended |

The Server makes none of these. Its own `server pki status --config <server.toml>` says when the
certificates of the Server host end, by the same rule.
