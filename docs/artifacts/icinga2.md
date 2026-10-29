# Artifact: Icinga 2

[← User Manual](../manual/README.md) · [Icinga 2 recipe](../manual/icinga2.md) ·
[The Client](../manual/client.md) · [Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how Icinga 2 is packed, and whoever changes what the `icinga2`
kind knows. It is the one place both sides state the same facts
([ADR-0017](../adr/0017-supervisor-mode-and-its-kinds.md) clause 20,
[ADR-draft-icinga-2](../adr/draft-icinga-2.md)).

Icinga 2 publishes **no portable tree at all** — distribution packages and an MSI, and nothing
else — so the tree is repacked here
([ADR-draft-icinga-2](../adr/draft-icinga-2.md)). The tree's
layout is therefore entirely this project's, and the whole reason this document exists.

**Only the Linux artifact is built.** `opamp-fleetctl package fetch` offers no Windows artifact for
Icinga 2 until a repacked tree has run on a Windows host, and the kind refuses to start on Windows
until then. The Windows column of [what the Client derives](#6-what-the-client-derives) is what the
kind states for that run.

| | |
|---|---|
| Packed by | `icinga2_plans` in `crates/fleet-tools/src/fetch.rs` |
| Run by | `crates/fleet-agent/src/supervisor/icinga2.rs` |
| Agent type | `icinga2` |

## 1. Source

Versions come from the GitHub tags of **`Icinga/icinga2`**, spelled `v<major>.<minor>.<patch>`. The
bytes come from elsewhere:

| | |
|---|---|
| Linux | `https://packages.icinga.com/debian`, the `icinga-<distro>` suite, read through its signed `InRelease` and its `Packages.gz` index — plus Debian's `<distro>`, `<distro>-updates` (`https://deb.debian.org/debian`) and `<distro>-security` (`https://deb.debian.org/debian-security`) for the check plugins and every shared library, the same way; a file `deb.debian.org` answers `404` for is read at the same path below `https://archive.debian.org`, where Debian keeps ended releases |

## 2. Assets per platform

| This fleet | What is fetched |
|---|---|
| `linux/amd64` | `icinga2-bin` and `icinga2-common` from Icinga's suite, `monitoring-plugins-basic` and `monitoring-plugins-common` from the distribution, and the library packages they need from the distribution |

The library packages are the `Depends` and `Pre-Depends` closure of the four in Debian's `libs`
section, glibc left out — 26 for bookworm and Icinga 2 2.16.5. Every download is verified before any
of them is used. The check plugins and the libraries come **from the distribution rather than from
Icinga**, in whatever version that distribution ships on the day of the repack — they are not
versioned with Icinga 2.

**The Linux artifact reaches what its suite reaches, whichever host repacked it.** Nothing in the
tree comes from the host that runs the tool, so `--distro` names any of `bullseye`, `bookworm` and
`trixie` on any host, and omitted it is the host's own. The reach is the highest `libc6` floor any
package of the tree states, which the tool prints before the packages are downloaded.

## 3. Integrity

| | |
|---|---|
| Linux | the SHA-256 the Debian index states for each `.deb`, from an index read only after the suite's `InRelease` verified with `gpgv` and vouched for its SHA-256 and size, and only while its `Valid-Until`, where it states one, has not passed: against **Icinga's archive key** (`DD3A F619 8ED0 00B4 C0B7 3956 CC11 6F55 AA7F 2382`) for Icinga's suite, and against **Debian's automatic signing keys of Debian 11, 12 and 13** for Debian's, the archive keys for a suite and its `-updates` and the security archive keys for its `-security` — both carried by the tool, a signature counting only when its primary key is one of those pinned, and the signed text only when it names, as its `Codename` or `Suite`, the suite it was fetched for |

## 4. Treatment

The `.deb`s are **repacked** into one normalised, link-free tree, under the wrapper directory
`icinga2-<version>`. They are unpacked with `dpkg-deb -x`, which runs no package script, and the
pieces gathered (below). Then the `DT_NEEDED` entries of every program are read out of its ELF
file, followed through each library they name, and every library but glibc and its loader is
copied into `lib/` out of the unpacked library packages. A library none of them holds fails the
repack, naming it and the program that needs it.

Repacking is redistribution, so the copyright file of every package the repack downloads travels
with it, as `doc/<package>.copyright`, a library package none of whose libraries the programs
need among them.

## 5. Form in the delivered tree

Unpacked to `<supervisor_dir>/program/tree/`:

| Path | Holds |
|---|---|
| `sbin/icinga2` | the daemon |
| `lib/` | the gathered libraries |
| `share/icinga2/include/` | the ITL the root configuration `include`s |
| `plugins/` | the check plugins |
| `doc/` | the copyright files of the packages the repack downloaded |

The Linux daemon is taken from `usr/lib`, **not** `/usr/sbin/icinga2` — the latter is a shell
wrapper, and what the Supervisor must spawn is the real program.

## 6. What the Client derives

| | Linux | Windows |
|---|---|---|
| program / `program_path` | `icinga2` / `sbin/icinga2` | `icinga2.exe` / `sbin/icinga2.exe` |
| `service_name` | `icinga2` | `icinga2` |
| include directory | `<tree>/share/icinga2/include` | the same |
| plugin directory | `<tree>/plugins` | `<tree>/sbin` |
| state, log, cache, spool, run | beside the tree, under `<supervisor_dir>/` | the same |
| environment | `LD_LIBRARY_PATH=<tree>/lib` — the tree's own libraries must win over the machine's | none needed |
| version | `--version`, with a parser for Icinga's own banner, reported as `service.version` — the one attribute of the Agent's description the daemon states; where it runs is the Supervisor's ([ADR-0016](../adr/0016-what-an-agent-reports-about-itself.md)) | the same |
| preflight | `--version` against the staged program, with `LD_LIBRARY_PATH` | `--version` |
| reload | `SIGHUP` | restart |
| console severity | `information` | the same |
| `endpoint_port` | refused — Icinga speaks its cluster protocol to its parent, not OpAMP to us | refused |

Three properties are Icinga's own and are why this is a kind rather than a recipe:

- **Nothing creates its directories.** Debian's packages leave that to a helper that needs the
  `nagios` user; a fleet-managed host has neither, so the plugin creates them.
- **The daemon runs a worker of its own**, so it is started in its own process group and the stop
  signals the group — otherwise a killed umbrella leaves the worker holding the data directory and
  port 5665.
- **A failed reload is silent from the outside**: Icinga aborts it and keeps running the old
  configuration, so every apply is validated with `daemon -C` before it reaches the Runner.

The **preflight carries `LD_LIBRARY_PATH`** where the version probe of a published binary would not
need one: a repacked tree raises exactly one question — does this host's libc satisfy it — and
running the staged program's own banner against its own libraries is what answers it before the
running daemon is stopped.

## 7. Configurations

| Name | Role | Agent type | Body |
|---|---|---|---|
| `icinga2-conf` | `main` | `icinga2` | `config/examples/icinga2-conf.conf` |
| `icinga2-zones` | — | `icinga2` | `config/examples/icinga2-zones.conf` |

A Configuration has no Selector: the Deployment that holds it aims it, and its Agent type fits it
to the `icinga2` Agents of that Deployment alone
([ADR-0026](../adr/0026-deployments-and-rollouts.md)).

**Which entry is the root is stated by a role, not by a name.** The daemon is pointed at one file,
from which it `include`s the rest, and being unroled says *"this is configuration"* rather than
*"this is the root"* — so the fleet marks the root with `role = "main"`
([ADR-0025](../adr/0025-configurations-and-the-rest-api.md),
[ADR-draft-icinga-2](../adr/draft-icinga-2.md)). Where nothing is
marked, the conventional name `icinga2-conf` stands in, which is what this tool uploads. Two
entries marked `main` are a reason not to start, naming both.

**The ticket and the parent's certificate are not here.** Both are per host, one is a secret, and
neither has a default this tool could put on a Server — see the
[Icinga 2 recipe](../manual/icinga2.md).

## 8. What can change, and what goes red

| An upstream that… | breaks | caught by |
|---|---|---|
| renames a vendor package or moves its suite | the plan, with a message naming the package | the run itself — the index is read, so a missing package is refused before anything downloads |
| moves the daemon inside the `.deb` | the repack | `repack_debs` fails to find it, naming the file |
| moves the ITL or the plugins | the tree, silently — the daemon starts and finds no templates | the layout tests in `icinga2.rs` pin what this repository writes, not what the vendor ships |
| rotates its repository signing key | the Linux build | the signature check, which refuses the index until the carried key changes |
| Debian starts signing a suite with a new release's key | the Linux build for that suite | the signature check, until the carried keyring and its pinned primaries change |
| moves a library to a package outside `libs` | the repack | the library check, which names the soname and the program that needs it |
| changes the version banner's shape | `service.version`, and the preflight's message | `parse_version`'s tests (client side) |
| raises the glibc floor | the rollout, on older hosts | not a test — the floor is printed before the build, which is the only place it can still be acted on |

The third row is the one to read before a version bump: the tree's internal layout is this
project's, so a vendor that reorganises `usr/share` produces an artifact that packs cleanly and
fails at run time.
