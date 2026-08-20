# ADR-0082: The fleet's own agent is called `supervisor` — the type, the package, the release and the program

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

Supersedes [ADR-0077](0077-the-clients-own-agent-type-is-supervisor.md),
[ADR-0078](0078-a-release-is-named-after-the-set-it-becomes.md) and
[ADR-0080](0080-the-program-and-its-configuration-are-named-supervisor.md) on acceptance, and
carries what they superseded and amended: it **supersedes**
[ADR-0025](0025-release-pipeline-and-artifacts.md) clauses 3 and 4,
[ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) and
[ADR-0030](0030-one-service-name-on-every-platform.md), and **amends**
[ADR-0010](0010-client-os-service-and-cli.md),
[ADR-0027](0027-interactive-install-writes-the-first-configuration.md),
[ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) point 1 and
[ADR-0048](0048-the-packaged-cli-is-a-symlink-through-current.md).

**It decides nothing that was not already decided.** It states the three as one and removes the one
contradiction between them.

## Context

One rename was taken in three steps inside a day, each amending the one before: the Agent *type*
first (ADR-0077), then the *release* and the Set its files become (ADR-0078), then the *program* on
the host, its service and its configuration file (ADR-0080). Each step was right when it was taken
and each was written as what it was — a decision about one layer.

Read afterwards, the three are one decision arrived at in instalments, and they cost the reader
three documents and one outright contradiction: ADR-0078 states that "the product keeps its name …
the binary, the service and its display name, the versioned install layout … are all
`opamp-fleet-client`, untouched", which ADR-0080 then overturned two hours later. Nothing in the
tree is wrong, but nothing in the tree says the whole thing either.

**Numbers are permanent here**, so this is a supersession and not a merge: `docs/adr/README.md`
process rule 6 forbids renumbering, deleting or merging ADRs, because other ADRs, commit messages
and about a hundred and ninety code and documentation citations name these numbers — and `0.4.0`
and `0.4.1` shipped with them inside. The sanctioned way to retire several documents into one is the
one this repository has already used: [ADR-0061](0061-a-rollout-is-an-explicit-act.md) supersedes
[ADR-0043](0043-a-package-is-published-before-it-is-offered.md) *and*
[ADR-0055](0055-a-configuration-is-published-before-it-is-offered.md). The three retired here stay
where they are, as record, with their status pointing at this one.

**Why the word.** The Baseline reserves `service.name` for the Agent *type* and recommends a reverse
FQDN — a recommendation ADR-0033 already declined to enforce, since neither a Collector's
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
   [ADR-0075](0075-the-self-update-consent-stands-unless-it-is-withdrawn.md)'s rule intact in letter
   and in substance: the default is not a wildcard, it is the one package that could legitimately be
   this Client, and an offer under any other name is refused and reported. The Set is therefore
   `supervisor` @ version @ `supervisor` — name and type the same string, the way every other
   agent's Set already reads ([ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md),
   [ADR-0052](0052-a-package-is-a-versioned-set.md)). A Configuration carrying the Client's
   `[[supervisor]]` blocks ([ADR-0056](0056-the-client-accepts-its-supervisor-set-from-the-server.md),
   [ADR-0057](0057-server-pushed-supervisor-blocks-name-only-client-owned-programs.md)) is typed
   `supervisor` as well.
3. **Every release artifact is a `.tar.gz` named `supervisor_<version>_<os>_<arch>`** — after the Set
   the files become, not after the product inside them. `.tar.gz` because it is the container every
   other agent's package already ships as ([ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md),
   [ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)): the only one that
   carries the executable bit and unpacks the same way on every platform. `.7z` remains an artifact
   container the Client opens and the packer writes, including encrypted
   ([ADR-0018](0018-packages-imported-from-a-url.md)); a *release* has no use for encryption, its
   bytes being published with their checksum beside them.
   - **All four artifacts of a target share the name**, as [ADR-0046](0046-a-release-ships-native-installers.md)
     clause 4 requires: the `.tar.gz`, the `.deb`, the `.rpm` and the `.msi` differ in extension alone.
   - **The fields and their separator are unchanged** ([ADR-0032](0032-release-artifacts-separate-their-fields-with-underscores.md)):
     `_` between four fields, the last two exactly what an Agent reports as `os.type` and
     `host.arch`, so an upload reads the platform out of the file name and needs no table.
