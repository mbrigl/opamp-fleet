# Recipe: rolling out and managing Icinga 2

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[Rollout walkthrough](rollout.md)

[Icinga 2](https://icinga.com/docs/icinga-2/latest/doc/01-about/) is a monitoring system whose
agents run on every monitored host, connected to a master or satellite. This recipe puts an Icinga 2
**Agent** under fleet management end to end: the fleet ships the program, creates its directories,
obtains its certificate from the Icinga master, distributes its configuration, and updates and rolls
it back — with nothing installed on the host through `apt`, `dnf`, or an MSI.

That is more than the [GLPI recipe](glpi-agent.md) does, and it needs a Supervisor kind of its own
([ADR-0068](../adr/0068-icinga-2-is-supervised-by-a-kind-of-its-own.md)), because Icinga 2 is not
built to be relocated: it must be *told*, on every invocation, where its state, its template library
and its account are — and it creates none of those directories itself.

- [What the kind does for you](#what-the-kind-does-for-you)
- [1. Build the artifact](#1-build-the-artifact)
- [2. The block](#2-the-block)
- [3. Enrolment: the ticket](#3-enrolment-the-ticket)
- [4. Send its configuration](#4-send-its-configuration)
- [5. Roll it out](#5-roll-it-out)
- [What to expect in the fleet view](#what-to-expect-in-the-fleet-view)
- [Updates and rollback](#updates-and-rollback)
- [Limits worth knowing before you start](#limits-worth-knowing-before-you-start)
- [Troubleshooting](#troubleshooting)

## What the kind does for you

`type = "icinga2"` derives ten command-line constants from a handful of values in the block. Three
of them are not choices at all — they follow from where the Client put things and which account it
runs as — and getting any of them wrong produces a daemon that starts and quietly uses the wrong
files:

| What | Why the Supervisor sets it |
|---|---|
| `-D RunAsUser` / `-D RunAsGroup` | **Every** `icinga2` invocation drops privileges to a compiled-in `nagios` account first, and refuses when it cannot. A fleet-managed host has no such account; the Client's own service account is what the daemon may run under. |
| `-D IncludeConfDir` | `include <itl>` resolves against this. `-I` does **not** override it — on a host that also has Icinga installed, the template library would silently come from the machine's copy instead of the delivered tree. |
| `-D DataDir` and its siblings | Where state, logs, cache, spool and the pid file go — beside the tree, never inside it, so a package update does not take the certificates with it. |
| `-D NodeName` | The node's own name: the certificate is issued for it, and the `ApiListener` looks for `<NodeName>.crt` under `DataDir/certs`. |

It also creates those directories before every start (Icinga does not), starts the daemon in its own
process group so a stop takes its worker with it, validates every configuration before applying it,
and reloads with `SIGHUP` where the platform has signals.

## 1. Build the artifact

Icinga publishes distribution packages and an MSI, no portable tree — so the artifact is repacked
from the vendor's own packages
([ADR-0070](../adr/0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)).

Build it **on** the distribution you are building for — the tree carries the libraries the build
host resolves, so the build host is the decision, not a flag (see the two rules below). This
project's Dev Container is that host: it is pinned to Debian 12 and carries Icinga's runtime
libraries for exactly this reason
([ADR-0074](../adr/0074-the-dev-container-is-pinned-to-the-distribution-it-builds-for.md)), so the
Debian 12 artifact is built in it directly:

```console
$ cargo run --bin opamp-package-fetch -- --agent icinga2 --version 2.16.5 --distro bookworm \
      --platform linux/amd64 --server http://127.0.0.1:4321
  reading https://packages.icinga.com/debian/dists/icinga-bookworm/main/binary-amd64/Packages.gz …
  reading https://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages.gz …
  this build needs glibc >= 2.34 on every host it is rolled out to

linux/amd64
  downloading …/icinga2-bin_2.16.5-1+debian12_amd64.deb …
  verified against upstream's SHA-256
  …
  bundled 28 shared libraries
  repacked  sha256 d60ee0e6…
```

Two things about that command line, each of which costs an attempt to discover:

- **`--distro bookworm` is the guard, not a choice.** Omitted, the tool builds for whatever the host
  is and says so; named, it refuses when the host is not that distribution. In a recipe meant to be
  copied, the refusal is the point — the wrong build host then fails loudly instead of quietly
  producing an artifact for a different reach.
- **`--server` uploads as it goes.** Leave it out with `--no-upload` and upload the artifacts
  afterwards; the tool prints the two `curl` calls that do it.

Add `--platform windows/amd64` to build the Windows artifact in the same run. It is repacked from
the MSI and verified by Icinga's own Authenticode signature
([ADR-0072](../adr/0072-the-windows-artifact-is-verified-by-its-publisher.md)) rather than by a
digest, so it needs no particular build host and no glibc floor applies to it.

To build for a **different** reach — an older distribution than the Dev Container, for hosts it does
not cover — run the same tool in a container of that distribution, which is then the build host:

```console
$ docker run --rm -v "$PWD:/src" -w /src --network host rust:bullseye bash -lc \
    'cargo run --bin opamp-package-fetch -- --agent icinga2 --version 2.16.5 --distro bullseye \
       --platform linux/amd64 --no-upload'
```

`cargo run` rather than the binary from this checkout: the tool is glibc-bound like everything it
builds, so one compiled in the Dev Container will not start under an older distribution at all.
Build it where it runs. `--network host` is only needed for `--server`, and only on Linux. That
container also needs Icinga's runtime libraries installed once — see the refusal below.

The tree carries the daemon, the template library, **the check plugins**
(`monitoring-plugins`, 47 of them, with the libraries they need), and the vendor copyright files.
For Icinga 2 2.16.5 on Debian 12 that is 140 files and 75 MB unpacked — well inside the limits a
package tree is held to ([ADR-0023](../adr/0023-multi-file-packages.md)).

The plugins come from the distribution rather than from Icinga, and one of them needs a word: Debian
ships `check_http` through `update-alternatives`, so it exists only after a package is *installed* —
the repack applies that same rule to the payload, highest priority winning, which is why
`check_http` is in the tree and is the implementation Debian would have chosen.

Two rules follow from what the tree carries:

- **Build it on the distribution you are building for.** The tree carries the libraries the build
  host resolves; everything except glibc travels with it. `--distro` states which build you mean and
  is checked against the host; omit it and the host's own is used. Either way the tool refuses to
  build for a distribution this host is not — which is why the second recipe above is a container
  and not a flag.
- **The glibc line it prints is the artifact's reach.** A tree built on Debian 13 does not run on
  Debian 12 or RHEL 9; one built on Debian 11 runs on all of them, across families, because glibc is
  backward compatible (ADR-0071). Build on the oldest distribution you must serve — that is the one
  decision this step really carries, and for this project it has been made once, as the Dev
  Container's image pin (ADR-0074). Bumping that pin narrows every artifact built afterwards.

If the build host is missing a library any of the packages depend on, the tool stops rather than
packing an incomplete tree — and prints the `apt-get install` line that fixes it, naming the
packages that provide the libraries it just listed. **The Dev Container already carries them**; a
container you started for a different reach needs the line once, inside that same container and
without `sudo`, since a container's shell is already root:

```console
error: the build host is missing libraries the package needs: libboost_coroutine.so.1.74.0, …
  install the vendor package's own dependencies first:
      sudo apt-get install -y --no-install-recommends libboost-coroutine1.74.0 …

$ apt-get update && apt-get install -y --no-install-recommends libboost-coroutine1.74.0 …
```

The names carry the distribution's own versions, so they differ per container — which is why the
tool reads them out of that distribution's package index rather than this page listing them.

## 2. The block

```toml
[[supervisor]]
type = "icinga2"
name = "icinga2"
service_name = "icinga2"          # what Selectors aim at (ADR-0033)
binary = "icinga2"                # bare name ⇒ the Client owns and updates the tree (ADR-0021)
program_path = "sbin/icinga2"     # where the program sits inside the delivered tree
stop_timeout_secs = 60
apply_grace_secs = 30

main_config = "icinga2-conf"      # the *name of a Configuration*, not a path
include_dir = "${supervisor_dir}/program/tree/share/icinga2/include"
plugin_dir  = "${supervisor_dir}/program/tree/plugins"

parent_host = "master.example.com"
parent_port = 5665
node_name   = "edge-01.example.com"          # default: the Supervisor's name
ticket_file = "${config_dir}/icinga2-ticket" # delivered per host, see below

[supervisor.env]
LD_LIBRARY_PATH = "${supervisor_dir}/program/tree/lib"
```

`LD_LIBRARY_PATH` is what makes the bundled libraries win over the ones compiled into the binary's
search path. Leave `parent_host` out for a standalone node that only runs local checks: then there
is no enrolment, and the daemon starts as soon as its configuration arrives.

On **Windows** the same block differs in three values: `binary = "icinga2.exe"`,
`program_path = "sbin/icinga2.exe"`, and `plugin_dir` pointing at `sbin` — the check executables
stay beside the daemon there, because a Windows program finds its DLLs in its own directory first.
No `LD_LIBRARY_PATH` is needed, and `reload_signal` does not exist, so a configuration change is a
restart.

A machine-installed Icinga 2 is supervised by the same kind — name `binary` with an absolute path
(`/usr/lib/x86_64-linux-gnu/icinga2/sbin/icinga2`), drop `program_path`, and disable the native
service. The Agent then declares no `AcceptsPackages` and the fleet ships it nothing.

## 3. Enrolment: the ticket

The Icinga master stays the certificate authority — the fleet Server signs nothing and never sees a
private key ([ADR-0069](../adr/0069-the-icinga-master-signs-the-ticket-travels-as-a-configuration.md)).
What the fleet transports is the **ticket**, which the master computes for one node name:

```console
$ icinga2 pki ticket --cn edge-01.example.com          # on the Icinga master
d9c8…

$ curl -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "icinga2", "role": "supplementary",
            "selector": {"service.instance.name": "edge-01"},
            "body": "d9c8…"}' \
       http://127.0.0.1:4321/api/v1/configurations/icinga2-ticket
```

`role = "supplementary"` writes it as a file the Supervisor reads and nothing else consumes, and the
Selector aims it at exactly one Agent. The Supervisor then generates a key locally, pins the
parent's certificate, and requests a signature; the key never leaves the host.

Two variations:

- **Pin the parent explicitly.** Deliver the parent's certificate as a second `supplementary`
  Configuration and name it in `trusted_cert_file`. It is the parent's **own** certificate —
  `DataDir/certs/<master-cn>.crt` on the master — **not** the CA that signed it: Icinga compares
  what the parent presents against this file, so a CA certificate here fails every enrolment with
  *"Peer certificate does not match trusted certificate"*. Without a pinned certificate the
  Supervisor trusts what the parent presents on first contact, and logs that it did.
- **No ticket at all.** The request lands in the master's signing queue; the Agent stays unhealthy
  until someone runs `icinga2 ca sign <hash>` there. That is correct behaviour, not a fault.

## 4. Send its configuration

Icinga reads **one root file** and pulls the rest in with `include`. A relative `include` resolves
against the including file, so the delivered entries can reference each other by name without
knowing any absolute path:

```console
$ curl -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d @icinga2-conf.json http://127.0.0.1:4321/api/v1/configurations/icinga2-conf
```

with a body along the lines of [`config/examples/icinga2-conf.conf`](../../config/examples/icinga2-conf.conf):

```
include <itl>
include <plugins>
include "icinga2-zones"

object ApiListener "api" { accept_config = true; accept_commands = true }
object CheckerComponent "checker" { }
object FileLogger "mainlog" { severity = "information"; path = LogDir + "/icinga2.log" }
```

The `zones.d` directory an Icinga master uses is not needed here: an Agent receives its checks over
the cluster protocol, into `DataDir/api/zones`.

## 5. Roll it out

```console
$ curl -u fleet-admin:secret -X POST \
       http://127.0.0.1:4321/api/v1/packages/icinga2/icinga2/2.16.4/rollout
$ curl -u fleet-admin:secret -X POST \
       http://127.0.0.1:4321/api/v1/configurations/icinga2-conf/rollout
```

## What to expect in the fleet view

| Field | What it says |
|---|---|
| `service_name` | `icinga2` — what a Selector aims at |
| `service_version` | the version out of Icinga's own banner (`r2.16.4-1` → `2.16.4`) |
| `health_status` | `running`, or **`awaiting the certificate for <node>`** while enrolment has not succeeded |
| `remote_config_status` | `APPLIED` — or `FAILED` with Icinga's own validation error, in which case the daemon kept running the previous configuration |
| `packages` | `Installed`, or `InstallFailed` with the reason the artifact would not run on this host |

## Updates and rollback

A new version is a new Set, rolled out the same way; the Supervisor unpacks it beside the running
tree, **proves it starts** before stopping anything, then swaps, health-gates, and rolls back if the
new one does not stay up. A package that cannot run on the host — a tree built against a newer glibc
— is refused before the running daemon is touched, and the fleet view carries the linker's own
message. The certificates and the enrolment survive every update, because they live beside the tree
rather than inside it.

## Limits worth knowing before you start

- **The daemon runs as the Client's service account**, not as `nagios`. Checks that need elevated
  capabilities — `check_icmp` and its relatives — will not work. Local checks do.
- **The build host decides the reach, and only glibc bounds it.** glibc cannot travel and is
  backward compatible, so **one** artifact serves every host whose glibc is at least the build
  host's — across distribution families (ADR-0071). Build on the oldest system you serve: a Debian 11
  build (`libc6 >= 2.30`) covers Debian, Ubuntu, and RHEL 9 and 10 alike.
- **A tree built on Debian carries Debian's OpenSSL layout.** Icinga's cluster TLS is unaffected —
  its certificates are named by explicit paths — but a check that reaches for the *system* trust
  store on a Red Hat host will look where Debian keeps it. Verify that before relying on such a
  check, or build the artifact on the family you run.
- **Icinga's own Red Hat packages are behind a subscription** from RHEL 9 on; the open repository
  ends at EL 8. That is a reason to serve those hosts from the Debian-built artifact rather than a
  reason to buy one.
- **Windows builds, but is unproven.** `opamp-package-fetch` produces a Windows artifact from the
  MSI's payload — verified by Icinga's own signature, since no digest is published — but whether a
  repacked tree finds its paths without the MSI's product registration has not been tried on a
  Windows host. Until it has, prefer the machine-installed form of the block there. The RPM repack
  is not built and is optional under ADR-0071.
- **Removing the Supervisor removes its certificate with the directory.** The Icinga master still
  holds the signed certificate for that node — `icinga2 ca remove` there is the operator's, and the
  Supervisor says so when it is retired.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `awaiting the certificate for <node>` | Enrolment has not succeeded. The health's error names why: an unreachable parent, an invalid ticket, or a signature nobody granted yet. It retries with backoff; the daemon deliberately does not start meanwhile. |
| `awaiting the configuration …` | No root Configuration has arrived yet, or `main_config` names one that does not match this Agent. |
| `remote_config_status = FAILED` with a syntax error | Icinga refused the configuration. The running daemon kept the previous one — fix the Configuration and roll out again. |
| `InstallFailed`, *"does not run on this host"* | The artifact was built against a newer glibc, or on a host missing a library. Rebuild on the oldest distribution you serve. |
| A check the parent assigns fails with `CheckCommand does not exist` | The root configuration includes `<itl>` but not `<plugins>`: the templates are there, the commands are not. |
| The daemon starts but the master never sees it | Check `NodeName` against the certificate's common name: the `ApiListener` looks for `DataDir/certs/<NodeName>.crt`. |
| `Peer certificate does not match trusted certificate` | `trusted_cert_file` holds the parent's CA instead of the parent's own certificate. Deliver `DataDir/certs/<master-cn>.crt` from the master. |
| The certificate is renewed on every start | The expiry could not be read from `pki verify`, so the Supervisor renews rather than guesses. Check that the parent signs certificates with a validity Icinga prints. |
| The ticket file disappeared | Every Configuration apply rewrites the entry directory. That is harmless after enrolment — the certificate is what matters, and it lives elsewhere. |
