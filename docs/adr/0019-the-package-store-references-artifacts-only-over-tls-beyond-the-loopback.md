# ADR-0019: The Server stores each release as one package per Agent type and version, with one entry per Platform referenced only over TLS beyond the loopback, and offers an Agent only the artifact built for its type and machine

- **Status:** 🟢 accepted
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-server/src/packages.rs`, the package routes and the download route of `crates/fleet-server/src/api.rs`, `packages_dir` and the package limits in `crates/fleet-server/src/config.rs`, the platform table in `crates/opamp/src/attributes.rs`, the `host.arch` the Client reports, the Packages tab of the bundled UI

## Context

The [specification](../SPECIFICATION.md) puts security before convenience (Strategy "Security
before convenience", Q-1 "Secure by default"): no package and no credential leaves a host
unencrypted beyond the loopback.

Goal 10 asks the Server to update an Agent's binary — the Collector's and the Client's own —
verifying each Package before it is applied. The Agent side of that (download, verify, unpack,
swap, health gate, rollback) is [ADR-0018](0018-signed-package-delivery-from-allowed-sources.md). This record is
the Server side: what the Server holds, how an artifact gets there, and which of the held artifacts
can be meant for a given Agent at all.

Forces:

- **A release is one version across several artifacts.** The release pipeline
  ([ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)) publishes five platform builds
  of one version, and upstream projects publish more. The store needs an object that *is* that
  release, or "which version is this at" has one answer per platform and a half-finished upload
  looks like a rollout in progress.
- **A mismatched binary is the worst mistake available.** A Windows artifact swapped over a Linux
  host's program, or a Promtail build swapped over a Collector, is caught only by the health gate —
  a fleet-wide outage window for a mistake the Server had every attribute in hand to refuse. A
  filter that holds only when an operator remembers to write it is not a guarantee.
- **Three spellings of one platform meet here.** Rust says `x86_64`/`aarch64`/`macos`; the semantic
  conventions — which the Baseline names as the vocabulary of `AgentDescription` — and the
  Collector's `opampextension` say `amd64`/`arm64`/`darwin`. A Managed Process's attributes are
  folded over the Supervisor's, so with two spellings a host would report a different architecture
  depending on what runs on it, and a filter written against one spelling would stop matching.
- **Upstream publishes archives, at an address, with a checksum.** `opentelemetry-collector-releases`
  publishes `.tar.gz`, `.deb`, `.rpm`, `.msi` — never a bare binary — and a `checksums.txt` with a
  SHA-256 per asset. Downloading 400 MiB to upload it again moves the same bytes twice. The
  Baseline's Download Server *"may be on the same host as the OpAMP Server or a different host"*,
  and `DownloadableFile.headers` exists so an Agent can authenticate to one.
- **The download is not secret.** Agents fetch artifacts without an operator credential, so what
  protects an installed binary is verification — the content hash, and a signature where the Agent
  holds a key — not transport secrecy. An artifact whose confidentiality matters must therefore be
  unreadable to the Server too. What travels with a download is another matter: a referenced
  entry's headers may carry a bearer token, and a plaintext request hands it to every network
  between the Agent and the source.
- **The Agent type is a fact the Server can read.** Every Agent this Client presents reports its
  type as `service.name` ([ADR-0012](0012-what-an-agent-reports-about-itself.md)), which the
  Baseline defines as "a reverse FQDN that uniquely identifies the Agent type".

## Decision

We will hold software in a store of packages — each one release of one Agent type at one version,
holding one uploaded or referenced artifact per Platform — never open, pack or fetch an artifact
on the Server, spell a Platform the semantic conventions' way everywhere, and make type and
platform fit a mandatory precondition of every offer.

1. **The store lives under `packages_dir` and is the Server's only source of software.**
   `packages_dir` (default `fleet-packages`) holds every package; it is managed through the REST
   API and restored at startup. `OffersPackages` and `AcceptsPackagesStatus` are declared only while
   the store holds a package — an undeclared capability is never exercised. The directory is
   created owner-only (`0700`) and every metadata file is written `0600` through a temporary file
   and a rename, because a referenced entry's headers may carry a bearer token (clause 5).

2. **A package is one release: one Agent type, one version, fixed at creation.** Both are stated
   when the package is created and never edited; a new version is a **new package**, never a change
   to an old one. There is no untyped package and no moment in which one exists. The store keeps
   every version openly as its own package until an operator deletes it — there is no hidden
   "previous" artifact and no automatic pruning, and the store is not where rollback happens
   ([ADR-0014](0014-rollout-and-what-reaches-an-agent.md) clause 18). The type is compared
   **raw** (clause 9); the version orders as [ADR-0017](0017-versions-resolved-in-the-internal-crate.md) clause 9 orders versions.

3. **A package holds one entry per Platform, so a duplicate is unrepresentable.** Entries are a map
   keyed by the canonical `(os, arch)` pair. Writing an entry for a Platform the package already
   holds replaces it; deleting the last one leaves an empty package, kept, because a package being
   assembled is a normal state and deleting the package is its own act. One entry suffices; five
   platforms are five entries under one identity. When an entry may be written at all is
   [ADR-0014](0014-rollout-and-what-reaches-an-agent.md) clause 8's rule.

4. **An entry is an uploaded artifact or a reference to one.**
   - **Uploaded:** the body of an entry upload is the artifact. The Server streams it to a staging
     file in the package's own directory (named per Platform, so parallel uploads of one release do
     not collide), hashes it while streaming, refuses an empty one, and renames it into place as
     `<os>-<arch>.bin`. One artifact may be at most `max_package_size_bytes` (default 1 GiB, `413`
     beyond), the whole store at most `max_total_package_bytes` (default 16 GiB, `507` when the
     upload would pass it — checked before streaming and again after); `0` for either is refused at
     load.
   - **Referenced:** the entry is a `url`, a **mandatory** `sha256` (64 hex characters, as a
     release's `checksums.txt` publishes it) and optional `headers`. The `url` is `https://`;
     `http://` is accepted only when its host is a loopback IP literal, `127.0.0.1` or `[::1]` — a
     host name never counts as loopback, `localhost` included. The Server refuses any other `url`
     when the entry is set through the REST API (`400`, naming the rule), so neither the artifact
     nor the headers an Agent sends for it cross a network in plaintext. The Server stores only
     that reference and offers it verbatim as the `DownloadableFile`; it never
     downloads the artifact and has nothing to serve. A referenced entry replacing an uploaded one
     deletes the bytes it replaces. The headers are stored in cleartext (owner-only) and travel to
     every Agent offered the entry.

   The checksum is supplied, never derived: for a reference nothing central ever sees the bytes, so
   the operator's `sha256` is the one thing standing between a URL and a host, and every Agent
   checks it.