4. **The program on the host is `supervisor`** (`supervisor.exe` on Windows): the binary Cargo
   builds, the file in every version directory, the member a package artifact carries, the payload
   under `/usr/libexec`, and the `PATH` symlink ADR-0048 lays through `current`. Its **service** is
   `supervisor` on systemd, launchd and the SCM, with the instance suffixed as before
   (`supervisor-prod`) — ADR-0030's rule with its string replaced — and the version directories
   follow the same constant, `supervisor-<MAJOR.MINOR.PATCH>-<hash>`. So do the log file, the
   self-check token, and the CLI's own name.
5. **The configuration file is `supervisor.toml`**, and the `--config` default with it. There is no
   fallback to the old name — and because there is none, **a Client that finds a `client.toml` beside
   the `supervisor.toml` it was looking for refuses to start**, naming both paths and the command
   that fixes it. Coming up on defaults there, dialling the development endpoint and managing
   nothing, is the one outcome nobody would see.
6. **The instance name stays a separate attribute, and its default is `Supervisor Agent`.** The
   top-level `name` is the operator's name for *this* Client, reported as `service.instance.name`
   (ADR-0033 point 2, untouched). Its default is a display name — spaces and capitals — and
   deliberately not `supervisor`: a default equal to the type would print the same word in both
   columns of the fleet view, which is the collapse ADR-0033 ended. Nothing resolves a path or a
   service from this key; the ADR-0010 grammar governs `--instance` and the `[[supervisor]]` block
   names instead.
7. **The break is not carried across.** No dual-named artifact, no compatibility link in a version
   directory, no reading of the old configuration name. A deployed Client offered the renamed release
   refuses it — the member it asks for by name is not in the artifact — and stays on the version it
   runs, which is the safe half of ADR-0028's hazard and the only half that survives this decision.
8. **That release is installed, not updated**: by `.deb`, `.rpm` or MSI, or by hand. Every one of
   those paths re-runs `service install`, which registers the new service — and the Linux
   post-install additionally **deregisters the old one**, because the package's own pre-removal
   deliberately falls through on an upgrade and the old unit would otherwise survive, pointing at a
   file the new layout does not have. It also removes the old `PATH` symlink and the orphaned
   `opamp-fleet-client-*` version directories, the way it already does the
   [ADR-0053](0053-the-linux-service-executes-from-opt.md) migration.
