# Recipe: rolling out and managing Icinga 2

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[Rollout walkthrough](rollout.md) · [Artifact: Icinga 2](../artifacts/icinga2.md)

[Icinga 2](https://icinga.com/docs/icinga-2/latest/doc/01-about/) is a monitoring system whose
agents run on every monitored host, connected to a master or satellite. This recipe puts an Icinga 2
**Agent** under fleet management end to end: the fleet ships the program, creates its directories,
obtains its certificate from the Icinga master, distributes its configuration, and updates and rolls
it back — with nothing installed on the host through `apt`, `dnf`, or an MSI.

That is more than a program and a configuration, and it needs a Supervisor kind of its own, because
Icinga 2 is not built to be relocated: it must be *told*, on every invocation, where its state, its
template library and its account are — and it creates none of those directories itself.

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

`type = "icinga2"` builds the daemon's whole command line — around ten `-D` constants and the
directories behind them — out of the artifact it delivers, the platform, and the account it runs
as. None of it is
written on a host any more, and getting any of it wrong used to produce a daemon that starts and
quietly uses the wrong files:

| What | Why the Supervisor sets it |
|---|---|
| `-D RunAsUser` / `-D RunAsGroup` | **Every** `icinga2` invocation drops privileges to a compiled-in `nagios` account first, and refuses when it cannot. A fleet-managed host has no such account; the Client's own service account is what the daemon may run under. |
| `-D IncludeConfDir` | `include <itl>` resolves against this. `-I` does **not** override it — on a host that also has Icinga installed, the template library would silently come from the machine's copy instead of the delivered tree. |
| `-D DataDir` and its siblings | Where state, logs, cache, spool and the pid file go — beside the tree, never inside it, so a package update does not take the certificates with it. |
| `-D NodeName` | The node's own name: the certificate is issued for it, and the `ApiListener` looks for `<NodeName>.crt` under `DataDir/certs`. |

It also creates those directories before every start (Icinga does not), starts the daemon in its own
process group so a stop takes its worker with it, validates every configuration before applying it,
reloads with `SIGHUP` where the platform has signals, and carries Icinga's own timing — 60 seconds
for a graceful stop, 30 for the apply grace — because a daemon that drains its checks and closes its
cluster connections is slower than the fleet's default allows.

**The complete derivation, per platform, is in
[`docs/artifacts/icinga2.md`](../artifacts/icinga2.md)** — the document the packing tool and this
kind are both held to by tests, and the one to read before a version bump. This page does not repeat
it: what follows is only what an operator decides.

## 1. Build the artifact

Icinga publishes distribution packages and an MSI, no portable tree — so the artifact is repacked
from the vendor's own packages, with the check plugins and every shared library they need taken
from Debian's packages of the same release.

Nothing in the tree comes from the host you run the tool on: every file is a `.deb` from Icinga's
or Debian's signed archive, checked against the archive's own index. So any host builds the
artifact for any of `bullseye`, `bookworm` and `trixie`; `--distro` says which, and omitted it is
the host's own. The Dev Container is Debian 12, so there it is bookworm:

```console
$ cargo run --bin opamp-fleetctl -- package fetch --agent icinga2 --version 2.16.5 --distro bookworm \
      --platform linux/amd64 --server https://127.0.0.1:4321
  reading https://packages.icinga.com/debian/dists/icinga-bookworm/InRelease …
  verified the repository's signature
  reading https://packages.icinga.com/debian/dists/icinga-bookworm/main/binary-amd64/Packages.gz …
  reading https://deb.debian.org/debian/dists/bookworm/InRelease …
  verified the repository's signature
  reading https://deb.debian.org/debian/dists/bookworm/main/binary-amd64/Packages.xz …
  …
  reading https://deb.debian.org/debian-security/dists/bookworm-security/main/binary-amd64/Packages.xz …
  26 library packages from Debian bookworm: gcc-12-base libboost-context1.74.0 … zlib1g
  this build needs glibc >= 2.36 on every host it is rolled out to

linux/amd64
  downloading …/icinga2-bin_2.16.5-1+debian12_amd64.deb …
  verified against upstream's SHA-256
  …
  bundled 28 shared libraries
  repacked  sha256 93b66c2c…
```

Two things about that command line:

- **`--distro bookworm` makes the recipe the same on every host.** Omitted, the tool takes the
  host's own suite and says so; on a host that is none of the three it builds no Linux artifact
  and says which flag to give.
- **`--server` uploads as it goes.** Leave it out with `--no-upload` and upload the artifacts
  afterwards; the tool prints the two `curl` calls that do it.

The tool needs `dpkg-deb`, `xz` and `gpgv` on the host, which every Debian system has. It reads
Debian's packages from the release, its `-updates` and its `-security` suites, the newest across
them, as `apt` would install them — so the tree carries the OpenSSL and the other libraries of the
day you run it. **A security update to one of them reaches your hosts only with a new repack.**

The tree carries the daemon, the template library, **the check plugins**
(`monitoring-plugins`, 47 of them), the libraries they all link against, and the copyright file of
every package the tool downloaded. For Icinga 2 2.16.5 from Debian 12 that is 158 files and 89 MB
unpacked — well inside the limits a package tree is held to.

The plugins come from the distribution rather than from Icinga, and one of them needs a word: Debian
ships `check_http` through `update-alternatives`, so it exists only after a package is *installed* —
the repack applies that same rule to the payload, highest priority winning, which is why
`check_http` is in the tree and is the implementation Debian would have chosen.

**The glibc line it prints is the artifact's reach.** glibc cannot travel, and it is backward
compatible, so the tree runs wherever the glibc is at least the highest floor any of its packages
states — across distribution families. Repack the oldest release you must serve:

| `--distro` | glibc at least | Runs on |
|---|---|---|
| `bullseye` | 2.30 | Debian 11+, Ubuntu 20.04+, RHEL 9+ |
| `bookworm` | 2.36 | Debian 12+, Ubuntu 24.04+, RHEL 10+ |
| `trixie` | 2.39 | Debian 13+, Ubuntu 24.04+, RHEL 10+ |

The bookworm tree needs 2.36 although Icinga's own binary states 2.34: the C++ runtime it carries
from Debian 12 needs the newer one, so Ubuntu 22.04 and RHEL 9 need the bullseye tree. Debian 11
has ended, and `deb.debian.org` no longer serves some of the packages its index still names; for
each of them the tool says so and reads the same file from `archive.debian.org`, where Debian keeps
ended releases, verified against the same signed index.

If a program links a library none of the downloaded packages holds, the tool stops rather than
packing a tree that would fail on its first start, and names the library and the program:

```console
error: the packages the tree is made of hold no library the programs link against: libfoo.so.1 (needed by check_bar)
```

That is a change in Debian's packaging the tool does not follow yet, not something to install on
your host.

## 2. The block

Four keys, and each describes the Icinga installation this host is **joining** — nothing this
Client can compute. The block is the same on both platforms:

```toml
[[supervisor]]
type = "icinga2"
name = "icinga2"

parent_host = "master.example.com"                       # or "master.example.com:5665"
node_name = "edge-01.example.com"                        # default: this host's FQDN
ticket_file = "${config_dir}/icinga2-ticket"             # delivered per host, see below
trusted_cert_file = "${config_dir}/icinga2-parent-cert"  # optional, see below
```

- **`parent_host`** is the master or satellite this Agent enrols with and connects to. The port
  rides in the value as `host:port` and defaults to Icinga's 5665. Leave the key out for a
  standalone node that only runs local checks: then there is no enrolment, and the daemon starts as
  soon as its configuration arrives.
- **`node_name`** is the one value that must match what was typed on the *other* side: it is the
  `NodeName`, the certificate's common name, and the Endpoint name at once, and Icinga requires the
  three to be the same string. It defaults to this host's fully qualified name — resolved the way
  `hostname --fqdn` resolves it, which is also where `icinga2 pki ticket --cn …` takes its argument
  — so a fleet whose master knows its hosts by FQDN need not write it at all. Only a name with a
  dot in it is accepted as that default; where the resolver has none to give, the Supervisor's own
  name stands and this key is how you say what the master actually knows.
- **`ticket_file`** and **`trusted_cert_file`** name delivered Configuration entries, not files you
  put on the host — see [step 3](#3-enrolment-the-ticket).

**Everything else the block used to carry is gone**, and a block still carrying any of it fails at
startup with a message naming what supplies the value now: `binary`, `program_path`, `service_name`,
`include_dir`, `plugin_dir`, the five state directories, `log_level`, `run_as_user`/`run_as_group`,
`parent_port`, `renew_before_days`, `args`, `env`, `stop_timeout_secs`, `apply_grace_secs` — and
`main_config`, which became a mark on the Configuration itself
([step 4](#4-send-its-configuration)). There is no block shape that both the old and the new Client
accept, so the cutover is per host and deliberate: the old Client requires `main_config`, this one
refuses it.

**One block now serves the whole fleet.** With `node_name` defaulting to the FQDN nothing in it is
per-host any more, so it can travel as a single Configuration typed `supervisor`
([The Server can manage the set](client.md#the-server-can-manage-the-set)) instead of being written
into every host's `supervisor.toml`. What stays per host is the ticket, which was always its own
Configuration.

An Icinga 2 the machine already installed is **not** supervised in place: the kind installs and
names its own program, so no block can point at `/usr/lib/x86_64-linux-gnu/icinga2/sbin/icinga2`.
The route across is the one this page describes — repack that version with
`opamp-fleetctl package fetch` and let the fleet deliver it — and the native service is disabled
once the fleet-delivered tree runs. The host keeps whatever the package manager installed; nothing
supervises it.

## 3. Enrolment: the ticket

The Icinga master stays the certificate authority for Icinga — the fleet Server signs no Icinga
certificate and never sees an Icinga private key.
What the fleet transports is the **ticket**, which the master computes for one node name. The
calls below go to the Operator plane over TLS; `--cacert ca.pem` names the CA that signed the
Server's certificate, and `-u` carries the [`[rest.auth]`](server.md#the-operator-plane-restauth)
credential:

```console
$ icinga2 pki ticket --cn edge-01.example.com          # on the Icinga master
d9c8…

$ curl --cacert ca.pem -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "icinga2", "role": "supplementary", "body": "d9c8…"}' \
       https://127.0.0.1:4321/api/v1/configurations/icinga2-ticket-edge-01
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"service.name": "icinga2", "host.name": "edge-01"}}' \
       https://127.0.0.1:4321/api/v1/deployments/icinga2-edge-01
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT \
       https://127.0.0.1:4321/api/v1/deployments/icinga2-edge-01/configurations/icinga2-ticket-edge-01
```

`role = "supplementary"` writes it as a file the Supervisor reads and nothing else consumes. A
Configuration has no Selector of its own: it reaches an Agent only through the Deployment that
claims it, so the ticket goes into a Deployment whose Selector matches exactly this one Agent. The
file name is the Configuration's name, so the block's `ticket_file` names
`${config_dir}/icinga2-ticket-edge-01` — or keep the name `icinga2-ticket` in every host's
Deployment and one block serves them all. The Supervisor then generates a key locally, pins the
parent's certificate, and requests a signature; the key never leaves the host.

**An Agent belongs to at most one Deployment**, and a rollout replaces everything the Agent was
given. So that per-host Deployment holds everything the Agent is to run while it enrols — the
Package, `icinga2-conf`, `icinga2-zones` and the parent's certificate besides the ticket (steps 4
and 5) — and no other Deployment may claim the Agent meanwhile, or neither reaches it. Once the
Agent is enrolled it can move to the fleet's channel: the next rollout there leaves the ticket
behind, which is harmless, because the certificate is what counts from then on.

Two variations:

- **Pin the parent explicitly.** Deliver the parent's certificate as a second `supplementary`
  Configuration and name it in `trusted_cert_file`. It is the parent's **own** certificate —
  `DataDir/certs/<master-cn>.crt` on the master — **not** the CA that signed it: Icinga compares
  what the parent presents against this file, so a CA certificate here fails every enrolment with
  *"Peer certificate does not match trusted certificate"*. Once `trusted_cert_file` names a file,
  enrolment waits until that file is there — the Agent stays unhealthy and says which path it
  waits for — and never falls back to trusting the parent on sight. Only a block that names no
  `trusted_cert_file` at all trusts what the parent presents on first contact, and logs that it did.
  Unlike the ticket, the parent certificate stays needed after enrolment: a renewal pins the parent
  again, so keep its Configuration released for as long as the block names it. Without
  `trusted_cert_file`, the parent is trusted on first use **once**: the certificate saved then is
  kept for that parent, and renewals and retries do not trust what it presents later.
- **A named `ticket_file` is waited for too.** Until the file is there and holds a ticket, nothing
  is sent: a request without it would land in the master's signing queue unasked for.
- **No ticket at all.** The request lands in the master's signing queue; the Agent stays unhealthy
  until someone runs `icinga2 ca sign <hash>` there. That is correct behaviour, not a fault. It asks
  again with the same key, so the certificate signed on the master is the one it fetches.

## 4. Send its configuration

Icinga reads **one root file** and pulls the rest in with `include`. A relative `include` resolves
against the including file, so the delivered entries can reference each other by name without
knowing any absolute path:

```console
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d @icinga2-conf.json https://127.0.0.1:4321/api/v1/configurations/icinga2-conf
```

where `icinga2-conf.json` is `{"service_name": "icinga2", "body": "…"}` — the type keeps the body
away from every other kind of Agent a Deployment carries — with a body along the lines of
[`config/examples/icinga2-conf.conf`](../../config/examples/icinga2-conf.conf):

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

Sign every entry of the Package, then put the Package and the Configurations into the Deployment
that should carry them, and roll that out
([steps 4 to 7 of the walkthrough](rollout.md#4-give-it-to-the-server)):

```console
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d "{\"signature\": \"$sig\"}" \
       https://127.0.0.1:4321/api/v1/packages/icinga2/2.16.5/entries/linux/amd64/signature
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"channel": "stable"}}' https://127.0.0.1:4321/api/v1/deployments/stable
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT \
       https://127.0.0.1:4321/api/v1/deployments/stable/packages/icinga2/2.16.5
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT \
       https://127.0.0.1:4321/api/v1/deployments/stable/configurations/icinga2-conf
$ curl --cacert ca.pem -u fleet-admin:secret -X PUT \
       https://127.0.0.1:4321/api/v1/deployments/stable/configurations/icinga2-zones
$ curl --cacert ca.pem -u fleet-admin:secret -X POST \
       https://127.0.0.1:4321/api/v1/deployments/stable/rollout
```

`$sig` is the artifact's signature from
`opamp-fleetctl package sign --agent-type icinga2 --version 2.16.5 …`, which holds for that type and
version alone. A Client takes the package only with `[packages] verification_key` set. One press
releases the Package and its Configurations together. For a host that enrols with a ticket, the same
calls go to its own Deployment of step 3 instead.

The Package version is SemVer, and Icinga's release numbers already are: `--version 2.16.5`
fetches the vendor's `2.16.5-1+debian12` packages and uploads
Package `2.16.5` — the same number the Agent reports out of its banner, so a rollout's version test
compares like with like.

## What to expect in the fleet view

| Field | What it says |
|---|---|
| `service_name` | `icinga2` — what a Selector aims at |
| `service_version` | the version out of Icinga's own banner (`r2.16.4-1` → `2.16.4`) |
| `health_status` | `running`, or **`awaiting the certificate for <node>`** while enrolment has not succeeded |
| `remote_config_status` | `APPLIED` — or `FAILED` with Icinga's own validation error, in which case the daemon kept running the previous configuration |
| `packages` | `Installed`, or `InstallFailed` with the reason the artifact would not run on this host |

## Updates and rollback

A new version is a new Package, rolled out the same way; the Supervisor unpacks it beside the
running tree, **proves it starts** before stopping anything, then swaps, health-gates, and rolls
back if the new one does not stay up. A package that cannot run on the host — a tree built against a
newer glibc — is refused before the running daemon is touched, and the fleet view carries the
linker's own message. The certificates and the enrolment survive every update, because they live
beside the tree rather than inside it.

**A second repack of the same Icinga version** — to bring in a Debian security update to one of
the libraries the tree carries, say — is a Package of its own, numbered with `--repack`:

```console
$ cargo run --bin opamp-fleetctl -- package fetch --agent icinga2 --version 2.14.3 --distro bookworm \
      --platform linux/amd64 --repack 2 --server https://127.0.0.1:4321
```

It is uploaded as `2.14.4-0.r.2`: above `2.14.3` and every earlier repack, below Icinga's next
release `2.14.4`. The daemon keeps printing `2.14.3`, but its Supervisor reports the version of the
Package it installed, so the hosts on an earlier repack are offered it, a host on it is not offered
it again, and `2.14.4` replaces it when it comes. Count on from the last repack you uploaded; the
tool does not look it up.

## Limits worth knowing before you start

- **The daemon runs as the Client's service account**, not as `nagios` — and on a host installed
  from the `.deb` or `.rpm` that is the account `opamp-fleet` the package creates, not root
  ([Running it under its own account](client.md#running-it-under-its-own-account)). Checks that need
  elevated capabilities — `check_icmp`, `check_dhcp` and their relatives — will not work from the
  delivered tree under that account: the Client unpacks the tree as that account, so the tree's
  copies carry no setuid-root bit, and a file capability set by hand is gone with the next package
  swap. The unit the Client writes for a non-root account sets no `NoNewPrivileges=` and nothing
  that implies it ([the systemd unit](client.md#the-systemd-unit-and-what-a-drop-in-adds)), so a
  setuid helper the *host* installed — `/usr/lib/nagios/plugins/check_icmp` from the distribution's
  `monitoring-plugins-basic`, or a `sudo` rule — still works, and a `CheckCommand` may name it by
  its absolute path. Where the delivered plugins themselves must run privileged, running the Client
  as root is the operator's choice: `service install --run-as root`, which every later package
  upgrade keeps. Local checks work either way.
- **Nothing is written under `/etc`.** The kind keeps Icinga's whole state beside the tree in the
  Supervisor's own directory and points every `-D` constant there, so the vendor's
  `/etc/icinga2` and `/var/lib/icinga2` are neither read nor written, and the service account
  needs no access to them. The unit leaves `ProtectSystem=` out anyway, so a check or a
  configuration that writes elsewhere on the host is bounded by the account's own file rights, not
  by the sandbox.
- **The Debian release repacked decides the reach, and only glibc bounds it.** glibc cannot travel
  and is backward compatible, so **one** artifact serves every host whose glibc is at least the
  floor the tool printed — across distribution families. Repack the oldest release you serve: a
  Debian 11 tree (`libc6 >= 2.30`) covers Debian, Ubuntu, and RHEL 9 and 10 alike, while Debian
  12's needs 2.36 and leaves out Ubuntu 22.04 and RHEL 9.
- **A tree built on Debian carries Debian's OpenSSL layout.** Icinga's cluster TLS is unaffected —
  its certificates are named by explicit paths — but a check that reaches for the *system* trust
  store on a Red Hat host will look where Debian keeps it. Verify that before relying on such a
  check.
- **Icinga's own Red Hat packages are behind a subscription** from RHEL 9 on; the open repository
  ends at EL 8. That is a reason to serve those hosts from the Debian-built artifact rather than a
  reason to buy one.
- **Linux only.** Whether a tree repacked from the MSI finds its paths without the MSI's product
  registration has not been tried on a Windows host, and there is no second form of the block to
  fall back on. **Windows is therefore an open task, not a supported platform**:
  `opamp-fleetctl package fetch` builds no Windows artifact for Icinga 2, and the kind refuses to
  start on Windows, naming why. What it needs is one run on a test host — unpack the MSI, roll it
  out, and see whether the daemon finds `include_dir`, `plugin_dir` and its libraries from
  `${supervisor_dir}/program/tree` without the registry keys the MSI writes. Until someone has done
  that and this page says so, keep Windows hosts on their MSI installation and outside the fleet.
  The RPM repack is not built, and nothing requires it.
- **Removing the Supervisor removes its certificate with the directory.** The Icinga master still
  holds the signed certificate for that node — `icinga2 ca remove` there is the operator's, and the
  Supervisor says so when it is retired.

## Troubleshooting

| Symptom | Cause |
|---|---|
| `awaiting the certificate for <node>` | Enrolment has not succeeded. The health's error names why: an unreachable parent, an invalid ticket, a signature nobody granted yet, or a parent certificate named in `trusted_cert_file` that has not arrived (the error names the path). It retries with backoff; the daemon deliberately does not start meanwhile. |
| `awaiting configuration` | The daemon is not started, and the health's error says why: no root Configuration has arrived yet (nothing delivered carries `role = "main"`, and no entry is named `icinga2-conf` either), two entries claim to be the root, the configuration on disk is the one Icinga refused, or the certificate is still missing. |
| `two configurations claim to be the root` | Two delivered entries carry `role = "main"`. The message names both; take the role off the one that is not Icinga's root file. |
| `remote_config_status = FAILED` with a syntax error | Icinga refused the configuration. The running daemon kept the previous one — fix the Configuration and roll out again. Until then a restart does not start the refused one: the daemon waits, and the health names the refusal. |
| `InstallFailed`, *"does not run on this host"* | The artifact needs a newer glibc than this host has. Repack it with `--distro` naming the oldest release you serve. |
| A check the parent assigns fails with `CheckCommand does not exist` | The root configuration includes `<itl>` but not `<plugins>`: the templates are there, the commands are not. |
| The daemon starts but the master never sees it | Check `NodeName` against the certificate's common name: the `ApiListener` looks for `DataDir/certs/<NodeName>.crt`. |
| `Peer certificate does not match trusted certificate` | `trusted_cert_file` holds the parent's CA instead of the parent's own certificate. Deliver `DataDir/certs/<master-cn>.crt` from the master. |
| The certificate is renewed on every start | The expiry could not be read from `pki verify`, so the Supervisor renews rather than guesses. Check that the parent signs certificates with a validity Icinga prints. |
| The ticket file disappeared | Every Configuration apply rewrites the entry directory. That is harmless after enrolment — the certificate is what matters, and it lives elsewhere. |
