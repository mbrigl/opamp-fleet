# ADR-0078: A release is named after the Set it becomes, and packed as `.tar.gz` like every other package

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

[ADR-0025](0025-release-pipeline-and-artifacts.md) decided two things about what a release publishes,
and both have been overtaken by decisions taken since — neither of which existed when it was written.

**The container.** ADR-0025 clause 3 packs each artifact as `.7z`, and its Alternatives section
rejected *"`.tar.gz` on Unix, `.zip` on Windows, the common convention"* on a sound argument: the
Client cannot open a `.zip` at all, so that convention would split the fleet's install path by
platform. But the rejected option was a **split**, not `.tar.gz` everywhere — and `.tar.gz`
everywhere is exactly what [ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md)
and [ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md) later settled for
every *other* agent's package, Windows included, for a reason ADR-0025 did not have in front of it:
it is the only container that carries the executable bit, and the Client unpacks it on every
platform. The result is that the Client's own release artifact became the one package in the fleet
in a container of its own.

The second half of ADR-0025's argument — *"the one the packer can also encrypt when an operator
needs that"* — is not lost by this. `.7z` remains an accepted artifact container
([ADR-0018](0018-packages-imported-from-a-url.md)) and `opamp-package-sign pack --format 7z` still
writes it. It is a *release* that has no use for encryption: its bytes are published on a public
release page, with their checksum beside them.

**The name.** [ADR-0077](0077-the-clients-own-agent-type-is-supervisor.md) made the Client's own
Agent type `supervisor`, and the package that carries it took the same name. The published file
names did not follow, and that was not cosmetic: a file called
`opamp-fleet-client_<version>_<os>_<arch>` states a package name no Set uses any more, and the
upload procedure in the release notes still built its Set at
`/api/v1/packages/opamp-fleet-client/opamp-fleet-client/<version>` — whose second field is the Agent
type, so the Set fitted no Client at all ([`fits_agent`](../../crates/server/src/packages.rs)
refuses a type the Agent does not report). The release was publishing a package the fleet could not
install.

## Decision

We will publish every release artifact as **`.tar.gz`**, named **`supervisor_<version>_<os>_<arch>`**
— after the Set the files become, not after the product inside them.

This **supersedes [ADR-0025](0025-release-pipeline-and-artifacts.md) clauses 3 and 4** (the container
and the artifact name) and **[ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) on its
artifact-name clause only**. Everything else both ADRs decide stands.

Bound by this decision:

- **The product keeps its name.** The binary, the service and its display name (ADR-0030), the
  versioned install layout (ADR-0010), the dpkg and rpm package identity from `Cargo.toml`, and the
  MSI's ProductName and UpgradeCode are all `opamp-fleet-client`, untouched. Only the *file names*
  and the Set they are uploaded to change, which is why an `apt` or `dnf` upgrade across this
  release is an ordinary upgrade and not a second package beside the first.
- **All four artifacts of a target share the name**, as ADR-0046 clause 4 requires — the `.tar.gz`,
  the `.deb`, the `.rpm` and the `.msi` differ in extension alone. The pipeline keeps one stem per
  matrix row; what changed is what that stem is built from.
- **The Set is `supervisor@<version>@supervisor`** — name and type the same string, the way every
  other agent's Set already reads (`icinga2@…@icinga2`), and the same string a Client's
  `[self_update] package` defaults to (ADR-0075). The release notes' upload procedure says so.
- **The fields and their separator are unchanged** (ADR-0032): `_` between four fields, the last two
  exactly what an Agent reports as `os.type` and `host.arch`, so the upload still reads them out of
  the file name and needs no table.

## Alternatives considered

- **Rename only the Set, leave the files `opamp-fleet-client_…`.** Smaller, and it keeps ADR-0028
  untouched. Rejected because ADR-0032's whole point is that the file name *states* the four fields
  the upload needs, the first of which is the package name — a file whose first field named
  something else would make the name a decoration and the procedure a lookup.
- **Rename everything, product included** — the binary, the service, the installed file. Rejected:
  that is ADR-0028's actual subject and its reasoning is undisturbed by any of this. What an
  operator installs is a fleet client; what the Server offers it is a package named after the kind
  of Agent it is.
- **Keep `.7z` and rename only.** Possible, and it would leave the Client's artifact the only one in
  the fleet needing a second container. The two changes travel together because they are the same
  question — what a release publishes — asked twice.
- **Ship both containers for a transition.** Doubles every asset and the checksum file to spare a
  `curl` in somebody's script. Rejected; the rename breaks those scripts anyway, so one break is
  cheaper than two.

## Sources / Prior art

- [ADR-0025](0025-release-pipeline-and-artifacts.md) clauses 3 and 4, and the Alternatives entry
  that rejected a *split* container rather than a uniform one.
- [ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md),
  [ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md) — `.tar.gz` on every
  platform, and why: the executable bit, and one unpack path.
- [ADR-0077](0077-the-clients-own-agent-type-is-supervisor.md) — the name this follows, and
  [ADR-0075](0075-the-self-update-consent-stands-unless-it-is-withdrawn.md), whose default is the
  same string.
- [ADR-0018](0018-packages-imported-from-a-url.md) — `.7z` stays a container the Client opens and
  the packer writes, including encrypted; this decision is about releases, not about artifacts.

## Consequences

- Positive: the Client's own release is an ordinary fleet package — same container as every other
  agent's, and a Set whose name and type agree, so nothing about it is a special case any more.
- Positive: the documented upload procedure produces a Set that actually fits a Client, which since
  ADR-0077 it did not.
- **Negative / trade-offs: artifact names are a public contract** (ADR-0025 says so in as many
  words), and this breaks it twice over — the name and the extension. A script fetching
  `opamp-fleet-client_*.7z` finds nothing. There is no deprecation window; the old name published a
  package that no longer installs, so keeping it alive would preserve a broken thing.
- **Negative / trade-offs: an already-deployed Client will refuse the renamed package.** The
  self-update gate is the package *name*: a host whose `client.toml` names `opamp-fleet-client` —
  which is every host configured before ADR-0077 — reports *"this Agent installs only the package
  "opamp-fleet-client"; the Server offered "supervisor""* and stays on its version. The refusal is
  loud and harmless, and the fix is one line in `client.toml` (`package = "supervisor"`, or delete
  the section and take the default), but it has to happen on the host, which is the one place this
  project tries not to send people. Stated in the CHANGELOG under *What to do*.
- Negative / trade-offs: `SHA256SUMS` and every release-page link change shape in the same release
  as the rename, so an operator comparing two releases side by side sees two conventions.