5. **Setting a source probes it once, as a typo catch and nothing more.** The Server sends one
   `HEAD` over TLS 1.3 with the given headers, a 10-second timeout and no redirect following, so
   it never follows a source onto a plaintext `url`. A `4xx` answer
   refuses the write (`400`, with a hint about headers on `401`/`403`); a source the Server cannot
   reach is stored anyway, because the Server is not in the download path. The probe refuses to aim
   at a non-routable address — link-local (the cloud metadata address among it), the shared/CGNAT
   range, unspecified, broadcast, documentation and `0.0.0.0/8` — and deliberately not at loopback
   or the RFC 1918 / unique-local ranges, where an operator's mirror legitimately lives, over
   `https://` as clause 4 requires. The probe
   proves nothing about what an Agent will later receive; only the `sha256` does.

6. **The Server never creates, packs, repacks, encrypts, decrypts or opens an artifact.** What it
   is given is finished — built, packed and, if it is to be confidential, encrypted by whoever
   produced it. Package bodies are opaque bytes: the Server stores or refers, hashes, offers and
   serves. Unpacking, and the archive key that opens an encrypted `.7z`, belong to the Agent
   ([ADR-0018](0018-signed-package-delivery-from-allowed-sources.md)), so the hash an Agent verifies is the one
   the artifact was published with, and a confidential artifact is unreadable on the Server's disk.

