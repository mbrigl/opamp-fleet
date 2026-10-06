# ADR-0021: One platform vocabulary from the release file name to the offer — a package is one name with one artifact per platform

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

## Context

ADR-0016 moved the decision *which artifact an Agent gets* from a file on every host to the Server,
and pointed at the heterogeneous fleet as the case it solves: "`host.arch` and `os.type` are already
reported, so one package per platform, each with a Selector, updates every machine from the Server".
That sentence does not hold up on its own.

**If a package name carries exactly one artifact**, "one package per platform" means one *name* per
platform — `otelcol-linux-amd64`, `otelcol-darwin-arm64`, and so on — and a Selector on each that
repeats, as an equality pair, what the name already says.

**For the Client's own update that shape does not work at all.** `[self_update] package` names
**one** package, and the Client refuses any offer under a different name
([`agent.rs:637`](../../crates/client/src/supervisor/agent.rs#L637)) — deliberately, because that
name is the only thing on the host standing between a fleet-wide Collector artifact and every Client
binary in the fleet (ADR-0017). A release publishes **five** artifacts, one per target. With one
artifact per name an operator has exactly two options, and both are bad: put one platform's build in
the fleet under the Client's package name — and uploading the second platform's build silently
overwrites the first for the whole fleet — or give each platform its own package name and write the
matching name into the configuration **on every host**. The second is the per-host wiring ADR-0016
exists to remove, reintroduced at the one place where the blast radius is the Client itself.

**A Selector alone does not stop a mismatched binary.** An empty Selector reaches every Agent that
accepts packages. An operator who uploads a Windows artifact and forgets the Selector would have it
downloaded, verified, unpacked and swapped over the binary on every Linux host in the fleet. The
Client's health gate catches it — the process will not stay up and is rolled back (ADR-0015) — but
that is a fleet-wide outage window, discovered per host, for a mistake the Server had every attribute
in hand to refuse. The Selector *can* express the platform, but it is opt-in, and the failure mode of
forgetting it is the worst one this system has.

**And a platform has more than one spelling.** Two vocabularies meet here:

| Where | Linux / macOS / Windows | 64-bit Intel | 64-bit ARM |
|---|---|---|---|
| Rust's `std::env::consts` (`OS`, `ARCH`) | `linux` / `macos` / `windows` | `x86_64` | `aarch64` |
| Semantic conventions, and what `opampextension` reports (`runtime.GOOS`/`GOARCH`) | `linux` / `darwin` / `windows` | `amd64` | `arm64` |

The operating system is very nearly settled — `os.type` is what the Baseline names
([`opamp.proto:716`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L716): "the following
attributes SHOULD be included: `os.type`, `os.version`"), our Client maps Rust's `macos` onto the
convention's `darwin`, and the Collector agrees. The architecture is where the two diverge, and the
divergence is not academic: a Managed Process's reported attributes are folded over the Supervisor's
([`agent.rs:826`](../../crates/client/src/supervisor/agent.rs#L826)), so a Collector whose
`opampextension` reports `host.arch=amd64` **overwrites** a Supervisor's Rust-spelled `x86_64`. The
same machine would report a different architecture depending on what happens to run on it, and a
Selector written against one spelling would stop matching.

So the question is not only how a package learns its platform. It is which words this project uses
for one, everywhere, at once.

## Decision

We will make a **Platform** — an operating system and an architecture, spelled the way the semantic
conventions spell them — a property of a package artifact, required wherever an artifact is written,
and make the Server offer each Agent **only** the artifact that fits the platform it reports. The
same two tokens name the platform in the release file name, in the upload, in the API, and in what
the Agent reports.

A **Package** is a name plus one artifact per Platform, under one name.

1. **The Selector aims, the Platform fits.** The Selector belongs to the package, shared by every
   per-platform artifact of it; the Platform belongs to each artifact. How the store holds them — a
   Set's entries keyed by Platform, and its on-disk layout — is ADR-0016 points 9 and 11.

2. **Platform is required wherever an artifact is written or served, and nowhere else.** The rule is
   that a request naming *bytes* names the Platform they are for; a request aiming the *package* does
   not. The routes that carry it are ADR-0016 point 12's: each entry route names `{os}/{arch}`, and
   the Selector's route names none.

3. **Fit before aim, and fit is not optional.** Offer resolution has a first step that cannot be
   switched off: every artifact whose Platform is not the Agent's reported platform is dropped before
   anything else is considered. Only then does ADR-0016's aiming run over what is left (ranked as
   ADR-0016 point 3 ranks, a tie refused and reported as `package_conflict`). Two artifacts of one
   package never tie, because at most one of them fits.

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
   fitting artifact, so two Agents on different platforms are offered the same name and different
   bytes. `all_packages_hash` is computed per Agent over its matching set (ADR-0016), which is exactly
   the right granularity for this. **The Client's package handling needs no change**: one
   `[self_update] package` (`supervisor`, ADR-0022 clause 7) works unmodified on all five release
   targets, and the name check that protects the binary keeps protecting it.

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

6. **The Client reports the convention it already claims to follow.** `host.arch` is `amd64` /
   `arm64`, not Rust's `x86_64` / `aarch64`
   ([`agent.rs:797`](../../crates/client/src/supervisor/agent.rs#L797)), the same one-line mapping
   `os.type` carries for `macos` → `darwin`. The Supervisor and a Collector's `opampextension` then
   report the same string, so folding a Managed Process's attributes over the Supervisor's cannot
   change a host's architecture.

7. **The release artifacts carry those same two tokens.** The file name itself is ADR-0022 clause
   8's (`supervisor_<version>_<os>_<arch>.tar.gz`, its fields separated as ADR-0019 decides); its
   `<os>` and `<arch>` are spelled as in point 5:

   | target | `<os>` | `<arch>` |
   |---|---|---|
   | `x86_64-unknown-linux-gnu` | `linux` | `amd64` |
   | `aarch64-unknown-linux-gnu` | `linux` | `arm64` |
   | `aarch64-apple-darwin` | `darwin` | `arm64` |
   | `x86_64-apple-darwin` | `darwin` | `amd64` |
   | `x86_64-pc-windows-msvc` | `windows` | `amd64` |

   The file name then states literally the platform pair the host reports, which is what makes the
   upload mechanical: the release notes publish the loop that uploads **all five** under one package
   name, with `os` and `arch` taken straight out of each file name.

8. **There is no "matches every platform" state.** An artifact without a Platform is unrepresentable
   in a Set (ADR-0016 point 9), and a stored package the Server cannot read that way fails startup,
   naming the file (ADR-0016 point 14). A silent "any" would mean the guarantee in point 3 holds for
   new packages and not for old ones, which is not a guarantee an operator can rely on. Configuration
   errors are fatal and named in this project (ADR-0008); this is the same rule applied to state.

## Alternatives considered

- **Platform as pure metadata on separate package names.** Keep one artifact per name, add `os` and
  `arch` as fields that hard-filter the offer. Much smaller: no store rework, no download-URL change.
  Rejected because it leaves the Client self-update exactly where it is — five artifacts still need
  five names, and `[self_update] package` still has to name the right one per host. It solves the
  mismatched-binary problem and not the problem that motivated the question.
- **Leave it to the Selector, and only document it.** A Selector of `{"os.type": "linux",
  "host.arch": "amd64"}` already does the filtering. Rejected: it is opt-in, and the cost of
  forgetting it is a bricked agent on every host of every other platform. It also cannot express the
  Client case at all. A rule that is only ever right when remembered is not the rule this needs.
- **Optional Platform, empty meaning "every platform".** Rejected as point 8 states: it is the
  backward-compatible reading of "filtered", and it makes the filter a property of how carefully a
  package was uploaded rather than of the Server.
- **Encode the platform in the version string** (`3.0.0+linux-amd64`). Rejected. It needs no schema
  change at all, which is its only virtue: ADR-0009 decides that build metadata is provenance and is
  *not* compared, so putting a load-bearing selector there contradicts an accepted ADR, and the
  Client's `self-check` compares the version it is offered against what the binary reports.
- **Canonicalise onto Rust's spelling** (`x86_64`, `aarch64`), leaving the Client's report as Rust
  gives it. The smaller change: the fleet view and the package view would agree, and only the release
  file name would need translating. Rejected: it would make this project's canonical spelling of an
  architecture one that neither the semantic conventions, nor the Collector, nor the release artifact
  uses — a private vocabulary maintained by a table, forever. The convention is already what two of
  the three worlds speak.
- **Spell the release file names with Rust's tokens** (`macos`, `x86_64`) and let the alias table
  absorb them at upload. Rejected. It works, and it leaves an operator reading `macos-x86_64` off a
  download page, `darwin`/`amd64` in the fleet view, and having to know these are the same machine.
  The table exists for spellings this project does not control; using it to paper over its own is how
  a divergence becomes load-bearing.
- **Reject an unknown `os`/`arch` at upload against a closed list.** Rejected. It catches a typo, but
  the list would have to enumerate every platform any Agent in any fleet might report, and being
  wrong about that means an operator cannot serve a platform the Server has no opinion about. The
  canonical pair in the response and the stated refusal on the Agent's fleet row cover the typo at a
  much lower price.

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
  concrete path by which a host's reported architecture would change spelling without the host
  changing.
- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — "the packages that are available on the Server **for this Agent**", and "there is normally only
  one top-level package": the per-Agent offer this builds on, unchanged. The protocol has no notion
  of platform, which is why the selection has to happen before the offer is composed rather than in
  it.
- [ADR-0016](0016-a-package-is-a-versioned-set.md) — the Selector semantics and the specificity rule
  this leaves intact and inserts a step in front of; also the source of the "one package per
  platform" sentence this ADR makes true.
- [ADR-0017](0017-client-self-update-and-its-consent.md) and [ADR-0019](0019-release-pipeline-and-artifact-names.md) — the
  single configured package name and the five published targets whose collision is the concrete case
  driving this.

## Consequences

- Positive: **the Client self-update works across a heterogeneous fleet with no per-host
  configuration** — five artifacts uploaded under one name, each host offered its own. That is the
  case ADR-0017 promises, and it needs no change to the Client's package handling.
- Positive: a mismatched binary is refused by construction rather than caught by a health gate, so
  the worst available operator mistake stops being available.
- Positive: **one spelling of a platform end to end** — release file name, upload, API response,
  package view, fleet row, and what a Collector reports through a Supervisor Endpoint all say
  `linux`/`amd64`. The upload of a full release becomes a loop over the published files with no
  translation step, and the alias table is left holding only foreign spellings.
- Positive: folding a Managed Process's attributes over the Supervisor's cannot change a host's
  reported architecture, which would silently break Selectors.
- Negative / trade-offs: **a Selector written against `host.arch: "x86_64"` does not match**, because
  the Client reports `amd64`. Selectors are compared raw — the canonicalisation table is for the
  Platform, not for arbitrary attribute matching — so such Selectors must be edited on the Server.
- Negative / trade-offs: an Agent reporting no platform gets no package. No Client this project
  ships is in that position — the Supervisor always reports both attributes — but a foreign OpAMP
  client connecting directly to the Server may be, and its rollout stops with a message rather than
  proceeding on a guess.
- Follow-ups: `cloud.*` attributes are not reported by any Agent here, and filling them means
  probing a metadata service at startup, which is a network call in the start path and belongs in its
  own ADR. A platform matrix in the bundled UI — which platforms a package has, against the platforms
  the fleet actually reports — is a natural next step, as is warning at upload time when a Platform
  fits no Agent currently in the fleet.
