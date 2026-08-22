# Artifact: Icinga 2

[← User Manual](../manual/README.md) · [Icinga 2 recipe](../manual/icinga2.md) ·
[The Client](../manual/client.md) · [Command-line tools](../manual/tools.md)

**Who reads this.** Whoever changes how Icinga 2 is packed, and whoever changes what the `icinga2`
kind knows. It is the one place both sides state the same facts
([ADR-0091](../adr/0091-a-kind-knows-its-own-agent.md) clause 9,
[ADR-0092](../adr/0092-icinga-2s-block-keeps-only-what-enrolment-needs.md)).

Icinga 2 publishes **no portable tree at all** — distribution packages and an MSI, and nothing
else — so both platforms are repacked here
([ADR-0070](../adr/0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)). The tree's
layout is therefore entirely this project's, and the whole reason this document exists.

| | |
|---|---|
| Packed by | `icinga2_plans` / `windows_plan` in `crates/package-tools/src/bin/opamp-package-fetch.rs` |
| Run by | `crates/client/src/supervisor/icinga2.rs` |
| Agent type | `icinga2` |

## 1. Source

Versions come from the GitHub tags of **`Icinga/icinga2`**, spelled `v<major>.<minor>.<patch>`. The
bytes come from elsewhere:

| | |
|---|---|
| Linux | `https://packages.icinga.com/debian`, the `icinga-<distro>` suite, read through its `Packages.gz` index — plus `https://deb.debian.org/debian` for the check plugins |
| Windows | `https://packages.icinga.com/windows/Icinga2-v<version>-x86_64.msi` |

## 2. Assets per platform

| This fleet | What is fetched |
|---|---|
| `linux/amd64` | `icinga2-bin` and `icinga2-common` from Icinga's suite, `monitoring-plugins-basic` and `monitoring-plugins-common` from the distribution |
| `windows/amd64` | the MSI |

Four downloads for one Linux artifact, and each is verified before any of them is used. The check
plugins come **from the distribution rather than from Icinga**, in whatever version that
distribution ships — they are not versioned with Icinga 2.

**The Linux artifact is only as portable as the host that built it.** The tree bundles what `ldd`
resolves on the build host, so a run produces the artifact for *that* distribution and no other.
That is why `opamp-package-fetch` asks the host for Icinga 2's platform line instead of naming one,
and why it prints the vendor's own glibc floor before the rollout rather than after it.

## 3. Integrity

| | |
|---|---|
| Linux | the SHA-256 the Debian index already states for each `.deb` — no separate checksum file is fetched |
| Windows | **the publisher's signature**, `O=Icinga GmbH`: Icinga publishes no digest for the MSI ([ADR-0072](../adr/0072-the-windows-artifact-is-verified-by-its-publisher.md)) |

## 4. Treatment

Both platforms are **repacked** into the same normalised, link-free tree, under the wrapper
directory `icinga2-<version>`:

- **Linux** — the vendor's `.deb`s are unpacked and the pieces gathered (below), then every program
  is scanned and the libraries it resolves are bundled beside it. A build host missing one of the
  vendor's declared dependencies cannot produce a complete artifact, and the refusal names the
  command that fixes it.
- **Windows** — the MSI's payload is unpacked into the same shape. **No libraries are gathered**:
  the payload already carries its DLLs beside the executable, which is where Windows looks first.

Repacking is redistribution, so the vendor's copyright files travel with the tree — and only those.

## 5. Form in the delivered tree

Unpacked to `<supervisor_dir>/program/tree/`:

| Path | Holds | Linux | Windows |
|---|---|---|---|
| `sbin/` | the daemon | `icinga2` | `icinga2.exe`, **and the check plugins** |
| `lib/` | the gathered libraries | yes | — |
| `share/icinga2/include/` | the ITL the root configuration `include`s | yes | yes |
| `plugins/` | the check plugins | yes | — |
| `doc/` | the vendors' copyright files | yes | yes |

The Linux daemon is taken from `usr/lib`, **not** `/usr/sbin/icinga2` — the latter is a shell
wrapper, and what the Supervisor must spawn is the real program.

The check plugins sit **beside the daemon in `sbin/` on Windows** rather than in `plugins/`: a
Windows program finds its DLLs in its own directory first, and separating the check executables
from the runtime they share with the daemon would break them
([ADR-0072](../adr/0072-the-windows-artifact-is-verified-by-its-publisher.md)).

## 6. What the Client derives

| | Linux | Windows |
|---|---|---|
| program / `program_path` | `icinga2` / `sbin/icinga2` | `icinga2.exe` / `sbin/icinga2.exe` |
| `service_name` | `icinga2` | `icinga2` |
| include directory | `<tree>/share/icinga2/include` | the same |
| plugin directory | `<tree>/plugins` | `<tree>/sbin` |
| state, log, cache, spool, run | beside the tree, under `<supervisor_dir>/` | the same |
| environment | `LD_LIBRARY_PATH=<tree>/lib` — the tree's own libraries must win over the machine's | none needed |
| version | `--version`, with a parser for Icinga's own banner | the same |
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

| Name | Role | Aimed at | Body |
|---|---|---|---|
| `icinga2-conf` | `main` | every Agent of type `icinga2` | `config/examples/icinga2-conf.conf` |
| `icinga2-zones` | — | every Agent of type `icinga2` | `config/examples/icinga2-zones.conf` |

**Which entry is the root is stated by a role, not by a name.** The daemon is pointed at one file,
from which it `include`s the rest, and being unroled says *"this is configuration"* rather than
*"this is the root"* — so the fleet marks the root with `role = "main"`
([ADR-0016](../adr/0016-configuration-content-role.md),
[ADR-0092](../adr/0092-icinga-2s-block-keeps-only-what-enrolment-needs.md)). Where nothing is
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
| stops signing the MSI, or changes the subject | the Windows verification | the publisher check, which refuses the artifact |
| changes the version banner's shape | `service.version`, and the preflight's message | `parse_version`'s tests (client side) |
| raises the glibc floor | the rollout, on older hosts | not a test — the floor is printed before the build, which is the only place it can still be acted on |

The third row is the one to read before a version bump: the tree's internal layout is this
project's, so a vendor that reorganises `usr/share` produces an artifact that packs cleanly and
fails at run time.
