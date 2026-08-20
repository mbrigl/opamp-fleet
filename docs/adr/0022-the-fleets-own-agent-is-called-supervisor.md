# ADR-0022: The fleet's own agent is called `supervisor` — the type, the package, the release and the program

- **Status:** 🟡 proposed
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

Supersedes [ADR-0021](0021-release-pipeline-and-artifacts.md) clauses 3 and 4, and **amends**
[ADR-0010](0010-client-os-service.md) and [ADR-0023](0023-what-describes-an-agent.md) point 1 and
[ADR-0021](0021-release-pipeline-and-artifacts.md).

## Context

The fleet's own agent has a name at several layers: the Agent *type* it reports, the *release* and
the Set its files become, and the *program* on the host with its service and its configuration
file. This decision gives all of them one word.

**Why the word.** The Baseline reserves `service.name` for the Agent *type* and recommends a reverse
FQDN — a recommendation ADR-0023 already declined to enforce, since neither a Collector's
`dist.name` nor a program's file name generally is one, and which open-telemetry/opamp-spec issue
131 records as a known overload of that key against the resource semantic conventions. So the value
was this project's to choose, and among Collectors, Foreign Agents and the process that supervises
them on a host, the useful answer for the last one is the role it plays. Everything else followed
from taking that answer seriously: what the fleet offers the thing is named after what the thing is,
and so is the thing itself.

## Decision

We will call the fleet's own agent **`supervisor`** at every layer it has a name.

1. **The Agent type is `supervisor`** — the constant every Client reports as `service.name`
   ([`agent.rs`](../../crates/client/src/supervisor/agent.rs)), the same on every host, because
   every Client in a fleet is the same kind of thing and that is what a type says.
2. **The package that carries the Client is `supervisor` too.** `[self_update] package` defaults to
   the Agent type ([`config.rs`](../../crates/client/src/config.rs)), which keeps
   [ADR-0018](0018-client-self-update.md)'s rule intact in letter
   and in substance: the default is not a wildcard, it is the one package that could legitimately be
   this Client, and an offer under any other name is refused and reported. The Set is therefore
   `supervisor` @ version @ `supervisor` — name and type the same string, the way every other
   agent's Set already reads ([ADR-0017](0017-package-artifacts.md),
   [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md)). A Configuration carrying the Client's
   `[[supervisor]]` blocks ([ADR-0019](0019-supervisor-directory-programs-and-supervisor-set.md)) is typed
   `supervisor` as well.
3. **Every release artifact is a `.tar.gz` named `supervisor_<version>_<os>_<arch>`** — after the Set
   the files become, not after the product inside them. `.tar.gz` because it is the container every
   other agent's package already ships as ([ADR-0030](0030-glpi-agent-supervision-and-packages.md),
   [ADR-0032](0032-icinga-2-through-the-fleet.md)): the only one that
   carries the executable bit and unpacks the same way on every platform. `.7z` remains an artifact
   container the Client opens and the packer writes, including encrypted
   ([ADR-0017](0017-package-artifacts.md)); a *release* has no use for encryption, its
   bytes being published with their checksum beside them.
   - **All four artifacts of a target share the name**, as [ADR-0021](0021-release-pipeline-and-artifacts.md)
     clause 16 requires: the `.tar.gz`, the `.deb`, the `.rpm` and the `.msi` differ in extension alone.
   - **The fields and their separator are unchanged** ([ADR-0021](0021-release-pipeline-and-artifacts.md)):
     `_` between four fields, the last two exactly what an Agent reports as `os.type` and
     `host.arch`, so an upload reads the platform out of the file name and needs no table.
4. **The program on the host is `supervisor`** (`supervisor.exe` on Windows): the binary Cargo
   builds, the file in every version directory, the member a package artifact carries, the payload
   under `/usr/libexec`, and the `PATH` symlink ADR-0021 lays through `current`. Its **service** is
   `supervisor` on systemd, launchd and the SCM: a single token that every backend renders the
   same, bare for the default instance and suffixed for any other (`supervisor-prod`). The version
   directories follow the same constant, `supervisor-<MAJOR.MINOR.PATCH>-<hash>`. So do the log
   file, the self-check token, and the CLI's own name.
5. **The configuration file is `supervisor.toml`**, and the `--config` default with it. There is no
   fallback to the old name — and because there is none, **a Client that finds a `client.toml` beside
   the `supervisor.toml` it was looking for refuses to start**, naming both paths and the command
   that fixes it. Coming up on defaults there, dialling the development endpoint and managing
   nothing, is the one outcome nobody would see.
6. **The instance name stays a separate attribute, and its default is `Supervisor Agent`.** The
   top-level `name` is the operator's name for *this* Client, reported as `service.instance.name`
   (ADR-0023 point 2, untouched). Its default is a display name — spaces and capitals — and
   deliberately not `supervisor`: a default equal to the type would print the same word in both
   columns of the fleet view, which is the collapse ADR-0023 ended. Nothing resolves a path or a
   service from this key; the ADR-0010 grammar governs `--instance` and the `[[supervisor]]` block
   names instead.
7. **The break is not carried across.** No dual-named artifact, no compatibility link in a version
   directory, no reading of the old configuration name. A deployed Client offered the renamed release
   refuses it — the member it asks for by name is not in the artifact — and stays on the version it
   runs. A rename carries two hazards: the service unit points at the program file, and the
   self-update extracts that file by name. The refusal is the safe half, and the only half that
   survives this decision.
8. **That release is installed, not updated**: by `.deb`, `.rpm` or MSI, or by hand. Every one of
   those paths re-runs `service install`, which registers the new service — and the Linux
   post-install additionally **deregisters the old one**, because the package's own pre-removal
   deliberately falls through on an upgrade and the old unit would otherwise survive, pointing at a
   file the new layout does not have. It also removes the old `PATH` symlink and the orphaned
   `opamp-fleet-client-*` version directories, the way it already removes the orphaned `versions/`
   and `current` under the old `/var/lib` default root.