7. **A Platform is spelled the semantic conventions' way, end to end.** The canonical Platform is an
   `os.type` value (`linux`, `darwin`, `windows`, …) and a `host.arch` value (`amd64`, `arm64`, …).
   One table in the shared crate canonicalises **both** sides before they are compared — what is
   uploaded and what an Agent reports: `macos`, `osx` → `darwin`; `win`, `win32`, `win64` →
   `windows`; `x86_64`, `x64`, `x86-64` → `amd64`; `aarch64` → `arm64`. Anything else is
   lower-cased and passed through, so a platform the table has never heard of is still served; a
   canonical token must match `[a-z0-9_]{1,16}`, which keeps it a safe file-name component. The API
   answers with the canonical pair, so a typo shows in the response. The table is compatibility,
   not translation — nothing this project produces needs it:
   - the Client reports `host.arch` as `amd64`/`arm64` and `os.type` as `darwin`, mapping Rust's
     constants, so a Supervisor and a Collector's `opampextension` report the same string;
   - the release artifacts carry the same two tokens in their file names (the file-name format is
     [ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)'s), so the upload of a release
     is a loop over its files with `os` and `arch` read straight out of each name.

8. **A request that names bytes names their Platform; one that names the package does not.** The
   entry upload, the entry source, the entry delete and the artifact download all carry `os` and
   `arch`; creating, reading or deleting a package does not. An invalid token is `400`.

9. **Fit is mandatory, comes before everything else, and has no "unknown, so anything goes".** A
   package can be meant for an Agent only when
   - its **Agent type equals** the `service.name` the Agent reports among its identifying
     attributes — compared raw, with no canonicalisation table and no prefix or pattern, because
     there is no authority to canonicalise Agent types against and `otelcol` and `otelcol-contrib`
     are different binaries; and
   - it holds an entry for the **Platform** the Agent reports — read from `os.type` and `host.arch`,
     non-identifying attributes first and an identifying copy as fallback, canonicalised as in
     clause 7. `os.description` (prose) and `os.version` (a release, not a system) are not read.

   An Agent that reports no type, or no `os.type` or `host.arch`, fits nothing. A per-Agent rollout
   act refused on fit says which of the three it was. A label can never supply or override these
   attributes ([ADR-0013](0013-the-fleet-record.md)), so a slip there cannot hand a host an artifact
   built for another machine.

10. **The Platform does not travel on the wire.** All Agents of one type are offered the package
    under the same key, each with the download URL and content hash of the entry that fits it — two
    platforms' artifacts differ in content hash by construction — so the Client's package handling
    is identical on every target and one `[self_update]` consent works across all five release
    platforms.

11. **The Server decides which artifact an Agent receives, never a file on the host.** A
    `[[supervisor]]` block names no package; one carrying `package` fails at startup with a message
    saying the Server decides. Type fit is also the second, independent guard on the Client's own
    binary: the Server does not offer a package of another type to the Client's Agent, whatever it
    is called, and the Client still refuses an offer outside its `[self_update]` consent
    ([ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)) — one side refuses to send, the other to
    install.

12. **The store opens loudly or not at all.** At startup every package's metadata is read and every
    uploaded artifact is re-hashed by streaming. Metadata that cannot be read or parsed, an invalid
    type, version or Platform, a content hash that is not hex, a directory whose name disagrees with
    the identity its metadata states, or an artifact that no longer matches its recorded hash fails
    startup naming the file — a corrupt distribution artifact must never ship, and a store that
    silently drops a package looks like one that was never given it.

13. **Uploaded artifacts are served on the Agent plane.** The download route is the one `/api/v1`
    route on the Agent-plane listener ([ADR-0023](0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)),
    not on the operator plane and not in the OpenAPI document; it streams the file and never holds
    it in memory. The offered `download_url` is a path the Client resolves against its own OpAMP
    endpoint, or `advertised_url` plus that path where downloads must go through another host.

14. **The Packages tab is one table and one detail form.** One row per package — Agent type,
    version, the platforms it holds, and where it is used. Selecting a row makes it the current
    record and shows the form; while nothing is current the form is not on screen. **Create** opens
    the form empty — the only moment the identity fields are writable. The form adds and removes
    entries in place (a file, or a source URL with its `sha256`); a file named as a release artifact
    fills the platform and, while creating, the identity. **OK** persists, **Cancel** discards, and
    both conclude the form — only a failed save keeps it open beside its message; **Delete** stands
    alone on the left. The form enforces no rule of its own: what it greys out is what the Server
    answers `409` to.

**Out of scope:** where a package's name on the wire, its aim and the signature it travels with come
from, and the store's on-disk layout and routes, which [ADR-0021](0021-packages-and-deployments-that-sign-every-package.md)
proposes; which package reaches which Agent and when
([ADR-0014](0014-rollout-and-what-reaches-an-agent.md)); everything on the Agent
([ADR-0018](0018-signed-package-delivery-from-allowed-sources.md)); verifying upstream sigstore/cosign
signatures; a retention policy for superseded packages.

## Alternatives considered

- **Leave the platform or the type to the operator's aim.** `os.type`/`host.arch`/`service.name`
  pairs already filter at zero cost. Rejected: opt-in, and forgetting it installs a foreign binary
  on every host of every other platform or type. A rule that is only right when remembered is not
  the rule this needs.
- **An optional Platform meaning "every platform".** The backward-compatible reading of a filter;
  rejected because it makes the guarantee a property of how carefully something was uploaded rather
  than of the Server.
- **Platform as metadata on separate package names** (`otelcol-linux-amd64`). Smaller, but five
  artifacts then need five names and every host a matching name in its own configuration — the
  per-host wiring this store exists to remove, at the one place where the blast radius is the
  Client itself.
- **A version per platform entry.** Allows a per-platform rollout of different versions under one
  object, and is indistinguishable from a half-finished upload. Two packages express that case
  explicitly.
- **Encode the platform in the version** (`3.0.0+linux-amd64`). Needs no schema, and contradicts
  [ADR-0017](0017-versions-resolved-in-the-internal-crate.md): build metadata is provenance and is not compared.
- **Canonicalise onto what Rust reports** (`x86_64`, `aarch64`). The smaller change; rejected
  because it makes this project's canonical spelling one that neither the conventions, nor the
  Collector, nor the release artifact uses — a private vocabulary maintained by a table, forever.
- **Reject unknown `os`/`arch` tokens against a closed list.** Catches a typo, and refuses to serve
  any platform the list forgot. The canonical pair in the response covers the typo far cheaper.
- **Canonicalise or pattern-match Agent types.** There is no authority to canonicalise against; a
  table would encode this project's opinion of every distribution, and a pattern is a matching
  language that outlives its convenience.
- **Default the type to a name**, so no extra input is needed. Right most of the time and silently
  wrong for every name chosen for a rollout rather than a target.
- **Remember one previous version per package and roll back by swapping.** Bounded disk, one button
  — but hidden, one step deep, and a second history beside the store. Keeping every version as its
  own package makes the history the store itself.
- **Import a URL: have the Server download it and serve the bytes.** Better in a fleet whose hosts
  cannot reach the internet — and that is exactly what uploading is. As the behaviour of a URL it
  makes the Server hold and expose artifacts it has no need to hold.
- **Derive the checksum by fetching once, or verify the reference by downloading at set time.**
  Records what the Server happened to receive, from where it stands — trust on first use with no
  anchor, and for a reference not even what the Agents get.
- **Unpack on the Server and store a bare binary.** One extraction instead of three hundred; but
  every Agent would then verify a hash the Server invented, and an encrypted artifact would have to
  be decrypted on the very machine the encryption keeps it from.
- **Plaintext referenced urls to LAN mirrors.** An HTTP file share on the operator's network is
  the cheapest mirror, and the `sha256` (and a signature where the Agent holds a key) already
  protects what an Agent installs. Rejected: a hash or a signature protects integrity, not
  confidentiality — the artifact and, worse, the download credential in `headers` would cross the
  network readable to anyone on it, and the specification accepts plaintext on the loopback alone.
- **An opaque identifier instead of readable tokens.** Robust against any character, meaningless in
  a URL, a file name and the UI; a bounded token grammar buys the same safety.

## Sources / Prior art

- [RFC 8446 — TLS 1.3](https://www.rfc-editor.org/rfc/rfc8446) — the protocol version the probe
  speaks and a referenced `url` is fetched over beyond the loopback.
- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `PackagesAvailable` is "the packages that are available on the Server **for this Agent**"; the
  Download Server "may be on the same host as the OpAMP Server or a different host"; one
  downloadable file per package, multiple files to be carried "in any file format that allows
  storing multiple files in a single file"; and what a package contains "is Agent type-specific and
  is outside the concerns of the OpAMP protocol".
- The Baseline's `AgentDescription` — "keys/values are according to OpenTelemetry semantic
  conventions", `os.type` SHOULD be included, `service.name` "uniquely identifies the Agent type".
- [OpenTelemetry semantic conventions — `host.arch`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/host.md)
  and [`os.type`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/os.md)
  — the canonical token sets adopted in clause 7.
- [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — reports `os.type` from `runtime.GOOS` and `host.arch` from `runtime.GOARCH`.
- [OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md) — one
  reference resolves to a list of manifests, each with a required `platform` object, values from
  Go's `GOOS`/`GOARCH`; pushed digests are immutable.
- [Debian repository format](https://wiki.debian.org/DebianRepository/Format) and
  [RPM repositories](https://rpm-software-management.github.io/) — a released package is
  `(name, version, architecture)` with per-file checksums.
- [`opentelemetry-collector-releases` v0.157.0](https://github.com/open-telemetry/opentelemetry-collector-releases/releases/tag/v0.157.0)
  — archives only, a `checksums.txt` with a SHA-256 per asset, sigstore keyless signatures.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the Agent type as a first-class object reported by the collector, not a free-form tag.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — its example Server offers no packages,
  so there is no upstream behaviour to copy for where artifacts come from or whom they reach.

## Consequences

- Positive: a release is one object — five platforms' artifacts under one identity with one
  version. One upload loop per release, no per-host configuration, and the Client's self-update
  works across a heterogeneous fleet.
- Positive: a mismatched binary is refused by construction rather than caught by a health gate; the
  Client's own binary is protected on both sides of the wire.
- Positive: one spelling of a platform from the release file name through the upload, the API, the
  fleet view and what a Collector reports; folding a Managed Process's attributes over the
  Supervisor's cannot change a host's architecture.
- Positive: a package can be a URL an operator already has — nothing uploaded, nothing duplicated —
  and for a referenced entry nothing exists on the Server to leak.
- Positive: a referenced entry's headers and artifact never cross a network in plaintext; the only
  plaintext source is one on the host itself.
- Negative / trade-offs: a mirror on the operator's network needs a certificate its Agents trust;
  a plain HTTP file share is no longer a source.
- Negative / trade-offs: disk is unbounded by design — every version stays until deleted.
- Negative / trade-offs: a mistyped Agent type is a silent no-op — there is no canonicalisation to
  catch it, and the package simply fits nobody. An Agent's type can change under a stable
  configuration (a Collector gaining `opampextension` switches to `dist.name`), and with it which
  packages fit it.
- Negative / trade-offs: an Agent reporting no platform or no type gets no package. No Client this
  project ships is in that position; a foreign OpAMP client may be.
- Negative / trade-offs: a referenced entry needs egress from every Agent to its source, three
  hundred downloads from a third party, and the Server cannot answer what exactly the Agents will
  install — a wrong hash, a moved release or a revoked token surfaces as `InstallFailed` on Agents.
  Headers for a private source are a fleet-wide secret in flight and at rest on every host.
- Follow-ups: a retention policy for superseded packages; warning when a type or Platform fits no
  Agent in the fleet; verifying upstream sigstore signatures; `host.name` and `cloud.*` reporting.

## Enforcement

- `crates/fleet-server/src/packages.rs` unit tests: `a_set_survives_a_reopen`,
  `fit_is_mandatory_platform_and_type`, `both_sides_of_the_comparison_are_canonicalised`,
  `the_offer_carries_a_download_url_naming_the_identity`, `deletion_frees_entries_and_sets`,
  `a_corrupt_artifact_fails_reopen`, `the_store_and_its_metadata_are_owner_only`,
  `identity_tokens_are_bounded`.
- `crates/fleet-core/src/platform.rs`: `folds_the_spellings_this_project_does_not_control`,
  `what_rust_calls_this_machine_folds_onto_a_canonical_token`,
  `an_unknown_token_passes_through_unchanged`.
- `crates/fleet-server/tests/packages.rs`: `an_uploaded_set_is_offered_downloaded_and_gated`,
  `the_artifact_is_served_where_the_agents_are_and_not_on_the_operator_plane`,
  `no_offer_without_the_capability`, `an_entry_needs_its_set_first`,
  `an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact`,
  `an_artifact_past_the_configured_limit_is_refused`, `the_package_store_has_a_total_size_ceiling`,
  `a_referenced_entry_is_offered_from_its_source_and_not_from_here`,
  `a_source_that_refuses_the_probe_is_rejected_but_an_unreachable_one_is_not`,
  `a_source_url_aimed_at_an_internal_address_is_refused`, `a_set_reaches_only_agents_of_its_type`.
- `crates/fleet-server/tests/packages.rs`, clauses 4 and 5:
  `a_plaintext_source_off_the_loopback_is_refused` (a host name, `localhost` included, is refused;
  both loopback literals are accepted) and `the_probe_follows_no_redirect`.
- `crates/fleet-server/src/labels.rs`: `a_label_never_overrides_what_the_agent_reports`.
- `crates/fleet-agent/src/config.rs`: `the_retired_package_keys_are_refused`.
- `crates/fleet-agent/tests/packages_e2e.rs` and `crates/fleet-agent/tests/self_update_e2e.rs` run the real
  Server and Client against one store across the reported Platform.

**Not mechanically decidable:** that the Server never opens an artifact (clause 6) is an absence —
no test can show a code path that does not exist; review of every change touching
`crates/fleet-server/src/packages.rs` and `crates/fleet-server/src/api.rs` keeps it. That the probe
speaks TLS 1.3 alone (clause 5) is a client setting no test can observe: a refused handshake counts
as an unreachable source, which is stored like an answering one, so review keeps it.
