# ADR-0031: One platform vocabulary from the release file name to the offer — a package is one name with one artifact per platform

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **artifact-naming** decision of
[ADR-0025](0025-release-pipeline-and-artifacts.md) — the `<os>` and `<arch>` tokens in the release
file name and the spelling of them in its target table. Everything else that ADR decides — the
`version/*` trigger, which five targets are built and on which runners, the `.7z` container written
by this project's own packer, the single job that reads the version, and what a release publishes —
is untouched and still binding. It extends [ADR-0017](0017-selector-targeted-packages.md) rather
than replacing it: Selector aiming and its specificity rule survive unchanged, with a new step in
front of them.

## Context

ADR-0017 moved the decision *which artifact an Agent gets* from a file on every host to the Server,
and pointed at the heterogeneous fleet as the case it solves: "`host.arch` and `os.type` are already
reported, so one package per platform, each with a Selector, updates every machine from the Server".
That sentence is the whole of the platform story today, and it does not hold up.

**A package name carries exactly one artifact.** The store is a `BTreeMap<String, Package>` keyed by
name, persisted as `<name>.json` and `<name>.bin`. "One package per platform" therefore means one
*name* per platform — `otelcol-linux-amd64`, `otelcol-darwin-arm64`, and so on — and a Selector on
each that repeats, as an equality pair, what the name already says.