9. **What keeps its name, and why:**

   | Stays | Because |
   |---|---|
   | the install roots, `/opt/opamp-fleet/client/<instance>` and `/var/lib/opamp-fleet/client/<instance>` (ADR-0010, ADR-0053) | the state directory holds the instance UID and the configuration; moving it makes every host a *new* Agent in the fleet view and loses the credential an operator typed |
   | the dpkg and rpm package identity, the MSI `ProductName` and `UpgradeCode` (ADR-0046) | an `apt`, `dnf` or MSI upgrade stays an upgrade rather than becoming a second product beside the first |
   | the Cargo package name `client` | a build-time identifier that never leaves the repository (ADR-0028's own reasoning, undisturbed) |
   | the OTLP instrumentation scope | it names the library a signal came from, and renaming it would move every operator's dashboards for no gain here |

10. **It is a breaking change for a deployed fleet, and `CHANGELOG.md` says so** — as
    [ADR-0031](0031-per-platform-package-variants.md) and ADR-0033 point 5 did for the same class of
    silent break. It shipped as `0.4.0`, with the per-host procedure written out: upgrade by package,
    rename the configuration, register and start. Nothing mis-delivers on the way — a type that does
    not fit is offered nothing, a package name that does not match is refused and reported, and a
    host that is upgraded but not renamed does not start.

## Alternatives considered

- **Leave the three ADRs as they stand.** They are each coherent about their own layer, and their
  headers point a reader forward. Rejected: the reader has to hold three documents to learn what one
  thing is called, and one of the three states a fact the next one overturned.
- **Flatten the ADR history on a branch** — rewrite the record so the three were always one, saving
  the numbers. Rejected outright: process rule 6, and the arithmetic behind it. Two releases carry
  those numbers in their code comments and their CHANGELOG.
- **Keep `opamp-fleet-client` everywhere.** The status quo of a day earlier, and not broken.
  Rejected: the type then repeats a product name the row already carries as its instance name, and
  the fleet cannot say what the Client *is* except by naming the product it happens to be.
- **A reverse FQDN, `io.opamp-fleet.supervisor`**, as the Baseline recommends. Rejected on ADR-0033's
  reasoning: the shape is a recommendation this project enforces nowhere else, and the string is a
  table column in the operator plane, where the short form is what gets read.
- **Rename the type but not the package, the release or the program** — each of the three steps,
  taken alone. Rejected in turn as each was written: a package name that is not the type splits
  ADR-0075's one rule into two names; a release named after the product publishes a Set no Client
  fits; a program called `opamp-fleet-client` under a service called `supervisor` is the doubled
  vocabulary this decision exists to end.
- **A transitional release carrying both program names** — the artifact holding the program twice,
  each version directory laying the old name beside the new, the loader accepting either
  configuration file. The only option under which a deployed host updates itself across the rename.
  Rejected deliberately: it re-creates the defect ADR-0028 removed — one file with two names — in the
  packer, the layout, the loader and the documentation at once, and ends in a second decision about
  when to withdraw the old name that every host has to survive as well.
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
- Bindplane's Agent Type, surveyed in ADR-0033: the type derives from the distribution's own name and
  is a separate field from the human-readable Agent name — the split this project implements.
- [ADR-0025](0025-release-pipeline-and-artifacts.md) clauses 3 and 4, and the alternatives entry that
  rejected a *split* container (`.tar.gz` on Unix, `.zip` on Windows) rather than a uniform one.
- [ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) — the naming decision this replaces, and
  the two hazards it recorded for exactly this moment: the service unit points at the file, and the
  self-update extracts the file by name. They are why point 7 is a break and not a transition.
- [ADR-0030](0030-one-service-name-on-every-platform.md) — one service name everywhere, kept in shape
  and changed in string; its collision argument is revisited under Consequences.

## Consequences

- **Positive:** one word for one thing, from the Agent type in the fleet view down to the unit an
  operator restarts and the file they edit. Every runbook line, every artifact name and every
  attribute says `supervisor`.
- **Positive:** the Client's own release is an ordinary fleet package — same container as every other
  agent's, and a Set whose name and type agree — so nothing about it is a special case any more, and
  the documented upload procedure produces a Set that actually fits a Client.
- **Positive:** one document to read. The three it retires stay as record for anyone tracing how the
  decision was reached.
- **Negative — the term now names three things in this project**: the unit inside a Client that
  manages one Managed Process (the specification's *Supervisor*), the Agent type, and the program
  with its service. Documentation keeps them apart by never using the bare word where a file or a
  unit is not meant: `service.name = "supervisor"` for the type, `[[supervisor]]` for the block.
- **Negative — every deployed host had to be touched once.** A self-update cannot cross the rename;
  the upgrade is `apt`/`dnf`/MSI or a manual `service install`, and the configuration file has to be
  renamed by hand on each host, because an installer that did it would leave exactly the hosts it
  cannot reach without one.
- **Negative — `supervisor` is a common word in `/usr/bin` and in `systemctl`.** ADR-0030 chose the
  old name partly because it "does not collide"; this one might. It does not collide with the
  best-known claimant — Debian's `supervisor` package installs `supervisord` and `supervisorctl` —
  but the margin is thinner, and a host running both has two things an operator could mean by the
  word. The instance suffix (`supervisor-prod`) is unchanged and still how two of ours are told apart.
- **Negative — artifact names are a public contract** (ADR-0025 says so in as many words), and this
  broke it twice over: the name and the extension. There was no deprecation window, because the old
  name published a package that no longer installed.
- **Follow-ups:** the release after the rename is an ordinary self-update again, and the first that
  proves the loop end to end on a real host rather than in the test that stands in for the service
  manager.