9. **What keeps its name, and why:**

   | Stays | Because |
   |---|---|
   | the install roots, `/opt/opamp-fleet/client/<instance>` and `/var/lib/opamp-fleet/client/<instance>` (ADR-0010) | the state directory holds the instance UID and the configuration; moving it makes every host a *new* Agent in the fleet view and loses the credential an operator typed |
   | the dpkg and rpm package identity, the MSI `ProductName` and `UpgradeCode` (ADR-0021) | an `apt`, `dnf` or MSI upgrade stays an upgrade rather than becoming a second product beside the first |
   | the Cargo package name `client` | a build-time identifier that never leaves the repository; the shipped name is the binary target's |
   | the OTLP instrumentation scope | it names the library a signal came from, and renaming it would move every operator's dashboards for no gain here |

10. **It is a breaking change for a deployed fleet, and `CHANGELOG.md` says so** — as
    [ADR-0017](0017-package-artifacts.md) and ADR-0023 point 5 did for the same class of
    silent break. It shipped as `0.4.0`, with the per-host procedure written out: upgrade by package,
    rename the configuration, register and start. Nothing mis-delivers on the way — a type that does
    not fit is offered nothing, a package name that does not match is refused and reported, and a
    host that is upgraded but not renamed does not start.

## Alternatives considered

- **Keep `opamp-fleet-client` everywhere.** The status quo of a day earlier, and not broken.
  Rejected: the type then repeats a product name the row already carries as its instance name, and
  the fleet cannot say what the Client *is* except by naming the product it happens to be.
- **A reverse FQDN, `io.opamp-fleet.supervisor`**, as the Baseline recommends. Rejected on ADR-0023's
  reasoning: the shape is a recommendation this project enforces nowhere else, and the string is a
  table column in the operator plane, where the short form is what gets read.
- **Rename the type but not the package, the release or the program** — any one layer taken
  alone. Rejected for each layer: a package name that is not the type splits
  ADR-0018's one rule into two names; a release named after the product publishes a Set no Client
  fits; a program called `opamp-fleet-client` under a service called `supervisor` is the doubled
  vocabulary this decision exists to end.
- **A transitional release carrying both program names** — the artifact holding the program twice,
  each version directory laying the old name beside the new, the loader accepting either
  configuration file. The only option under which a deployed host updates itself across the rename.
  Rejected deliberately: it re-creates one file with two names, the defect a single shipped name
  removes, in the packer, the layout, the loader and the documentation at once, and ends in a
  second decision about when to withdraw the old name that every host has to survive as well.
- **Rename everything, package identity included** (dpkg/rpm identity, MSI `ProductName`). Rejected:
  it buys consistency where nobody reads and costs every host a second package beside the first, with
  the old one left installed.

## Sources / Prior art

- [OpAMP specification, `AgentDescription`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `service.name` "should be set to a reverse FQDN that uniquely identifies the Agent type".
- [opamp-spec issue #131, "Opamp spec overloads definition of service.name"](https://github.com/open-telemetry/opamp-spec/issues/131)
  — that reading contradicts the resource semantic conventions, where `service.name` is the logical
  name of the service. The recommendation is therefore guidance, not a constraint on the value.
- [OpenTelemetry resource semantic conventions, `service.name`](https://opentelemetry.io/docs/specs/semconv/resource/#service).
- Bindplane's Agent Type, surveyed in ADR-0023: the type derives from the distribution's own name and
  is a separate field from the human-readable Agent name — the split this project implements.
- [ADR-0021](0021-release-pipeline-and-artifacts.md) clauses 3 and 4, and the alternatives entry that
  rejected a *split* container (`.tar.gz` on Unix, `.zip` on Windows) rather than a uniform one.

## Consequences

- **Positive:** one word for one thing, from the Agent type in the fleet view down to the unit an
  operator restarts and the file they edit. Every runbook line, every artifact name and every
  attribute says `supervisor`.
- **Positive:** the Client's own release is an ordinary fleet package — same container as every other
  agent's, and a Set whose name and type agree — so nothing about it is a special case any more, and
  the documented upload procedure produces a Set that actually fits a Client.
- **Negative — the term now names three things in this project**: the unit inside a Client that
  manages one Managed Process (the specification's *Supervisor*), the Agent type, and the program
  with its service. Documentation keeps them apart by never using the bare word where a file or a
  unit is not meant: `service.name = "supervisor"` for the type, `[[supervisor]]` for the block.
- **Negative — every deployed host had to be touched once.** A self-update cannot cross the rename;
  the upgrade is `apt`/`dnf`/MSI or a manual `service install`, and the configuration file has to be
  renamed by hand on each host, because an installer that did it would leave exactly the hosts it
  cannot reach without one.
- **Negative — `supervisor` is a common word in `/usr/bin` and in `systemctl`.** Dropping the
  reverse-DNS label convention rests on the name not colliding, and this one might. It does not
  collide with the best-known claimant — Debian's `supervisor` package installs `supervisord` and
  `supervisorctl` — but the margin is thinner, and a host running both has two things an operator could mean by the
  word. The instance suffix (`supervisor-prod`) is unchanged and still how two of ours are told apart.
- **Negative — artifact names are a public contract** (ADR-0021 says so in as many words), and this
  broke it twice over: the name and the extension. There was no deprecation window, because the old
  name published a package that no longer installed.
- **Follow-ups:** the release after the rename is an ordinary self-update again, and the first that
  proves the loop end to end on a real host rather than in the test that stands in for the service
  manager.