**For the Client's own update that shape does not work at all.** `[self_update] package` names
**one** package, and the Client refuses any offer under a different name
([`agent.rs:637`](../../crates/client/src/supervisor/agent.rs#L637)) — deliberately, because that
name is the only thing standing between a fleet-wide Collector artifact and every Client binary in
the fleet (ADR-0020). Meanwhile ADR-0025 publishes **five** artifacts per release. So an operator has
exactly two options, and both are bad: put one platform's build in the fleet under
`opamp-fleet-client` — which is what the release notes currently show
([`release.yml:238`](../../.github/workflows/release.yml#L238)), and what makes uploading the second
platform's build silently overwrite the first for the whole fleet — or give each platform its own
package name and write the matching name into `client.toml` **on every host**. The second is the
per-host wiring ADR-0017 exists to remove, reintroduced at the one place where the blast radius is
the Client itself.

**Nothing stops a mismatched binary today.** A package's Selector is the only filter, and an empty
Selector reaches every Agent that accepts packages. An operator who uploads a Windows artifact and
forgets the Selector has it downloaded, verified, unpacked and swapped over the binary on every Linux
host in the fleet. The Client's health gate catches it — the process will not stay up and is rolled
back (ADR-0015) — but that is a fleet-wide outage window, discovered per host, for a mistake the
Server had every attribute in hand to refuse. The Selector *can* express the platform, but it is
opt-in, and the failure mode of forgetting it is the worst one this system has.

**And nothing in this project spells a platform the same way twice.** Three vocabularies meet here:

| Where | Linux / macOS / Windows | 64-bit Intel | 64-bit ARM |
|---|---|---|---|
| What the Client reports ([`agent.rs:796`](../../crates/client/src/supervisor/agent.rs#L796)) | `linux` / `darwin` / `windows` | `x86_64` (Rust's `std::env::consts::ARCH`) | `aarch64` |
| Semantic conventions, and what `opampextension` reports (`runtime.GOOS`/`GOARCH`) | `linux` / `darwin` / `windows` | `amd64` | `arm64` |
| ADR-0025 release file names | `linux` / `macos` / `windows` | `x86_64` | `aarch64` |

The operating system is very nearly settled — `os.type` is what the Baseline names
([`opamp.proto:716`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L716): "the following
attributes SHOULD be included: `os.type`, `os.version`"), our Client already maps Rust's `macos` onto
the convention's `darwin`, and the Collector agrees; only the release file name still says `macos`.
The architecture is not settled at all. The Client passes Rust's constant through where the
convention says `amd64`, and that deviation is not academic: a Managed Process's reported attributes
are folded over the Supervisor's
([`agent.rs:826`](../../crates/client/src/supervisor/agent.rs#L826)), so a Collector whose
`opampextension` reports `host.arch=amd64` **overwrites** the Supervisor's `x86_64`. The same
machine reports a different architecture depending on what happens to run on it, and a Selector
written against one spelling stops matching.

So the question is not only how a package learns its platform. It is which words this project uses
for one, everywhere, at once.

## Decision

We will make a **Platform** — an operating system and an architecture, spelled the way the semantic
conventions spell them — a property of a package artifact, required wherever an artifact is written,
and make the Server offer each Agent **only** the artifact that fits the platform it reports. The
same two tokens name the platform in the release file name, in the upload, in the API, and in what
the Agent reports.

A **Package** is a name plus one or more **Variants**: one artifact per Platform, under one name.

1. **The store is keyed by `(name, os, arch)`.** A variant carries everything that belongs to bytes —
   version, type, content hash, signature, size, source, and its own one-step history (ADR-0019).
   The Selector belongs to the **name**, shared by every variant of it: **the Selector aims, the
   Platform fits.** On disk a variant is `<name>@<os>-<arch>.json` / `.bin` / `.previous.bin`; the
   ADR-0010 name grammar admits neither `@` nor `_`, so the file name parses back unambiguously.

2. **Platform is required wherever an artifact is written or served, and nowhere else.** The rule is
   that a request naming *bytes* names the Platform they are for; a request aiming the *package* does
   not.

   | Route | Platform |
   |---|---|
   | `PUT /api/v1/packages/{name}?version=…&os=…&arch=…` | **required** — `400` without it |
   | `PUT /api/v1/packages/{name}/source` (`os`, `arch` in the body) | **required** |
   | `POST /api/v1/packages/{name}/rollback?os=…&arch=…` | **required** |
   | `GET /api/v1/packages/{name}/file?os=…&arch=…` | **required** |
   | `DELETE /api/v1/packages/{name}[?os=…&arch=…]` | optional — one variant, or the whole package |
   | `PUT /api/v1/packages/{name}/selector` | absent — the Selector is the name's |

   Rollback is per variant rather than per name because a rollback per name would be wrong: an
   operator who canaries `3.1.0` on Linux only and then takes it back would otherwise also push
   macOS back to *its* predecessor, which was never part of the rollout.

3. **Fit before aim, and fit is not optional.** Offer resolution gains a first step that cannot be
   switched off: every variant whose Platform is not the Agent's reported platform is dropped before
   anything else is considered. Only then does ADR-0017's aiming run over what is left — most
   specific Selector wins, a tie is refused and reported as `package_conflict`. Two variants of one
   name never tie, because at most one of them fits.

   The platform is read from exactly **two non-identifying attributes: `os.type` and `host.arch`** —
   the same list a Selector matches against, so fitting and aiming read the same place. The Baseline
   names `os.type` itself, our Client reports both, and the Collector's `opampextension` reports both
   as well. Two neighbours are deliberately *not* read: `os.description` is prose ("Ubuntu 24.04.2
   LTS"), which the fleet view uses for display with a fallback to `os.type` and which nothing can
   compare; and `os.version` describes a release of the system, not which system it is.

   An Agent that reports **no** `os.type` or `host.arch` fits nothing and is offered nothing. Saying
   "unknown platform, so anything goes" would put the mismatched-binary failure back exactly where
   this decision removes it. The reason is reported on the Agent's fleet row, so it reads as a stated
   refusal rather than a rollout that never starts.

4. **The name on the wire is still the name.** `PackagesAvailable` maps the package *name* to the
   fitting variant, so two Agents on different platforms are offered the same name and different
   bytes. `all_packages_hash` is already computed per Agent over its matching set (ADR-0017), which
   is exactly the right granularity for this. **The Client's package handling needs no change**: one
   `[self_update] package = "opamp-fleet-client"` works unmodified on all five release targets, and
   the name check that protects the binary keeps protecting it.

5. **One vocabulary, and it is the semantic conventions'.** The canonical Platform is an `os.type`
   value (`linux`, `darwin`, `windows`, …) and a `host.arch` value (`amd64`, `arm64`, …). Everything
   this project writes or shows uses those two tokens.

   A fixed table canonicalises **both** sides before they are compared — the uploaded Platform and
   the reported attributes alike — so older and foreign spellings keep working: `macos`, `osx` →
   `darwin`; `win`, `win32`, `win64` → `windows`; `x86_64`, `x64`, `x86-64` → `amd64`; `aarch64` →
   `arm64`. Anything else is lower-cased and passed through, so a platform this table has never heard
   of is still serviceable without a code change; a canonical token must match `[a-z0-9_]{1,16}`,
   which is what keeps it a safe file-name component. The canonical pair is what the API answers
   with, so a typo is visible in the response rather than only in a rollout that never happens.

   The table is compatibility, not translation. With points 6 and 7 below, nothing this project
   produces needs it.

6. **The Client reports the convention it already claims to follow.** `host.arch` becomes `amd64` /
   `arm64` instead of Rust's `x86_64` / `aarch64`
   ([`agent.rs:797`](../../crates/client/src/supervisor/agent.rs#L797)), the same one-line mapping
   `os.type` has carried for `macos` → `darwin` all along. The Supervisor and a Collector's
   `opampextension` then report the same string, so folding a Managed Process's attributes over the
   Supervisor's stops being able to change a host's architecture.

7. **The release artifacts carry those same two tokens.** `opamp-fleet-client-<version>-<os>-<arch>.7z`,
   with `<os>` and `<arch>` spelled as in point 5 — replacing the `<os>`/`<arch>` columns of
   ADR-0025's target table and the name in its clause 4. The five targets, their runners, and
   everything else about the release are ADR-0025's and stand:

   | target | runner | `<os>` | `<arch>` | artifact |
   |---|---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `linux` | `amd64` | `…-linux-amd64.7z` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `arm64` | `…-linux-arm64.7z` |
   | `aarch64-apple-darwin` | `macos-latest` | `darwin` | `arm64` | `…-darwin-arm64.7z` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `darwin` | `amd64` | `…-darwin-amd64.7z` |
   | `x86_64-pc-windows-msvc` | `windows-latest` | `windows` | `amd64` | `…-windows-amd64.7z` |

   The file name is then literally the platform pair the host reports, which is what makes the upload
   mechanical: the release notes publish the loop that uploads **all five** under one package name,
   with `os` and `arch` taken straight out of each file name. ADR-0025 called artifact names a public
   contract, and they are — which is why this changes them once, here, together with everything else
   that names a platform, rather than leaving `macos`/`x86_64` as the last two words in the project
   that mean something no Agent ever says.

8. **A stored package without a Platform is refused at startup.** There is no "matches every
   platform" state: the store fails to open, naming the file and saying to re-upload it with `os` and
   `arch` or delete it. A silent "any" would mean the guarantee in point 3 holds for new packages and
   not for old ones, which is not a guarantee an operator can rely on. Configuration errors are fatal
   and named in this project (ADR-0008); this is the same rule applied to state.

## Alternatives considered

- **Platform as pure metadata on separate package names.** Keep the store keyed by name, add `os` and
  `arch` as fields that hard-filter the offer. Much smaller: no store rework, no per-variant history,
  no download-URL change. Rejected because it leaves the Client self-update exactly where it is —
  five artifacts still need five names, and `[self_update] package` still has to name the right one
  per host. It solves the mismatched-binary problem and not the problem that motivated the question.
- **Leave it to the Selector, and only document it.** A Selector of `{"os.type": "linux",
  "host.arch": "amd64"}` already does the filtering. Rejected: it is opt-in, and the cost of
  forgetting it is a bricked agent on every host of every other platform. It also cannot express the
  Client case at all. A rule that is only ever right when remembered is not the rule this needs.
- **Optional Platform, empty meaning "every platform".** Rejected as point 8 states: it is the
  backward-compatible reading of "filtered", and it makes the filter a property of how carefully a
  package was uploaded rather than of the Server.
- **Encode the platform in the version string** (`3.0.0+linux-amd64`). Rejected. It needs no schema
  change at all, which is its only virtue: ADR-0029 just decided that build metadata is provenance
  and is *not* compared, so putting a load-bearing selector there contradicts an accepted ADR, and
  the Client's `self-check` compares the version it is offered against what the binary reports.
- **Canonicalise onto what the Client happens to report today** (`x86_64`, `aarch64`), leaving the
  Client untouched. Considered first, and it is the smaller change: the fleet view and the package
  view would agree, and only the release file name would need translating. Rejected once the file
  names moved: it would make this project's canonical spelling of an architecture one that neither
  the semantic conventions, nor the Collector, nor the release artifact uses — a private vocabulary
  maintained by a table, forever. The convention is already what two of the three worlds speak.
- **Keep the release file names as ADR-0025 wrote them** (`macos`, `x86_64`) and let the alias table
  absorb them at upload. Rejected. It works, and it leaves an operator reading `macos-x86_64` off a
  download page, `darwin`/`amd64` in the fleet view, and having to know these are the same machine.
  The table exists for spellings this project does not control; using it to paper over its own is how
  the divergence became load-bearing in the first place.
- **Reject an unknown `os`/`arch` at upload against a closed list.** Rejected. It catches a typo, but
  the list would have to enumerate every platform any Agent in any fleet might report, and being
  wrong about that means an operator cannot serve a platform the Server has no opinion about. The
  canonical pair in the response and the stated refusal on the Agent's fleet row cover the typo at a
  much lower price.
- **One rollback per name rather than per variant.** Rejected on the canary case described in point 2:
  it reverts platforms that never took the rollout.

## Sources / Prior art

- [OCI Image Index (image-spec)](https://github.com/opencontainers/image-spec/blob/main/image-index.md)
  — the same shape, and the reason to be confident in it: one name resolves to a list of manifests
  each carrying a required `platform` object of `architecture` and `os`, and a client picks the entry
  matching its requirements. It also settles the vocabulary question the same way this ADR does —
  "image indexes SHOULD use, and implementations SHOULD understand, values listed in the Go Language
  document for `GOARCH`/`GOOS`" — i.e. one named canonical set that everything is understood *as*,
  rather than a free-for-all of equivalent spellings.
- The Baseline's own `AgentDescription`
  ([`opamp.proto:690`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L690)) — "keys/values
  are according to OpenTelemetry semantic conventions", then "the following attributes SHOULD be
  included: `os.type`, `os.version`" and "`host.*` to describe the host the Agent runs on". The
  protocol names the two attributes this decision fits against *and* names the conventions as the
  vocabulary, which is the direct authority for points 5 and 6.
- [OpenTelemetry semantic conventions — `host.arch`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/host.md)
  (`amd64`, `arm32`, `arm64`, `ia64`, `ppc32`, `ppc64`, `s390x`, `x86`) and
  [`os.type`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/os.md)
  (`aix`, `darwin`, `dragonflybsd`, `freebsd`, `hpux`, `linux`, `netbsd`, `openbsd`, `solaris`,
  `windows`, `zos`) — the canonical set adopted here, and the evidence for the `x86_64`/`amd64`
  divergence point 6 closes.
- [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — checked as the behavioural oracle for what actually arrives at a Supervisor Endpoint: it reports
  `os.type` from `runtime.GOOS` and `host.arch` from `runtime.GOARCH`, i.e. exactly the canonical set
  above. Since a Managed Process's attributes are folded over the Supervisor's, this is also the
  concrete path by which a host's reported architecture currently changes spelling without the host
  changing.
- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — "the packages that are available on the Server **for this Agent**", and "there is normally only
  one top-level package": the per-Agent offer this builds on, unchanged. The protocol has no notion
  of platform, which is why the selection has to happen before the offer is composed rather than in
  it.
- [ADR-0017](0017-selector-targeted-packages.md) — the Selector semantics and the specificity rule
  this leaves intact and inserts a step in front of; also the source of the "one package per
  platform" sentence this ADR makes true.
- [ADR-0020](0020-client-self-update.md) and [ADR-0025](0025-release-pipeline-and-artifacts.md) — the
  single configured package name and the five published targets whose collision is the concrete case
  driving this.

## Consequences

- Positive: **the Client self-update works across a heterogeneous fleet with no per-host
  configuration** — five artifacts uploaded under one name, each host offered its own. That is the
  case ADR-0020 promised and ADR-0025 made unreachable, and it needs no change to the Client's
  package handling.
- Positive: a mismatched binary is refused by construction rather than caught by a health gate, so
  the worst available operator mistake stops being available.
- Positive: **one spelling of a platform end to end** — release file name, upload, API response,
  package view, fleet row, and what a Collector reports through a Supervisor Endpoint all say
  `linux`/`amd64`. The upload of a full release becomes a loop over the published files with no
  translation step, and the alias table is left holding only foreign spellings.
- Positive: folding a Managed Process's attributes over the Supervisor's can no longer change a
  host's reported architecture, which silently broke Selectors before.
- Positive: per-variant history means a rollback is per platform, which is what a staged rollout
  across platforms actually needs.
- Negative / trade-offs: **this breaks the published v1 package contract in four places** — `PUT`
  requires `os`/`arch`, the download URL and rollback do too, and `PackageView` moves `version`,
  `previous_version` and `source_url` into a `variants` list. Every generated client and every script
  against those routes changes. The project is pre-1.0 and the alternative is a guarantee with a hole
  in it, but this is the largest API break since ADR-0012.
- Negative / trade-offs: **artifact names change**, and ADR-0025 was right that they are a public
  contract — anything scripted against `…-macos-aarch64.7z` breaks, and the previous release's assets
  keep the old names forever. This is the one chance to do it while the number of releases is small.
- Negative / trade-offs: **a Selector written against `host.arch: "x86_64"` stops matching**, because
  the Client now reports `amd64`. Selectors are compared raw — the canonicalisation table is for the
  Platform, not for arbitrary attribute matching — so these must be edited on the Server. A
  `CHANGELOG.md` entry has to name this; it is a silent break otherwise, which is the worst kind.
- Negative / trade-offs: **an existing package store will not open** until every stored package is
  re-uploaded with a Platform. This is deliberate (point 8), but it is an operator action on every
  Server, and the Server stays down until it is done.
- Negative / trade-offs: an Agent reporting no platform now gets no package where it previously could
  have got one. No Client this project ships is in that position — the Supervisor always reports both
  attributes — but a foreign OpAMP client connecting directly to the Server may be, and its rollout
  stops with a message rather than proceeding on a guess.
- Negative / trade-offs: the package view and the bundled UI grow a dimension. "Which version is this
  package at" no longer has one answer, and a fleet running five platforms has five, which is more
  honest and more to read.
- Follow-ups: the Baseline names two more attributes no Agent here reports — `os.version`, and
  `host.*` beyond the architecture. **`host.name` is the pressing one**: ADR-0017 twice offers "a
  Selector matching that host's `host.name`" as the way to pin one host, and no Agent reports it, so
  that Selector never matches; `opampextension` does report it, so a Collector-backed Agent has a
  host name in the same fleet where a Supervisor-backed one has none. Closing that gap is
  implementation within ADR-0012 and ADR-0017 rather than a decision of its own. `cloud.*` is not:
  filling it means probing a metadata service at startup, which is a network call in the start path
  and belongs in its own ADR. A variant matrix in the bundled UI — which platforms a package has,
  against the platforms the fleet actually reports — is a natural next step, as is warning at upload
  time when a Platform fits no Agent currently in the fleet.
