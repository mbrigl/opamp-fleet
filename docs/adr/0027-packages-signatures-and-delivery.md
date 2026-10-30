# ADR-0027: Packages, signatures, and delivery

- **Status:** 🟢 accepted
- **Date:** 2026-10-17
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-server/src/packages.rs`, `crates/fleet-server/src/fs/packages.rs`, `crates/fleet-server/src/deployments.rs`, the package, signature, Deployment-package and download routes of `crates/fleet-server/src/api.rs` with the handler `download_package`, the package limits and the `[packages]` section of `server.toml` in `crates/fleet-server/src/config.rs`, the package assignment in `crates/fleet-server/src/fleet.rs` and `crates/fleet-server/src/agent_store.rs`, the `download.refused` audit entry, the platform table in `crates/fleet-core/src/platform.rs` and `crates/opamp/src/attributes.rs`, the `host.arch` the Client reports, the Packages and Deployments tabs of the bundled UI in `crates/fleet-server/static/index.html`, the signing file `opamp-fleet-signing-v1`, the `package sign` command of `opamp-fleetctl` in `crates/fleet-tools/src/package.rs` and `crates/fleet-tools/src/signing.rs`, the Gateway's package cache in `crates/fleet-agent/src/gateway/` and its directory under the Client's `state_dir`, `crates/fleet-agent/src/packages.rs`, `crates/fleet-agent/src/archive.rs`, `crates/fleet-agent/src/install.rs`, `crates/fleet-agent/src/supervisor/process.rs`, the package handling in `crates/fleet-agent/src/supervisor/agent.rs`, the `[packages]` and `[updates]` sections, the `[gateway] package_cache_bytes` key and the `program_path` and `retain_previous_secs` keys of `supervisor.toml` in `crates/fleet-agent/src/config.rs`, `crates/fleet-core/src/package.rs`, and every document that tells an operator how to add, sign or deliver an artifact

## Context

The [specification](../SPECIFICATION.md) puts security before convenience (Strategy *Security
before convenience*, **Q-1** *Secure by default*): software is installed only when it is signed
with a key the operator holds and fetched from the fleet itself, the Server or a Gateway relaying
it, and no Agent installs a package without a valid signature. **G-10** asks the Server to update
an Agent's software, the Collector's and the Client's own: the Agent verifies each Package before
it applies it, reports the outcome, and rolls back on failure. **Q-4** bounds what a host
receives: the Server, and a Gateway from its package cache, never serve a host an artifact that
is not released to an Agent that host speaks for.

The Baseline's mechanism is a hash-gated sync. The Agent reports
`PackageStatuses.server_provided_all_packages_hash`; on a mismatch the Server sends
`PackagesAvailable`, per package a type, a version, a per-package `hash` and a
`DownloadableFile{download_url, content_hash, signature, headers}`, and the Agent downloads,
verifies, installs and reports each package through `Downloading → Installing → Installed |
InstallFailed`. This decision covers the path from what the Server holds, through how an operator
signs it and who may fetch it, to the Agent that downloads, verifies, unpacks, swaps, health-gates
and rolls it back. When content reaches an Agent, and the version test it must pass, are
[ADR-0026](0026-deployments-and-rollouts.md)'s; how a Package version is written and the floors
below which nothing is taken are [ADR-0028](0028-package-versions-high-water-mark-and-floor.md)'s.

The store, and which held artifact can be meant for an Agent:

- **A release is one version across several artifacts.** The release pipeline
  ([ADR-0034](0034-releases-attestations-and-the-server-image.md)) publishes five platform builds
  of one version, and upstream projects publish more. The store needs an object that *is* that
  release, or "which version is this at" has one answer per platform and a half-finished upload
  looks like a rollout in progress.
- **A mismatched binary is the worst mistake available.** A Windows artifact swapped over a Linux
  host's program, or a Promtail build swapped over a Collector, would be caught only by the health
  gate, a fleet-wide outage window for a mistake the Server had every attribute in hand to refuse.
  A filter that holds only when an operator remembers to write it is not a guarantee.
- **Three spellings of one platform meet here.** Rust says `x86_64`/`aarch64`/`macos`; the
  semantic conventions, which the Baseline names as the vocabulary of `AgentDescription`, and the
  Collector's `opampextension` say `amd64`/`arm64`/`darwin`. With two spellings a host would report
  a different architecture depending on what runs on it, and a filter written against one spelling
  would stop matching.
- **Upstream publishes archives with a checksum.** `opentelemetry-collector-releases` publishes
  `.tar.gz`, `.deb`, `.rpm` and `.msi`, never a bare binary, with a `checksums.txt` of SHA-256
  values. Some agents publish their portable Windows build as a `.zip`.
- **The Agent type is a fact the Server can read.** Every Agent this Client presents reports its
  type as `service.name` ([ADR-0016](0016-what-an-agent-reports-about-itself.md)), which the
  Baseline defines as "a reverse FQDN that uniquely identifies the Agent type".

What identifies a Package and how it is released:

- **A name beside the Agent type is a second identity for one thing.** The type decides fit; a
  free-form name would decide nothing but would group, and the only thing it could add, two
  artifacts of one type under different names, is an ambiguity resolution would have to rank its
  way out of.
- **The Baseline's addon kind has no Client behind it.** The Client refuses every addon offer, as
  a defence against a foreign Server writing one over a Managed Process's binary.
- **A Selector cannot say "not".** Every Selector is equality over the effective description
  ([ADR-0026](0026-deployments-and-rollouts.md)); "everyone except the canary hosts" is not
  writable. Disjoint targets come from membership, which is what Server-set Labels
  ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)) and provisioned attributes are.
- **What a signature covers decides where it belongs.** The signed statement names the Agent type,
  the version and the artifact's SHA-256, and no Deployment. A signature kept beside the release
  would be an arrangement of the Server's data no Client can verify, signed again for every
  Deployment and able to outlive the bytes it was made for.
- **Signing is done where the key is, not where the bytes are.** Everything a signature covers is
  known to the Server: the Package's Agent type and version and every entry's `content_hash`. The
  key is best kept off the Server host, and the machine that holds it is the one least likely to
  hold hundreds of megabytes of artifacts. Signing from the hashes alone signs what the Server
  claims, which is weighed against Q-1 under Consequences.

The download route and the Gateway:

- **The host is the one bound the Server has.** A certificate proves fleet membership and the host
  it was issued to, not an Agent; what a certificate speaks for is
  [ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)'s. Any test of "may fetch" can be no
  finer than the host.
- **What reaches an Agent is already decided, per Agent.** Its offer is composed from its
  assignment alone: the assigned Package's entry for the Platform it reports, with that entry's
  signature. The download needs no rule of its own. It needs the same test the offer uses.
- **Hiding is cheap only if it is complete.** An answer that differs between "exists, not yours"
  and "does not exist" lets a host list the store. HTTP allows a server to hide a forbidden
  resource behind `404` (RFC 9110 section 15.5.4).
- **A partition is only as strong as the hosts it partitions.** A Selector matches the effective
  description, which is what the Agent reports plus the Server's Labels, and where the two collide
  what the Agent reports wins. A compromised host can report another partition's key and value
  under a fresh `instance_uid`, be assigned by that partition's next bulk rollout, and then fetch
  what was released to it. What this decision bounds is fetching what was released to Agents the
  host does not speak for.
- **Behind a Gateway, one artifact would cross one link once per Agent.** A Gateway stands at a
  network boundary in front of a site or a segment (G-15). Relaying each request costs one token
  of the Gateway's aggregate bucket and sends the same bytes n times over the link the Gateway was
  placed to spare. The Gateway already sees every offer it carries, with its `download_url` and
  `content_hash`, and knows which downstream connection carries the Agent. A cache gives the
  Gateway no artifact it could not fetch already; what it must not do is widen what a downstream
  host receives.
- **Verification stays with the installing Client.** The Client checks the content hash and the
  signature before it installs, whatever host served the bytes. A cache changes where the bytes
  come from, not what is installed.

On the Agent:

- **A signature over the bytes alone says nothing about what the artifact is for.** Any artifact
  signed with the operator's key would install as any Supervisor's program at any version label;
  the signature covers the Agent type and the version as well ([`HARDENING.md`](../HARDENING.md)
  H23).
- **A running process cannot reliably replace its own binary,** so the work crosses a process
  boundary. For a Managed Process that boundary exists: the Supervisor owns its process's stop,
  spawn and apply-grace health gate ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)). The
  Client's own binary is the other case
  ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)).
- **Verification protects the host; the origin bounds what is exposed.** What stands between the
  bytes and an executed program is the content hash and an Ed25519 signature made with a key the
  operator holds (the Baseline's *Code Signing* leaves the method to the Agent). The `ring`
  provider the build carries ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)) verifies
  Ed25519. The transport still decides what a download exposes: the Client's certificate is its
  fleet identity, and an offered header is a credential ([`HARDENING.md`](../HARDENING.md) H12,
  H13, H19).
- **Many agents are more than one file.** Fluent Bit ships an executable plus the shared objects
  and plugins it loads. A package that unpacks into a tree gives the archive a say in where bytes
  land, a security boundary this decision creates. The protocol carries one file per package and
  leaves multi-file packages to "any file format that allows storing multiple files in a single
  file".
- **The rollback lifecycle must end.** Discarding a first install that will not start empties the
  program directory and the Server re-offers it: a download loop. A predecessor that also fails to
  start must not be respawned forever, and "survived the grace" is a first signal, not a final
  one, so the predecessor is worth keeping for a while.

## Decision

We will hold software as Packages, each one release of one Agent type at one version with one
uploaded, signed entry per Platform, offer and serve an entry only to a host that speaks for an
Agent it is released to, have a Gateway cache what it relays under the same bound, and have each
Supervisor install only a package it verified against the operator's key, opened on the host,
swapped in by rename, health-gated and rolled back only to a real predecessor.

### The package store

1. **A Package is one release: one Agent type, one version, fixed at creation.** How a Package is
   identified and which versions it may carry is
   [ADR-0028](0028-package-versions-high-water-mark-and-floor.md) clause 1. Both are stated when
   the Package is created and never edited; a new version is a **new Package**, never a change to
   an old one. There is no untyped Package and no moment in which one exists. The store keeps
   every version openly as its own Package until an operator deletes it: there is no hidden
   "previous" artifact and no automatic pruning, and the store is not where rollback happens
   ([ADR-0026](0026-deployments-and-rollouts.md) clause 21: rollback is not a rollout). The type
   is compared **raw** (clause 14); the version orders as
   [ADR-0033](0033-the-products-version.md) orders versions.

2. **A Package holds one entry per Platform, so a duplicate is unrepresentable.** Entries are a map
   keyed by the canonical `(os, arch)` pair. Writing an entry for a Platform the Package already
   holds replaces it; deleting the last one leaves an empty Package, kept, because a Package being
   assembled is a normal state and deleting the Package is its own act. One entry suffices; five
   platforms are five entries under one identity. When an entry may be written at all is
   [ADR-0026](0026-deployments-and-rollouts.md)'s rule on writes to an assigned Package.

3. **An entry is an uploaded artifact, and nothing else.** The body of an entry upload is the
   artifact. The Server streams it to a staging file, hashes it while streaming, refuses an empty
   one, and moves it into place. One artifact may be at most `max_package_size_bytes` (default
   1 GiB, `413` beyond). The whole store may be at most `max_total_package_bytes` (default 16 GiB,
   `507` when the upload would pass it, checked before streaming and again after). `0` for either
   is refused at load. Where the bytes are kept is
   [ADR-0029](0029-the-servers-state-and-its-stores.md)'s. There is no referenced entry: no `url`,
   no `sha256` given instead of bytes, and no request headers.

4. **The Server sends no request for a package anywhere.** It probes no source, follows no URL and
   holds no credential for one. What it offers is what it holds.

5. **The Server never creates, packs, repacks, encrypts, decrypts or opens an artifact.** What it
   is given is finished: built, packed and, if it is to be confidential, encrypted by whoever
   produced it. Package bodies are opaque bytes: the Server stores, hashes, offers and serves.
   Unpacking, and the archive key that opens an encrypted `.7z`, belong to the Agent (clauses 53
   and 54), so the hash an Agent verifies is the one the artifact was published with, and a
   confidential artifact is unreadable on the Server's disk.

6. **A Platform is spelled the semantic conventions' way, end to end.** The canonical Platform is
   an `os.type` value (`linux`, `darwin`, `windows`, …) and a `host.arch` value (`amd64`, `arm64`,
   …). One table in the shared crate canonicalises **both** sides before they are compared, what
   is uploaded and what an Agent reports: `macos`, `osx` → `darwin`; `win`, `win32`, `win64` →
   `windows`; `x86_64`, `x64`, `x86-64` → `amd64`; `aarch64` → `arm64`. Anything else is
   lower-cased and passed through, so a platform the table has never heard of is still served; a
   canonical token must match `[a-z0-9_]{1,16}`, which keeps it a safe file-name component. The
   API answers with the canonical pair, so a typo shows in the response. The table is
   compatibility, not translation; nothing this project produces needs it:
   - the Client reports `host.arch` as `amd64`/`arm64` and `os.type` as `darwin`, mapping Rust's
     constants, so a Supervisor and a Collector's `opampextension` report the same string;
   - the release artifacts carry the same two tokens in their file names (the file-name format is
     [ADR-0034](0034-releases-attestations-and-the-server-image.md)'s), so the upload of a release
     is a loop over the Client's archives with `os` and `arch` read straight out of each name.

7. **A request that names bytes names their Platform; one that names the Package does not.** The
   entry upload, the entry delete and the artifact download all carry `os` and `arch`; creating,
   reading or deleting a Package does not. An invalid token is `400`.

8. **The store opens loudly or not at all.** At startup every Package's metadata is read and every
   uploaded artifact is re-hashed by streaming. Metadata that cannot be read or parsed, an invalid
   type, version or Platform, a content hash that is not hex, a stored Package whose name disagrees
   with the identity its metadata states, or an artifact that no longer matches its recorded hash
   fails startup naming the file. A corrupt distribution artifact must never ship, and a store that
   silently drops a Package looks like one that was never given it.

9. **Two names are derived, and they answer different questions.** The **wire name** is the Agent
   type alone: the `PackagesAvailable` map key, the key an Agent reports its `PackageStatuses`
   under, and the value the Client's `[self_update] package` is compared against
   ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)). It must be stable across
   versions. The **display name** is `<agent_type> <version>`, used in the UI and the log. Using
   one name in both places would break the Client's self-update guard on every release or make the
   fleet view unreadable.

10. **The hashes are fixed constructions, and both are shown.** The per-package hash is SHA-256
    over the version's length (u64, little-endian), the version, and the entry's content hash; the
    Platform has no place in it. `all_packages_hash` is SHA-256 over the Agent type's length, the
    Agent type, and that per-package hash. Each entry in the API carries its `content_hash`, the
    exact value an Agent verifies against, and its `package_hash`, the value an Agent echoes once
    in sync.

11. **Every offered Package is top-level.** `PackageType::Addon` is never emitted. The Client's
    refusal of an addon offer stays where it is (clause 49): it defends against a foreign Server,
    not against this one.

12. **The Package routes.**
    - `GET|PUT|DELETE /api/v1/packages/{agent_type}/{version}`; `PUT` takes no body.
    - `PUT|DELETE …/entries/{os}/{arch}` with the artifact as the body.
    - `PUT|DELETE …/entries/{os}/{arch}/signature` with `{signature}`.
    - `GET /api/v1/packages` lists each Package with its entries, each entry's
      `signature_state`, and the Deployments holding it.
    - There is no `…/entries/{os}/{arch}/source` route and no Deployment signature route
      `…/deployments/{name}/signatures/…`; a request to either is answered `404`.
    - The download is `GET /api/v1/packages/{agent_type}/{version}/file?os=…&arch=…` on the
      Agent plane.
    - A Package has no Selector route and no rollout route. The OpenAPI document follows.

13. **The Packages tab is one table and one detail form, and it signs.**
    - One row per Package: Agent type, version, the platforms it holds, the Deployments that hold
      it, and whether every entry carries a valid signature. Selecting a row makes it the current
      record and shows the form; while nothing is current the form is not on screen.
    - **Create** opens the form empty, the only moment the identity fields are writable.
    - The form adds and removes entries in place, each from a file; a file named as a release
      artifact fills the platform and, while creating, the identity.
    - Each entry shows its hash, size and `signature_state`, with an `invalid` one marked as a
      warning.
    - The signing-file download and import both live here (clauses 21 and 24).
    - **OK** persists and **Cancel** discards; both conclude the form, and only a failed save keeps
      it open beside its message. **Delete** stands alone on the left. The form enforces no rule
      of its own: what it greys out is what the Server answers `409` to.

### Fit

14. **Fit is mandatory, comes before everything else, and has no "unknown, so anything goes".** A
    Package can be meant for an Agent only when
    - its **Agent type equals** the `service.name` the Agent reports among its identifying
      attributes, compared raw, with no canonicalisation table and no prefix or pattern, because
      there is no authority to canonicalise Agent types against and `otelcol` and
      `otelcol-contrib` are different binaries; and
    - it holds an entry for the **Platform** the Agent reports, read from `os.type` and
      `host.arch`, non-identifying attributes first and an identifying copy as fallback,
      canonicalised as in clause 6. `os.description` (prose) and `os.version` (a release, not a
      system) are not read.

    An Agent that reports no type, or no `os.type` or `host.arch`, fits nothing. A per-Agent
    rollout act refused on fit says which of the three it was. A Label can never supply or override
    these attributes ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)), so a slip there
    cannot hand a host an artifact built for another machine.

15. **The Platform does not travel on the wire.** All Agents of one type are offered the Package
    under the same key, each with the download URL and content hash of the entry that fits it (two
    platforms' artifacts differ in content hash by construction), so the Client's package handling
    is identical on every target and one `[self_update]` consent works across all five release
    platforms.

16. **The Server decides which artifact an Agent receives, never a file on the host.** A
    `[[supervisor]]` block names no package; one carrying `package` fails at startup with a message
    saying the Server decides. Type fit is also the second, independent guard on the Client's own
    binary: the Server does not offer a Package of another type to the Client's Agent, whatever it
    is called, and the Client still refuses an offer outside its `[self_update]` consent
    ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)). One side refuses to
    send, the other to install.

17. **The version test reads the claim under the Agent type**, because that is the key an Agent
    reports its status under; the version test is otherwise as
    [ADR-0026](0026-deployments-and-rollouts.md) states it.

### Signatures

18. **The signature lives on the entry, and the wire carries no field of its own for it.**
    - `PUT|DELETE /api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}/signature` writes or
      removes the hex Ed25519 signature, as `opamp-fleetctl package sign` prints it.
    - A signature for an entry that does not exist is `404`, an empty or malformed one `400`.
    - Writing new bytes to an entry removes its signature, because a signature covers the hash.
    - A signature write on a Package assigned to an Agent is refused (`409`), like any other write
      to its entries ([ADR-0026](0026-deployments-and-rollouts.md), writes to an assigned
      Package).
    - What an Agent receives in `DownloadableFile.signature` is the signature of the entry for its
      Platform. An entry without one is no candidate, the Agent is offered nothing for it, and the
      fleet view says the signature is missing.
    - One signature serves every Deployment that holds the Package.

19. **The Server checks a signature when it holds the operator's public key, and only reports.**
    - `server.toml` may name `[packages] verification_key`: the hex Ed25519 public key, in the
      format the Client's key of the same name uses. A malformed key fails startup naming it.
    - With the key, the Server verifies an entry's signature over
      `statement(agent_type, version, content_hash)` when the signature is written and when the
      store is loaded.
    - Each entry reports `signature_state`: `missing`, `valid`, `invalid`, or `unchecked` when no
      key is set.
    - An `invalid` signature is stored and shown with a warning; it is never refused. The Client's
      verification (clause 52) is the one that decides, and a Server without the key verifies
      no signature and reports each as `unchecked`.

### The signing file

20. **The file.** One JSON object per Package:

    ```json
    {
      "format": "opamp-fleet-signing-v1",
      "agent_type": "supervisor",
      "version": "0.5.1",
      "entries": [
        { "os": "linux", "arch": "amd64", "sha256": "127f…cfb9e", "signature": "9a1c…" }
      ]
    }
    ```

    `format` is exactly `opamp-fleet-signing-v1`; a reader refuses any other value rather than
    guess at it. `agent_type`, `version`, `os` and `arch` are the names the REST API uses for the
    same values. `sha256` is the entry's `content_hash`, 64 lowercase hex characters. `signature`
    is absent in the downloaded file and 128 hex characters once signed. The platform is **not**
    part of what is signed (clause 52); it is in the file so the import knows which entry each
    signature belongs to.

21. **The download.** The Packages tab offers the file for the selected Package, built in the
    browser from the `GET /api/v1/packages/{agent_type}/{version}` the page already read. No new
    route: the file is a projection of an existing response.

22. **Signing.** `opamp-fleetctl package sign --key <file> --signing-info <file>` is the second
    form of `sign`, exclusive with the artifact form. It checks the whole file first (the format,
    every hash's shape, no platform twice) and signs nothing if any of it fails; then it signs
    `statement(agent_type, version, sha256)` for every entry and writes the file back with each
    `signature` filled, to `--out <file>` or in place. An existing `signature` is overwritten:
    signing again with the same key gives the same value (Ed25519 is deterministic), and with
    another key the new one is the one meant.

23. **The hashes are the operator's to check.** `sign --signing-info` verifies nothing against the
    artifacts or upstream; it prints each entry's platform and hash as it signs, and the manual
    states the duty: compare the hashes with the checksums upstream published or with
    `opamp-fleetctl package sha256` over the files before signing. This is the decision's
    deliberate trade-off (Consequences).

24. **The import is on the Packages tab.** The tab takes a signed signing file and puts each
    entry's signature through
    `PUT /api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}/signature`, one call per
    entry, and reports per entry what the Server answered, the `signature_state` included. The
    file format `opamp-fleet-signing-v1` names no Deployment.

### Deployments and Packages

25. **At most one Package per Agent type.**
    `PUT /api/v1/deployments/{name}/packages/{type}/{version}` adds a Package; a second one of a
    type already held is refused (`409`) naming the one held, unless the request says
    `?replace=true`, which swaps the version the channel holds. Writing the held one again
    succeeds. A Package the store does not hold is `404`. Two of one type would collide on the wire
    key *and* fit the same Agent; refusing at the write turns a resolution-time mystery into an
    error at the moment of the mistake, and, the collision being on the type, which is identity,
    there is no dead end where every write is refused.

26. **A Deployment's Selector is never empty, and no pair has a blank half:** `400` at creation and
    at every edit, with a message naming what to write. An empty Selector is the channel that
    collides with every other, and a forgotten field would silently become the base for the whole
    fleet.

27. **Channels are a partition by a host property, and there is no "everyone".** The Server
    prescribes no key and reserves no word. The key an operator picks says what the partition
    means: `channel` (`stable`, `beta`) for release risk, `region` for where it runs, `tenant` for
    whose it is; keys compose as further equality pairs. Each names a property of the **host**,
    arriving from provisioning through the Client's `[attributes]` table or as a Server Label,
    which moves a host between channels without touching it. An Agent no Deployment claims waits;
    it is the ordinary state of a fresh enrolment, not an error. The fleet view tells apart an
    Agent in no Deployment, one whose Deployment holds nothing it can take (no Package for its
    type or its Platform), one with something waiting, and one in conflict, because the next move
    differs.

28. **An Agent belongs to at most one Deployment; any overlap is a conflict.** Not the most
    specific, not the newest: none. The Agent's `package_conflict` names every Deployment that
    claims it, and it is offered nothing new until an operator narrows a Selector. There is no
    specificity rule anywhere.

29. **A conflict takes the candidate away, never a standing assignment.** An Agent already rolled
    out to keeps its offer: nothing distributes or un-distributes by itself, and creating an
    overlapping Deployment must not withdraw software from a running host.

30. **The per-Agent act refuses to pick a side.** Naming a Deployment while a second one also
    claims the Agent is `409`, as is naming one that does not claim it. Otherwise the conflict is
    sidestepped for good, and the per-Agent path becomes the way into a state the bulk act forbids.

31. **A Deployment reports three counts.** `claiming_agents`: Agents it claims and no other does;
    zero is the aim mistake worth hunting. `targeted_agents`: of those, the Agents a rollout would
    move. `conflicting_agents`: Agents it matches that another Deployment matches too. The UI shows
    "⚠ n in conflict", the "Roll out (n)" press, "n up to date", or "aims at nobody" accordingly.

32. **An absent assignment means nothing was rolled out.** An Agent record carrying no assignment
    fields loads as assigned nothing; the Server never invents a rollout at startup. Records are
    written in the envelope [ADR-0029](0029-the-servers-state-and-its-stores.md) holds, and an
    envelope of another version stops the Server with a message saying that `server migrate`
    moves a data directory an older Server wrote.

### The download route

33. **Uploaded artifacts are served on the Agent plane, and offered as a path.** The download
    route is the one `/api/v1` route on the Agent-plane listener
    ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)), not on the Operator plane and not
    in the OpenAPI document. It streams the file and never holds it in memory. The offered
    `download_url` is always a path, which the Client resolves against its own OpAMP endpoint.
    There is no `advertised_url`: a `server.toml` that names it fails startup, naming the key as
    removed. A download through another host would be refused by every Client (clause 51).

34. **The package download stays on the Agent plane, outside the credential check, under its
    `/api/v1` path.** The offered `download_url` is a path the Client resolves against its own
    endpoint. The download is reached through the same TLS handshake as `/v1/opamp`, so the Client
    presents its client certificate to its own Server's origin and to no other host. What the
    handshake requires of that certificate is the admission decision's
    ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). The route is served on
    that one listener only (the Operator plane answers it `404`), and the OpenAPI document,
    generated from the Operator plane's routes, does not carry it; the manual documents it as the
    Agent plane's.

35. **"Currently offered" is the offer's own test, shared.** An artifact `(agent_type, version, os,
    arch)` is offered to an Agent when all of the following hold:
    - the Agent declares `AcceptsPackages`;
    - it holds a package assignment naming that Package;
    - the Agent type equals the `service.name` it reports;
    - the Platform it reports, canonicalised, is the requested one, and the Package holds an entry
      for it;
    - that entry carries a signature.

    The offer and the download decide this from the same parts in `packages.rs`, and a test that
    runs every combination through both keeps them from disagreeing.

36. **An offer stands until the assignment changes, not until it was last sent.** The hash gate
    suppresses re-sending an offer whose `all_packages_hash` the Agent has echoed
    ([ADR-0026](0026-deployments-and-rollouts.md)). It does not withdraw the offer, so an Agent
    retrying after a failed install can fetch again. Neither connection state nor the version test
    ([ADR-0026](0026-deployments-and-rollouts.md)) enters the test. Matching does not enter it
    either: a version that waits for an operator's press is no one's offer, and it cannot be
    fetched until the press releases it.

37. **Every other request for an artifact is answered `404`, as for one that does not exist.** The
    status, the body and the headers are the same whether the store holds no such artifact or holds
    an artifact that is not offered to any Agent the requesting host speaks for (what a certificate
    speaks for is [ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)'s). The Server decides
    this before it opens a file. Two answers differ:
    - a malformed identity or Platform token is `400`, which concerns syntax and reveals nothing
      about the store;
    - admission refusals (no member certificate, a bootstrap certificate, a revoked certificate)
      are `401` ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)).

38. **The handler decides, after it has parsed the request.** `download_package` parses the
    identity (`PackageId::new`) and the Platform (`query.platform()`), so one parser decides which
    artifact is meant and a malformed token is answered `400` before any offer is tested. It then
    reads the host from the presented certificate: from the `PeerCertificate` extension through
    `ca::facts`, or from the `Proofs` the guard puts into the request. It asks the fleet
    `Fleet::offers_artifact(host, id, platform)`, which reads the host register
    (`Revocations::speaks_for`) and the Agent records under the fleet lock. Only after that does it
    look for the file. `admit_download` keeps only admission
    ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) and records
    `download.admitted`. A request that fails the offered test is recorded as `download.refused`,
    outcome `refused`, with check `not offered`, the host, the certificate's serial and the
    requested type, version and Platform. That entry goes through the refusal aggregation of
    [ADR-0023](0023-an-append-only-audit-record.md). It does not count toward the admission
    throttle ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)): the handshake
    succeeded, so the refusal is not a guess at a credential, and counting it would throttle every
    member behind the same address.

39. **Each download request is counted against the requesting host's rate.** It costs one token
    from that host's bucket ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)). A download
    names no `instance_uid`, so for a host marked as a Gateway the token comes from the Gateway's
    aggregate bucket ([ADR-0015](0015-client-modes-and-the-gateway.md)). That bucket bounds how
    often a Gateway's request can scan the fleet's records under the fleet lock.

40. **Configurations: this decision adds no bound.** An Agent receives only its own composed
    map, over OpAMP, from what was released to it. This decision states that bound for the record
    and adds nothing to it.

### The Gateway's cache

41. **A Gateway fetches an uploaded artifact once, when it relays its offer.** When the Gateway
    relays a `ServerToAgent` whose `packages_available` names a file on the Server's download route
    (clause 42) to an Agent, and records that offer
    ([ADR-0015](0015-client-modes-and-the-gateway.md), what a host receives downstream), it starts
    fetching each such artifact that it neither holds nor is fetching, before any downstream peer
    asks for it. An offer it does not record starts no fetch. The fetch is single-flight per
    artifact. The Gateway resolves the path against its own endpoint and fetches from its Server's
    origin with its own client certificate and no offered headers, by the rules every Client's
    download follows (clause 51). A marked Gateway may fetch what is offered to any Agent
    ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)), and each request costs one token
    of its aggregate bucket (clause 39). The message itself is forwarded unchanged and without
    waiting for the fetch ([ADR-0015](0015-client-modes-and-the-gateway.md)). At most four
    fetches run at a time; further ones wait for a free slot. A `429` or `503` from the Server
    with `Retry-After` defers the fetch within the bound of clause 47, and a deferred fetch gives
    its slot back while it waits and takes one again before it asks anew. Once its first 60
    seconds have passed, a fetch is cut at the first chunk that finds it below an average of
    64 KiB/s since then; the read timeout of clause 51 still cuts a silent one. A Gateway that
    shuts down ends its fetches, waiting or not. Any other failure (the Server's refusal, where an
    unmarked Gateway is answered `404` by clause 37; a network error; a cut; bytes that do not
    match the hash) is logged and remembered for that artifact. It is fetched again only when it
    newly appears in an Agent's offer, that is, in a relayed offer to an `instance_uid` whose
    previous recorded offer did not name it.

42. **The Gateway caches every offered artifact the Server hosts.**
    - An offered file is the Server's when its `download_url` is a path beginning
      `/api/v1/packages/` and naming the route's `/file`. The Server sends nothing else
      (clause 33), and a downstream Client resolves the path against its own endpoint, the
      Gateway.
    - An absolute `download_url`, which only a foreign Server could send, is neither fetched nor
      served by the Gateway.

43. **The Gateway stores and serves only bytes that match the offer's hash.** An artifact is the
    offered path, query included, together with the offer's `content_hash`. The fetch streams the
    body to a staging file in the cache directory and hashes it on the way; peak memory is one
    chunk. The file is renamed into place only when its SHA-256 equals the offered
    `content_hash`; otherwise it is deleted, the mismatch is logged, and nothing is served. The
    Gateway does not check the signature. The downstream Client checks hash and signature itself
    before it installs (clause 52), so the cache cannot make an Agent install anything the Agent
    would not install from the Server.

44. **The Gateway logs what it refuses; it keeps no audit record.** The audit record is the
    Server's ([ADR-0023](0023-an-append-only-audit-record.md)). A downstream request answered
    `404` because no offer of that artifact was recorded for the requesting host is logged at
    `info` with the host (or that the certificate names none), the certificate's serial and the
    requested path without its query. An offer not recorded because its `instance_uid` is bound to
    another host is logged at `warn` with both hosts. Both kinds of line are aggregated per host:
    the first five in a minute are logged one by one, and the rest of that minute are counted into
    one line when the next minute's first line for that host arrives. The Server records the
    Gateway's own fetches as it records every download (clause 38).

45. **The cache is bounded, and an artifact that does not fit is refused.**
    `[gateway] package_cache_bytes` bounds the bytes the cache holds, stages and is deleting
    together, default `10737418240` (10 GiB); `0` is refused at load. The cache lives in
    `<state_dir>/gateway-packages`, owner-only on Unix, emptied when the Gateway starts. Every
    stored copy has a file name of its own, so deleting an old copy never touches a newer copy of
    the same artifact. Before a fetch stages a byte it reserves the artifact's `Content-Length`,
    or the per-artifact limit when the Server sends none, against `package_cache_bytes`. To make
    room the Gateway first deletes artifacts no current offer names, least recently used first,
    and then offered ones, least recently used first. A file being deleted stays counted until it
    is gone, and one whose deletion fails stays counted until a later attempt deletes it. A held
    artifact whose file has disappeared is forgotten. A fetch that cannot reserve room because
    other fetches hold it fails (clause 41). On a verified rename the reservation becomes the held
    artifact's size; on failure it is released. A fetch is cut, and nothing stored, once its
    `Content-Length` or its body exceeds the smaller of `package_cache_bytes` and
    `max_artifact_size_bytes`; the Gateway logs this once per artifact and does not fetch that
    artifact again while it runs. Such an artifact is not streamed through: a downstream request
    for it is answered `404` ([ADR-0015](0015-client-modes-and-the-gateway.md)). No file operation
    runs while the cache's shared state is locked.

46. **Downstream download requests are not counted by the Gateway.** A downstream request costs a
    lookup in memory and a read from the cache directory. It reaches the Server only through the
    fetches of clause 41 and the request-triggered fetch
    ([ADR-0015](0015-client-modes-and-the-gateway.md)): one per artifact that newly appears in an
    Agent's offer and is not held, and at most one request-triggered fetch per artifact per
    relayed offer. Two artifacts that do not fit the cache together therefore cannot evict each
    other in a loop driven by requests. Each request to the Server costs a token of the Gateway's
    aggregate bucket (clause 39). As on `/v1/opamp`, the Gateway applies no rate of its own.

47. **A Client waits as its own Server origin asks before it reports a download failed.** A `429`
    or `503` with a `Retry-After` in seconds, answered by the Client's own Server origin (the
    Gateway behind one, otherwise the Server, whose rate limit answers `429` with
    `Retry-After: 30` ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)) and whose
    admission throttle answers `429` with the seconds of back-off left
    ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md))), is waited out and the
    download sent again. Each wait is the `Retry-After`, at least 1 second and at most 60 seconds.
    The asking stops 30 minutes after the first request of the download, counting the requests as
    well as the waits: a wait that would end past that point is not begun. Past it, or for a
    `429`/`503` without a `Retry-After` in seconds (an HTTP date, none, or an unreadable one), or
    from any other host, a redirect target off the Server's origin included, the download fails
    and is reported `InstallFailed` (clause 50). While it waits the status stays `Downloading`.
    The bound is on asking, not on a transfer that has begun, which keeps no total timeout
    (clause 51). A Client that shuts down stops waiting and reports nothing for that download.

### Delivery on the Agent

48. **The Supervisor is the updater, and only a Client holding a verification key takes
    packages.** With `[packages] verification_key` set, every Supervisor-backed Agent declares
    `AcceptsPackages` and `ReportsPackageStatuses` (every Managed Process is one this Client
    installed, [ADR-0017](0017-supervisor-mode-and-its-kinds.md)). Without it, no Agent of this
    Client declares `AcceptsPackages`, neither a Supervisor nor the Client's own Agent
    ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)), and the Client says so
    at startup, once, naming `[packages] verification_key` as what to set. A package offered to
    such an Agent regardless is ignored: nothing is downloaded, and an Agent that declared no
    package capability reports no package status. A verified artifact travels to the Supervisor as
    the Port command `ProcessCommand::ApplyPackage { staged, version, hash }`, answered by
    `ProcessEvent::PackageApplied`, beside `ApplyConfig`/`ConfigApplied`. No further process is
    spawned for it.

49. **One top-level package per Agent; anything else is refused and reported.** An Agent takes the
    one top-level package of an offer. An offer carrying only addons, or two top-level packages,
    is refused with a reason in `PackageStatuses.error_message` and nothing is downloaded: the
    Baseline lets any Server offer addons, and a Supervisor's only use for a package is to *be* the
    program, so the filter guards against a non-conforming peer writing an addon over the binary.
    An offer whose per-package hash equals the installed one is acknowledged by echoing the
    aggregate hash; a repeat of the hash already in flight is ignored.

50. **Status follows the Baseline lifecycle, and every outcome ends the offer.** While the bytes
    arrive the status is `Downloading` with `PackageDownloadDetails` (percent from
    `Content-Length`, `0` when unknown, and bytes per second); otherwise a taken offer is
    `Installing`, with `agent_has_version` still the old one; the Supervisor's answer makes it
    `Installed` or `InstallFailed` with the reason. A failed download or verification is
    `InstallFailed` too. Success *and* failure echo the offered `all_packages_hash`, so the Server
    stops re-offering the same bytes: a refusal is a report, not a loop. The installed package
    (name, version, hash) is persisted in the Agent's state, so a restarted Client reports what it
    runs and is not offered it again.

51. **The Client downloads only from its own Server origin; the download is streamed, bounded and
    staged inside the Supervisor's own directory.**
    - A `download_url` that is a path is resolved against the Client's own origin: the scheme,
      host and port of its OpAMP endpoint, `wss` mapped to `https` (and `ws` to `http`, which the
      endpoint may only be on the loopback).
    - An absolute `download_url` is accepted only when its origin is that origin. Any other is
      refused without a byte fetched, reported `InstallFailed` naming its origin.
    - A source is `https://`. `http://` is accepted only when its host is the IP literal
      `127.0.0.1` or `::1`; a host name is never the loopback, `localhost` included. Every
      download is TLS 1.3, on the Client's TLS trust.
    - There is no `[packages] allowed_sources`. A `supervisor.toml` that names it fails startup
      naming the key, so no operator believes a list is in force that is not.
    - The offered package name must be 1–64 characters of letters, digits, `.`, `_`, `+`, `-`,
      checked before anything is resolved or written: the staged file is named after it.
    - The artifact is streamed to `<supervisor_dir>/packages/<name>.staged` and hashed on the way;
      peak memory is one chunk. The staging directory is owner-only (`0700` on Unix), so no other
      local user can swap the file between verification and install. A failure leaves no partial
      file.
    - `max_artifact_size_bytes` (top level of `supervisor.toml`, default `1073741824`, `0` fails
      startup) bounds the download: a `Content-Length` above it is refused before a byte is
      written, and a chunked body is cut the moment it crosses it.
    - Connect timeout 30 s and read timeout 60 s, never a total timeout: a large artifact over a
      slow link legitimately takes minutes.
    - Redirects are followed up to 5 and only while every hop stays on that origin; a hop off it
      fails the download, naming the origin it pointed at.
    - The offer's `headers`, if any, are sent only to that origin; an invalid header fails the
      download naming its key only. Header values never reach a log or a debug print, and the
      logged source URL drops its query, fragment and credentials.
    - The Client presents its client certificate only to the Server's own origin, whose download
      route is reached through the Agent plane's TLS handshake and stays outside the credential
      check ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md),
      [ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). An identity belongs to
      the Server, not to whoever hosts an artifact.

52. **Verification decides, and the signature is mandatory.** The SHA-256 of the streamed bytes
    must equal `content_hash`, always. The Ed25519 signature must then verify against
    `[packages] verification_key` (hex-encoded Ed25519 public key, decoded at startup; a malformed
    key fails startup), always. An offer with no signature is refused before anything is
    downloaded; an invalid signature refuses the artifact. Both checks complete on the staged
    bytes before the artifact is opened, so an unverified archive is never parsed beyond its hash
    and signature. There is no unsigned posture: a Client without a key takes no packages
    (clause 48). The signature covers a statement of what the artifact is, not its bytes:
    `opamp-fleet-package-v1`, the Agent type (the offered package's name), the version and the
    artifact's SHA-256 in lowercase hex, one per line, each ended by a newline
    (`fleet_core::package::statement`). `opamp-fleetctl package sign --agent-type … --version …`
    makes it. Since the hash is in the statement and checked against the streamed bytes first, the
    signature still covers the artifact exactly as published, archive and all, and holds for that
    type and version alone. Where the signature is kept is clause 18. The same download and
    verification serve the Client's own update
    ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)).

53. **The Agent opens the artifact; nothing between its author and the host repacks it.** The hash
    an Agent verifies is therefore the SHA-256 the artifact was published with: integrity runs in
    one line from the release page to the running program. What an artifact is, is decided by its
    leading bytes, never by a name: `1f 8b` is a `.tar.gz`; `37 7a bc af 27 1c` is a `.7z`;
    `PK\x03\x04` or `PK\x05\x06` is a `.zip`; anything else is the program itself. These three
    containers are the whole set the Client opens, for every kind and for its own update alike. A
    `.deb`, `.rpm` or `.msi` is not opened.

54. **A `.7z` may be encrypted, and the key lives only on the Agent.** `[packages] archive_key`
    opens an encrypted `.7z` (AES-256); the Server never learns it, so a confidential artifact is
    readable only on the host that runs it. It is one secret for the fleet (a single archive
    serves every Agent) and must never be the `[auth]` credential, which the Server rotates on its
    own ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)) and would leave
    every archive unopenable. It is masked in the configuration the Client reports. An encrypted
    archive without the key, or with the wrong one, fails naming the key. A `.zip` is never
    encrypted: an encrypted zip member refuses the archive and names `.7z` as the way.

55. **A single-file package installs one member, to a place the Client chose.** Without
    `program_path`, the artifact is the program or an archive holding it: the member whose *file
    name* equals the configured program's is extracted, wherever the archive keeps it, and nothing
    else. The archive never chooses a path, so a member named `../../etc/cron.d/x` lands where the
    Client put it like any other. A missing member fails, naming up to eight members the archive
    does hold. Output is bounded at 2 GiB, and the decompression needed to *reach* the member is
    bounded by the same budget from the declared sizes before anything inflates, so a bomb ahead
    of the target is refused rather than skipped through.

56. **A `[[supervisor]]` block's `program_path` makes the package a directory tree.** It is a
    relative path *inside* the package, e.g. `bin/fluent-bit`; absent, the package is one file. It
    is validated at startup (non-empty, relative, no `.` or `..`), and it says *where inside*,
    never *whether*: the program's bare file name stays the consent
    ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)). Being written in the configuration, the
    spawn path `program/tree/<program_path>` is known before any package exists.
    - `program_path` matches a member by its **trailing path components**, so `bin/fluent-bit`
      finds `fluent-bit-3.1.0/bin/fluent-bit` and stays right at the next release. No match fails
      naming what the archive holds; several matches fail naming them, answered by writing more of
      the path.
    - The directory prefix above the match is stripped, and every member below it is extracted
      keeping its relative path. Members outside that prefix are not unpacked; they are counted in
      the install line and named at `debug`.
    - A bare program delivered to a tree Supervisor is placed at `program_path`.

57. **A tree's paths are sanitized and its size is bounded, or nothing is written.** Every member
    is checked before the first byte lands. An absolute path, a root or drive prefix, a `..`
    component, a symbolic or hard link, or a 7z anti-item refuses the **whole** archive: a
    partially unpacked agent is worse than none. Extraction is bounded at 2 GiB across all members
    and at 10 000 members, from declared sizes before decompression and again on what is written.

58. **The swap is a rename, and the apply grace gates it.** On disk, per Supervisor:

    | Package | Live | Predecessor | Staged |
    |---|---|---|---|
    | single file | `program/<file>` | `program/<file>.rollback` | `program/<file>.staged` |
    | tree | `program/tree/` | `program/tree.rollback/` | `program/.staging/` |

    The artifact is unpacked to the staged name first, beside what runs; a raw artifact is moved
    there rather than copied when it can be. A kind that declares a preflight proves the staged
    program runs before anything is stopped ([ADR-0017](0017-supervisor-mode-and-its-kinds.md),
    the preflight). Then the Managed Process is stopped, the live name renamed to the predecessor,
    the staged name renamed to the live one, so the live name is always the old package or the new
    one, never a mixture, and the process is spawned and must survive `apply_grace_secs`
    ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)). Surviving is `Installed`, after which the
    program is asked for its version again. A spawn failing with `ETXTBSY` right after the swap is
    retried briefly rather than rolled back. When there is nothing to run yet (no Configuration),
    the package is installed and reported `Installed`; the configuration that arrives starts it.

59. **A failed apply rolls back only to a predecessor.** If the process does not start or exits
    within the grace and a predecessor exists, the predecessor is renamed back over the live name
    and respawned. If there is none (a first install), nothing is rolled back: the verified
    program stays in place and is reported `InstallFailed`. A failure never empties `program/`, so
    the "installed ≠ offered, re-offer, re-download" loop cannot start.

60. **A program that keeps failing to start is held, not looped.** A start counts as failed when
    the apply fails its grace, or the process exits before it has run `max(apply_grace, 10 s)`.
    After **three** failed starts in a row the Supervisor stops restarting, reports the Agent
    unhealthy (`not restarting: the program keeps failing to start`) and waits. A new
    Configuration, a new package or a restart command is a fresh chance and resets the count; a
    run that outlasted the floor resets it too. This holds for a failed package, a rolled-back
    predecessor that will not start either, and a Configuration alike. Three is the self-update's
    give-up ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)), so the two
    update paths behave alike.

61. **The superseded version is kept for a window, then deleted.** After a successful apply the
    predecessor stays, and a marker beside it (`<predecessor>.until`, Unix seconds) records
    `now + retain_previous_secs`. A sweep at startup and every 10 minutes deletes a predecessor
    past its deadline; a marker that will not parse counts as expired, and a predecessor without a
    marker is left alone. `[updates] retain_previous_secs` sets the window (default `86400`, one
    day); a `[[supervisor]]` block's `retain_previous_secs` overrides it for that Supervisor;
    negative values fail startup; `0` deletes the predecessor on success. A Supervisor keeps at
    most one predecessor: the next update replaces it and its marker. The marker lives in the
    Supervisor's directory, so the deadline survives a Client restart.

62. **The Client opens `.zip` as a third container beside `.tar.gz` and `.7z`.** It is detected by
    its leading bytes (clause 53), held to exactly the member and tree rules of clauses 55 to 57,
    read-only, and unencrypted (clause 54: a confidential artifact is a `.7z`). Like a `.7z` it
    carries no Unix modes, which costs a Windows tree nothing. It is read through the
    [`zip`](https://crates.io/crates/zip) crate with `default-features = false` and `deflate`
    only, so decompression runs on the `flate2`/`miniz_oxide` chain the Client already carries and
    the build stays pure Rust. An upstream that publishes its Windows build as a zip therefore has
    it travel byte for byte: the hash an Agent verifies is upstream's own.

**Out of scope:** which Package reaches which Agent and when, and the Deployment's own shape and
rollout ([ADR-0026](0026-deployments-and-rollouts.md)); how a Package version is written and the
floors below which nothing is taken
([ADR-0028](0028-package-versions-high-water-mark-and-floor.md)); where and how the Server keeps
the bytes ([ADR-0029](0029-the-servers-state-and-its-stores.md)); what a certificate speaks for
([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)); what a host behind a Gateway receives
from it ([ADR-0015](0015-client-modes-and-the-gateway.md)); verifying upstream sigstore/cosign
signatures; a retention policy for superseded Packages; inequality in Selectors; refusing an
overlapping Selector at write time; whether `[self_update] package` should be renamed, since its
value is an Agent type; addons, and installing them, until a Client can install one; a bound
finer than the host; roles, permissions and tenancy on the Operator plane; keeping the Gateway's
cache and the offers it serves across a restart of the Gateway, and the Gateway's own Supervisors
fetching through the cache; resuming an interrupted download with range requests; distributing
the archive key or the verification key; keeping more than one predecessor; refusing an `invalid`
signature on the Server; more than one verification key on the Server, or rotating it; signing on
the Server or through it; an import or signing route on the Server; `sign` writing signatures to a
Server itself; a signing file spanning several Packages; an external signer (HSM, `openssl`)
reading the signing file, for whom the statement of clause 52 is documented; a reproducible tree
mode for `opamp-fleetctl package pack`.

## Alternatives considered

- **Leave the platform or the type to the operator's aim.** `os.type`/`host.arch`/`service.name`
  pairs already filter at zero cost. Rejected: opt-in, and forgetting it installs a foreign binary
  on every host of every other platform or type. A rule that is only right when remembered is not
  the rule this needs.
- **An optional Platform meaning "every platform".** The backward-compatible reading of a filter;
  rejected because it makes the guarantee a property of how carefully something was uploaded
  rather than of the Server.
- **Platform as metadata on separate package names** (`otelcol-linux-amd64`). Smaller, but five
  artifacts then need five names and every host a matching name in its own configuration, the
  per-host wiring this store exists to remove, at the one place where the blast radius is the
  Client itself.
- **A version per platform entry.** Allows a per-platform rollout of different versions under one
  object, and is indistinguishable from a half-finished upload. Two Packages express that case
  explicitly.
- **Encode the platform in the version** (`3.0.0+linux-amd64`). Needs no schema, and contradicts
  [ADR-0033](0033-the-products-version.md): build metadata is provenance and is not compared.
- **Canonicalise onto what Rust reports** (`x86_64`, `aarch64`). The smaller change; rejected
  because it makes this project's canonical spelling one that neither the conventions, nor the
  Collector, nor the release artifact uses: a private vocabulary maintained by a table, forever.
- **Reject unknown `os`/`arch` tokens against a closed list.** Catches a typo, and refuses to serve
  any platform the list forgot. The canonical pair in the response covers the typo far cheaper.
- **Canonicalise or pattern-match Agent types.** There is no authority to canonicalise against; a
  table would encode this project's opinion of every distribution, and a pattern is a matching
  language that outlives its convenience.
- **Default the type to a name**, so no extra input is needed. Right most of the time and silently
  wrong for every name chosen for a rollout rather than a target.
- **Remember one previous version per Package and roll back by swapping.** Bounded disk, one
  button, but hidden, one step deep, and a second history beside the store. Keeping every version
  as its own Package makes the history the store itself.
- **Referenced entries: a `url`, a mandatory `sha256` and optional headers, offered verbatim.** A
  Package would be a URL an operator already has, nothing uploaded and nothing on the Server to
  leak, with an `https://` rule, a loopback-literal exception, and a one-time `HEAD` probe over
  TLS 1.3 that follows no redirect and refuses non-routable addresses. Rejected: every host has to
  fetch from upstream and needs internet access, the Gateway cannot cache such an artifact, so a
  host behind a Gateway without internet receives nothing, and the headers are stored in
  cleartext and handed to every Agent offered the entry, frozen once the Package is assigned, so
  an expired or leaked token can only be replaced by a new Package version. Keeping them with the
  credential held by the Server per URL prefix, or only on the host per allowed source, would
  solve the frozen token and keep the rest.
- **Plaintext referenced URLs to LAN mirrors.** An HTTP file share is the cheapest mirror, and the
  hash and signature protect what an Agent installs. Rejected: they protect integrity, not
  confidentiality; the artifact and any download credential would cross the network readable to
  anyone on it, and the specification accepts plaintext on the loopback alone.
- **Import a URL: have the Server download it and serve the bytes.** As the behaviour of a URL it
  makes the Server hold and expose artifacts it has no need to hold; uploading is what a fleet
  whose hosts cannot reach the internet needs, and it is the only way in.
- **Derive the checksum by fetching once, or verify a source by downloading at set time.** Records
  what the Server happened to receive, from where it stands: trust on first use with no anchor.
- **A base URL per Package from which entries and hashes are loaded.** A URL yields neither asset
  names nor hashes, which differ for every upstream. That knowledge per agent lives in
  `opamp-fleetctl package fetch` ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)), which
  uploads.
- **An `advertised_url` for a mirror or another host in front of the download.** With the Client
  downloading only from its own Server's origin, an `advertised_url` naming another host reaches
  no Client, and one naming the same origin says what the path already says. With it set, an
  uploaded artifact would not reach a host behind a Gateway at all. A setting that cannot work is
  one an operator can believe in wrongly.
- **An allow-list of download sources on the Client (`[packages] allowed_sources`), of `https://`
  URL prefixes matched by exact origin and a path at a `/` boundary.** It bounds where an offer or
  a redirect can send the Client's headers and its fleet identity, which matters only where
  entries reference other sources. This Client serves this Server; with every artifact on the
  Server's origin, a list nothing uses is a setting an operator can believe in wrongly. An open
  policy, any `https://` source with the hash and signature deciding, would send the offered
  headers and the client certificate wherever an offer or a redirect points.
- **Check only the first URL, not every redirect hop.** A host on the path could then send the
  download, and the headers re-attached on its own origin, anywhere.
- **Present the client certificate to every source.** It would hand the Client's fleet identity to
  a host that has no use for it, and to every host a redirect reaches.
- **An opaque identifier instead of readable tokens.** Robust against any character, meaningless in
  a URL, a file name and the UI; a bounded token grammar buys the same safety.
- **Keep a name as an optional field defaulting to the Agent type.** Smallest change; the room it
  leaves is the problem: the degree of freedom that needed a ranking.
- **Identity `(name, version)` with the type as an attribute.** The type decides fit, and an
  attribute is editable; retyping stored bytes to another kind of Agent is what immutable identity
  forecloses.
- **Keep the addon kind, and its byte in the hash, against a future need.** No Client installs an
  addon and no artifact here is one; with nothing deployed the byte buys no stability and leaves
  an unexplained constant in a hash function.
- **Aim on the artifact, with a specificity ranking where several aims match.** Makes "what is
  this" and "who gets it" one record, and "which artifact does this host get" a computation across
  all of them, with ties to refuse, that no operator can read off any one object. Moving the
  ranking to the Deployment keeps the fleet-wide-plus-canary shape and the unreadable computation
  with it; the requirement is a partition.
- **An empty Selector as a catch-all that loses to every other.** A ranking one level deep, makes
  forgetting a field the way to target the whole fleet, and overlaps everything by construction.
- **Inequality in Selectors (`key != value`).** Most expressive; the operator keeps exclusions in
  sync by hand, overlap stops being visible by eye, and it changes the Selector for Configurations
  too. A follow-up if the partition proves too rigid.
- **Naming a Deployment in the per-Agent act resolves the conflict.** Makes the conflict
  permanently liveable; a rule that can be waived per Agent is not a partition.
- **The signature on the Deployment, per `(Package, Platform)`.** It keeps the place where an
  operator releases and the place where an operator signs together. The statement does not name
  the Deployment, so the place adds nothing a Client can verify: anyone who may write to the
  Server can copy a signature from one Deployment to another. It costs re-signing per Deployment
  and integrity in the data: a deleted Package leaves its signatures in every Deployment that held
  it, a held but unassigned Package can have its bytes replaced while the old signature stays, a
  signature can exist for a platform the Package has no entry for, and signature writes have to
  be frozen separately because the re-offer hash does not cover the signature.
- **A Deployment names only the required key.** The Client holds one key, so there is nothing to
  name.
- **Bind the signature to the Deployment by putting its name into the statement.** That would make
  the release decision verifiable. It changes the statement, the signing file, `package sign` and
  every Client, and needs a signer who does not also roll out to be worth it; the specification has
  no such role (Non-Goal "Authorization and multi-tenancy").
- **The Server accepts unsigned Deployments.** An unsigned fleet as a legitimate policy, reported
  rather than refused; it is the convenience the specification ranks below security.
- **Refuse an `invalid` signature on the Server.** Stricter. The Server gives feedback only: the
  Client's check decides, and the Server's key may lag behind the operator's.
- **No signature check on the Server.** A wrong key would be noticed only by the Client that
  refuses the artifact, as a failed install after the rollout instead of a warning before it.
- **`sign --signing-info` demands an independent source**, the artifacts (`--artifacts <dir>`) or
  upstream's checksums file (`--checksums <file>`), and refuses an entry whose hash is in neither.
  The strongest form: what the operator signs is always something they hold or upstream
  published. Not chosen by the decider: it brings back the file, or a second download, for the
  case the signing file exists to make easy. The check stays a documented duty, and this
  alternative is the natural tightening should that duty prove to be skipped.
- **`sign` fetches upstream's checksums itself** for the agents `package fetch` knows. Convenient,
  but it covers only artifacts that travel as published; a repacked artifact has no upstream
  checksum to find, so the tool would need both paths and the rule which applies.
- **The statement itself as the file**, one text file per entry. Any Ed25519 signer could sign it
  without this tool, but five platforms are five files, the `(os, arch)` an import needs is lost,
  and the signatures still need a way back.
- **A `checksums.txt`-style file** (`<sha256>  <name>`). Comparable to upstream's file with `diff`,
  but the Server knows entries by platform, not by file name, and the Agent type and version would
  ride in comments, which is a format nobody can parse strictly.
- **`sign` writes the signatures to the Server** over REST (`--server`, `--cacert`). One step
  fewer, but it puts Operator-plane credentials and reachability on the machine that holds the
  key, which is the machine best kept off the network. The UI import keeps the key's machine
  offline; a CLI path can be added later without changing the file.
- **The signing-file import on the Deployments tab.** The import belongs where the signature
  lives, and the signature lives on the entry, so it belongs to the Package.
- **Keep a reader for older store and record shapes.** A migration with no source is untestable in
  the only way that matters and untrue as documentation.
- **The download route on both listeners.** One resource at two addresses under two policies
  invites protecting one and forgetting the other, and the offer names only one.
- **Leave the download route open to every member.** Membership becomes a key to every build the
  store holds, including builds released to another partition and builds released to no one yet,
  and a compromised host can list the store. The cost of closing it is one lookup per download.
- **Answer `403` for an artifact that exists but is not offered.** The more literal status code,
  but it tells a host which type, version and Platform the store holds, which is the enumeration
  this decision closes. RFC 9110 explicitly allows `404` for this purpose.
- **Bind the fetch to an Agent instead of the host**, by naming an `instance_uid` in the
  `download_url` or in a header. The `instance_uid` is self-asserted, and the only thing the
  Server can check against is the host binding: the same bound with an extra request parameter
  that the Baseline's `DownloadableFile` does not need.
- **Signed, expiring download URLs**, like pre-signed object-store links. Whoever holds the URL can
  fetch, and the URL ends up in logs. The Server would need a signing key and its rotation, and an
  expiry would conflict with a retry after a failed install, which re-reads an offer the hash gate
  no longer re-sends. The handshake already identifies the host.
- **Allow whatever is a candidate for the host's Agents** (what matching would release now). This
  would let a host fetch a build the operator saved but did not release, which breaks the rule that
  a release happens only by an operator's act ([ADR-0026](0026-deployments-and-rollouts.md)).
- **Limit a Gateway to the Agents currently carried on its connections.** The Server would have to
  track which connection or host carries each Agent. That would buy nothing, because a Gateway's
  certificate may report under any `instance_uid` and so can receive any Agent's offer over OpAMP
  anyway.
- **Treat a certificate that names no host as membership alone**, able to fetch anything offered.
  It removes the bound for exactly the certificates the register cannot track.
- **Bind certificates that name no host by their issuer and serial.** A second binding register
  next to the host register, whose entries would have to be carried across certificate
  replacements the Server never sees; the bound would lapse whenever an operator replaces a
  certificate by hand.
- **Check the offer in the `admit_download` middleware.** The middleware would have to parse the
  path and query a second time, and could answer "not offered" for a request the handler would
  have answered `400`. Two parsers could disagree on which artifact is meant.
- **Count `not offered` refusals toward the admission throttle.** That throttle counts a peer
  address, so one misbehaving host would throttle every member behind a shared address. The record
  already holds the refusals, aggregated.
- **Relay the download route through the Gateway, request by request.** The bytes would cross the
  Gateway's link once per Agent, every request would cost a token of its aggregate bucket, and it
  would still need the same downstream bound as the cache.
- **Fetch on the first downstream request instead of when the offer is relayed.** Simpler by one
  trigger, but the artifact would only start to cross the link once an Agent asks, and every Agent
  behind the Gateway would be answered `503` for the whole transfer.
- **Let a downstream request wait for the fetch instead of answering `503`.** The Client's read
  timeout of 60 seconds also bounds the wait for the response's headers. A fetch over a slow link
  outlasts it, the Client reports the download failed and echoes the offer's hash, and the Server
  does not offer it again (clause 50). A `503` with `Retry-After` keeps the Client asking within
  the bound of clause 47.
- **Rewrite the offer's `download_url` to name the Gateway.** The Gateway forwards messages
  unchanged ([ADR-0015](0015-client-modes-and-the-gateway.md)), and a Gateway that edits what the
  Server offers is one more place an offer can be made to say something the Server did not.
- **Serve whatever the Gateway holds to any admitted downstream peer.** Any member behind the
  Gateway could fetch another partition's build once one Agent behind the same Gateway was offered
  it, which the Server's bound closes.
- **Stream an artifact too large for the cache through without storing it.** The Gateway would pass
  on bytes before their hash is checked, and every downstream request for that artifact would
  become an upstream fetch, which the cache exists to prevent. An operator who rolls out such an
  artifact behind a Gateway raises `package_cache_bytes`; the refusal is logged.
- **Count downstream download requests in a bucket per downstream host.** It would add a rate the
  Gateway does not apply on `/v1/opamp` either. A downstream request reaches the Server at most as
  the one shared fetch, so the Server's bucket for the Gateway already bounds what downstream
  requests cost upstream.
- **Keep the cache and its offers on disk across a restart.** It saves one fetch per artifact
  after a restart, but a persisted offer outlives what the Server last said. An emptied directory
  and offers in memory need no reconciliation.
- **A Client-side downgrade refusal alone, the signature left over the bytes.** Keeps every
  existing signature valid, but a signed artifact of one type stays installable as another's
  program.
- **The type and version in a detached manifest, signed beside the artifact.** A second file to
  carry and keep in step; the statement is a few dozen bytes the Client builds from what the offer
  already says.
- **A separate Updater process for Managed-Process packages**, symmetric with the Client's own
  update. The Supervisor already is a distinct process that owns stop, swap, spawn and health
  gate; another process would buy nothing.
- **Content hash only, signatures later.** Authenticity is what verifying a binary means, and
  retrofitting it onto an install path is the costly change; Ed25519 through the `ring` already
  present costs little.
- **Unsigned operation as a posture of the operator's**, accepted on the content hash with a
  warning at startup. The hash arrives in the same offer as the URL, so whoever can make the offer
  (a compromised Server, a stolen operator login) chooses both and runs code on every host it
  reaches. A warning is the convenience the specification puts second.
- **Refuse startup without a verification key.** A Client that only applies configurations would
  have to hold a key it never uses. Not declaring `AcceptsPackages` installs nothing unsigned just
  as surely, and says so at startup.
- **Unpack on the Server and store a bare binary.** One extraction instead of hundreds, no
  unpacking code on hosts. Rejected: the Server would re-hash its own output, so every Agent would
  verify a number the Server invented instead of the one upstream published, and an encrypted
  artifact would have to be decrypted on the Server, key and plaintext on the very machine the
  encryption keeps them from.
- **Support only `.7z`** (one code path, carries a password). Upstream publishes no `.7z`, so no
  release could be used without repacking by hand. **Only `.tar.gz` with some outer encryption**:
  a format someone would have to invent, where AES-256 `.7z` is understood by every tool.
- **Repack an upstream zip into `.tar.gz` or `.7z`** and teach the Client nothing. Rejected: the
  fleet would verify a hash the packing host invented instead of upstream's; one read-only
  container under the existing member rules is cheaper.
- **The archive key in `server.toml`, distributed over OpAMP, or carried as a Configuration.** Each
  puts the key where the Server can open every artifact; as a Configuration it would also be echoed
  back as effective configuration and shown in the fleet view.
- **Addon packages for an agent's supplementary files.** Nothing groups a set of packages, so there
  is no consistent version, no ordering and no atomicity: an Agent could sit with a new executable
  and old shared objects.
- **A `strip_components` key, or stripping a lone top directory automatically.** The first is one
  more number to get right by inspecting an artifact; the second flattens an archive whose top
  level is meaningful and changes behaviour with the archive's contents. Suffix matching needs
  nothing written and fails loudly when ambiguous.
- **Find the program by searching the unpacked tree.** It removes "what will this host run" from
  the configuration: before the first package there is nothing to search.
- **A self-extracting single file.** Re-extracts on every update, hides what is installed, makes
  the reported version the wrapper's, and turns rollback into hope.
- **Unpack over the live `program/` in place.** A failed unpack halfway leaves a tree that is
  neither version and destroys the predecessor the health gate rolls back to.
- **A version-named directory and a `current` pointer** for trees. A directory rename is atomic on
  every platform the Client runs on; a pointer is a symlink on Unix and a junction on Windows,
  machinery that would run on every package. Two fixed names are the single-file swap one level
  up; the version is read from the Agent's report, not from disk.
- **Keep every superseded version.** An unbounded version store on hosts chosen for their disk
  budget; a rollback needs one predecessor.
- **Give up after one failed start.** A transient failure, a briefly held port, would strand a
  rollout; three matches the self-update.
- **Retention on the downloaded artifact instead of the installed predecessor.** A rollback
  restores the installed program, not the download; tying retention to the staged file would
  delete what a fallback needs.
- **A directory per Package.** It is not the layout the store keeps: the filesystem adapter of
  [ADR-0029](0029-the-servers-state-and-its-stores.md) holds one stored file per Package and the
  PostgreSQL adapter one row, so startup checks a stored Package, not a directory (clause 8).
- **An envelope of its own for an Agent record, with a cleared agents directory as the way past a
  version change.** Clearing the directory drops the fleet record an operator relies on. The
  record's envelope is [ADR-0029](0029-the-servers-state-and-its-stores.md)'s, and `server migrate`
  moves a data directory an older Server wrote (clause 32).

## Sources / Prior art

- [OpAMP specification § Packages
  (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md),
  also [§ Packages, Downloadable Packages, Code
  Signing](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#packages):
  the hash-gated sync and the status lifecycle; `PackagesAvailable` is "the packages that are
  available on the Server **for this Agent**", maps a name to one version and hash *for this
  Agent*, carries `signature` per downloadable file, and expects "normally only one top-level
  package"; one downloadable file per package, multiple files to be carried "in any file format
  that allows storing multiple files in a single file"; and what a package contains "is Agent
  type-specific and is outside the concerns of the OpAMP protocol".
- [OpAMP specification
  v0.20.0](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md),
  section *Packages*: "The PackagesAvailable message describes the packages that are available on
  the Server for this Agent"; its URLs "point to package files on a Download Server (which may be
  on the same host as the OpAMP Server or a different host)". Section *Downloading Packages*, step
  3: the Agent uses "an HTTP GET message" with the offered headers. Sections *Packages / Security
  Considerations* and *Security* say nothing about which Agent may fetch which file; this decision
  fills that gap on the Server and changes nothing on the wire.
- The vendored schema `crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`: `PackagesAvailable` is
  the "List of packages that the Server offers to the Agent"; `DownloadableFile` carries
  `download_url`, `content_hash`, `signature` and `headers`.
- The Baseline's `AgentDescription`: "keys/values are according to OpenTelemetry semantic
  conventions", `os.type` SHOULD be included, `service.name` "uniquely identifies the Agent type".
- [OpenTelemetry semantic conventions,
  `host.arch`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/host.md)
  and
  [`os.type`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/os.md):
  the canonical token sets adopted in clause 6.
-
  [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go):
  reports `os.type` from `runtime.GOOS` and `host.arch` from `runtime.GOARCH`.
- [OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md): one
  reference resolves to a list of manifests, each with a required `platform` object, values from
  Go's `GOOS`/`GOARCH`; pushed digests are immutable.
- [Debian repository format](https://wiki.debian.org/DebianRepository/Format) and
  [RPM repositories](https://rpm-software-management.github.io/): a released package is
  `(name, version, architecture)` with per-file checksums. With the OCI Image Index: the name *is*
  what the software is; nothing carries a second identity beside it.
- [`opentelemetry-collector-releases`
  v0.157.0](https://github.com/open-telemetry/opentelemetry-collector-releases/releases/tag/v0.157.0):
  archives only, never a bare binary; `checksums.txt` with a SHA-256 per asset; sigstore keyless
  `.sig`/`.pem` companions.
- [Bindplane, Bring Your Own
  Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector):
  the Agent type as a first-class object reported by the collector, not a free-form tag.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go): its example Server offers no packages,
  so there is no upstream behaviour to copy for where artifacts come from or whom they reach; its
  `PackagesSyncer` is the reference download-verify-report component.
- [Collector Supervisor
  specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md):
  signs the package hash server-side and verifies before applying.
- [WSUS update
  approval](https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/deploy/3-approve-and-deploy-updates-in-wsus):
  approval binds a concrete update to a **computer group**: membership, never exclusion.
- [Jamf Pro patch
  policies](https://learn.jamf.com/r/en-US/jamf-pro-documentation-current/Patch_Policies):
  one version bound to a scope built from groups.
- [Bindplane
  rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts):
  deployment starts on an explicit act and names a pinned version.
- [Argo CD manual sync](https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/): drift
  displayed, nothing applied until Sync; the waiting view extended to "in no Deployment".
- [Kubernetes label
  selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/):
  set-based selectors have `NotIn` and `DoesNotExist`, and grouping still leans on membership
  labels.
- [RFC 9110 section 15.5.4](https://www.rfc-editor.org/rfc/rfc9110#section-15.5.4): "An origin
  server that wishes to 'hide' the current existence of a forbidden target resource MAY instead
  respond with a status code of 404 (Not Found)."
- [nginx `ngx_http_proxy_module`](https://nginx.org/en/docs/http/ngx_http_proxy_module.html),
  `proxy_cache_lock`: "only one request at a time will be allowed to populate a new cache element
  … Other requests of the same cache element will either wait for a response to appear in the
  cache or the cache lock for this element to be released". `proxy_cache_path` `max_size`: when
  the size is exceeded the cache manager "removes the least recently used data". The single-flight
  fetch and the LRU bound of clauses 41 and 45 follow that shape, with offers deciding what is
  evicted first.
- The code read for the download route: `offer_for_assigned`, `assigned_entry` and `to_available`
  in `crates/fleet-server/src/packages.rs`; `packages_offer` and `AgentRecord` in
  `crates/fleet-server/src/fleet.rs`; `check_report` and `Host` in
  `crates/fleet-server/src/revocation.rs`; `download_package` in `crates/fleet-server/src/api.rs`;
  `admit_download` and `Proofs` in `crates/fleet-server/src/transport.rs`; `resolve_url` and
  `send_download` in `crates/fleet-agent/src/packages.rs`; the Gateway's router in
  `crates/fleet-agent/src/gateway/mod.rs`.
- The code read for the Gateway clauses: `Forwarding`, `run_on_timed` and `server_tls` in
  `crates/fleet-agent/src/gateway/mod.rs`; `Registry::deliver` in
  `crates/fleet-agent/src/gateway/registry.rs`; the pool's reader in
  `crates/fleet-agent/src/gateway/pool.rs`; `RevocationList::verdict` in
  `crates/fleet-agent/src/gateway/revocations.rs`; `download_and_verify`, `write_stream` and
  `download_client` in `crates/fleet-agent/src/packages.rs`; `HOST_URI_PREFIX` and `facts` in
  `crates/fleet-server/src/ca.rs`.
- `crates/fleet-core/src/package.rs`, `statement`: the signed bytes name type, version and hash,
  not a Deployment. `crates/fleet-agent/src/archive.rs`: the Client detects raw, `.tar.gz`, `.7z`
  and `.zip` by their leading bytes; the Server never inspects an artifact, so no format field is
  needed.
- [RFC 8032](https://www.rfc-editor.org/rfc/rfc8032), section 5.1.6: Ed25519 signing is
  deterministic, which is why re-signing an entry with the same key is harmless. No external
  format was surveyed for the signing file: it is this project's own exchange between its UI and
  its tool, and its fields are the REST API's.
- [`sevenz-rust2`](https://crates.io/crates/sevenz-rust2): pure-Rust 7z with an `aes256` feature;
  its default features pull `bzip2`, so it is taken with `default-features = false`.
- [`zip`](https://crates.io/crates/zip): MIT; its default features pull C-binding codecs, so it is
  taken with `default-features = false` and `deflate` only, and reading stays on the
  `flate2`/`miniz_oxide` chain.
- [Elastic Agent standalone
  install](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent):
  the `.tar.gz` tree distribution is the one Fleet can upgrade.
- [Datadog Fleet Automation
  upgrades](https://docs.datadoghq.com/agent/fleet_automation/upgrade_agents/):
  two installs side by side "in case a rollback is needed".
- [fluent/fluent-bit#2558](https://github.com/fluent/fluent-bit/pull/2558): `FLB_STATIC_BINARY`,
  an unmerged draft: why a static Fluent Bit is not the answer.
- [RFC 6454, The Web Origin Concept](https://www.rfc-editor.org/rfc/rfc6454): scheme, host and
  port as the unit a download, a redirect hop and an offered header are compared by.
- [RFC 8446, TLS 1.3](https://www.rfc-editor.org/rfc/rfc8446): the one protocol version a download
  is made over beyond the loopback.
-
  [`reqwest::redirect::Policy::custom`](https://docs.rs/reqwest/latest/reqwest/redirect/struct.Policy.html):
  a policy that sees each hop before it is followed, which is where the origin rule applies.
- [`HARDENING.md`](../HARDENING.md) H12, H13, H19 and H23: the fail-open signature check, the
  unbounded source set, the offered header as a credential on someone else's host, and a signature
  that covers the Agent type and the version.

## Consequences

- Positive: a release is one object, five platforms' artifacts under one identity with one
  version. One upload loop per release, no per-host configuration, and the Client's self-update
  works across a heterogeneous fleet.
- Positive: a mismatched binary is refused by construction rather than caught by a health gate; the
  Client's own binary is protected on both sides of the wire.
- Positive: one spelling of a platform from the release file name through the upload, the API, the
  fleet view and what a Collector reports; folding a Managed Process's attributes over the
  Supervisor's cannot change a host's architecture.
- Positive: the Server makes no outbound request for packages and stores no source credential
  (Q-1). The Gateway caches every artifact, so hosts behind a Gateway receive every Package.
- Positive: "what is this artifact" is readable off one object with two fields, and "which artifact
  does this host get" off one Deployment. A collision between two Deployments is **refused and
  named** rather than silently resolved; that is a discipline, not a proof that one Deployment
  matches.
- Positive: signing is done once per artifact, and every Deployment that holds it uses it. No
  signature outlives its entry or Package: new bytes remove the old signature.
- Positive: an operator sees a wrong signature on the Packages tab before a rollout, not as a
  failed install.
- Positive: a Package of any number of platforms is signed with one download, one command and one
  import, and no artifact has to reach the machine that holds the key. The signed file is a record
  of what the operator released (type, version, every hash and its signature) that can be kept
  beside the key, and a signature made this way is byte for byte the one the artifact form makes.
- Positive (Strategy *Security before convenience*, Q-4): a compromised host gets from the store
  only what its own Agents were released, within the limit the partition entry below names. An
  unreleased build cannot be fetched by anyone, and the store's contents cannot be listed by
  probing. A host behind a Gateway receives from it only what the Server offered through it to
  that host's own Agents, and a downstream host cannot list what the Gateway holds.
- Positive (G-10): the offer and the download share one test, so a Client that was offered an
  artifact can always fetch it.
- Positive (Q-1): no setting turns the bound off.
- Positive (G-10, G-15): a Client behind a Gateway receives uploaded artifacts through the Gateway.
  Each artifact crosses the Gateway's upstream link once per Gateway instead of once per Agent,
  and costs the Server one download. A nested Gateway is served by the Gateway in front of it like
  any downstream host.
- Positive: an upstream release, archive and all, is deliverable as published; the SHA-256 from its
  `checksums.txt` and the operator's signature are checked on every host, and a confidential agent
  stays encrypted everywhere but the host that runs it. An upstream zip travels exactly as
  published and is held to the same member rules.
- Positive: nothing reaches a host that the operator's key did not sign, whatever the Server or an
  offer claims, and no archive parser sees bytes that have not passed both checks. An offered
  header and the Client's certificate go to the Server's origin alone.
- Positive: an agent that is an executable plus its libraries is managed like any other: the health
  gate, rollback, retention and version report come with it.
- Positive: neither failure loop can start. A first package that will not run stays installed and
  `InstallFailed`; a pair of broken versions is reported and held; for a day after a successful
  update the previous version is still on disk.
- Negative / trade-offs: every artifact passes through the Server's storage, and disk is unbounded
  by design: every version stays until deleted. A download cannot go through a mirror or another
  host.
- Negative / trade-offs: a mistyped Agent type is a silent no-op; there is no canonicalisation to
  catch it, and the Package simply fits nobody. An Agent's type can change under a stable
  configuration, and with it which Packages fit it; an Agent that reports a different
  `service.name` after it was rolled out to is no longer offered the old type's Package.
- Negative / trade-offs: an Agent reporting no platform or no type gets no Package. No Client this
  project ships is in that position; a foreign OpAMP client may be.
- Negative / trade-offs: **there is no "roll out to everyone".** A fleet-wide delivery needs every
  Agent to carry the same channel value, and a fresh host belongs to no Deployment until labelled
  or provisioned with one.
- Negative / trade-offs: two builds of one Agent type at one version cannot coexist; they must
  differ in type or version. An operator who set `[self_update] package` to something other than
  an Agent type sees that Client refuse the offer on its fleet row.
- Negative / trade-offs: nothing reaches an Agent before it is signed: a trial rollout needs the
  signing key too, and a Platform entry uploaded after a rollout reaches no Agent until it is
  signed. A Package signed once may be released in any Deployment, which matches what the
  statement allows.
- Negative / trade-offs: **what the operator signs from a signing file is what the Server said.** A
  compromised Server, or a file altered between download and signing, can put the hash of an
  artifact the operator never saw in front of the key; an operator who skips the comparison then
  signs it, and every Client accepts it. The guarantee that nothing reaches a host the operator's
  key did not sign still holds to the letter, but its strength rests on the operator's check
  rather than on the operator holding the bytes. Q-1 holds for a fleet whose operator signs from
  the artifacts, which stays available and documented as the stronger path; the signing file
  trades some of it for convenience, by the decider's choice.
- Negative / trade-offs: the signing file is an interface between the UI and `opamp-fleetctl`, and
  the two ship together; a change to it is a new `format` value, never an edit of `v1`.
- Negative / trade-offs: a certificate that names no host fetches nothing; such a host gets `404`
  instead of bytes. The manual (`docs/manual/server.md`) states that without `[client_ca]` a
  hand-provisioned certificate must carry `urn:opamp-fleet:host:<id>` to receive uploaded
  artifacts.
- Negative / trade-offs: a partition is only as strong as the hosts it partitions (Context). A
  partition set by a Server Label is not stronger, because a reported attribute satisfies the same
  Selector. The fleet view shows such an Agent as waiting before the press that assigns it.
- Negative / trade-offs: one lookup under the fleet lock per download. For an ordinary host this is
  a lookup per bound `instance_uid`; for a Gateway it is a scan of the fleet's records, which the
  rate limit of clause 39 bounds. An index from artifact to offering Agents is an option if the
  scan shows up. A Gateway's certificate stays as broad as its mark makes it.
- Negative / trade-offs: the Gateway holds artifacts on disk, up to `package_cache_bytes` (10 GiB
  by default), staged fetches included. A Gateway that restarts holds no offers: an Agent whose
  install is still in flight reconnects and its report re-draws the offer, but an Agent that echoed
  the offer with a failed install and then retries is answered `404` until the Server sends it a
  different offer.
- Negative / trade-offs: a fetch that failed is not tried again until the artifact newly appears in
  an Agent's offer, so a transient error upstream can leave the Agents behind the Gateway without
  it until the next rollout. A `429` or `503` with `Retry-After` is not such a failure. An
  artifact larger than the cache is not delivered behind the Gateway at all. A Gateway whose host
  is not marked delivers no uploaded artifact; its log says that the Server refused the fetch.
- Negative / trade-offs: a Client waits up to 30 minutes, in waits of at most 60 seconds, on a
  `429` or `503` from its own Server origin before it reports a download failed (clause 47).
- Negative / trade-offs: every fleet must create and keep a signing key, and put its public half on
  every host, before it can distribute anything; a fleet that has not is told so at startup and
  takes no packages.
- Negative / trade-offs: every host parses untrusted archives, in three formats. Path sanitizing,
  link refusal and the size and member bounds are the tested boundary, because a tree archive does
  choose where its bytes land.
- Negative / trade-offs: disk on the host: two copies of a program or tree per Supervisor during
  the retention window, and the artifact ceiling and unpack bound sized for agents of hundreds of
  megabytes. Two on-disk layouts, chosen by whether one optional key is set.
- Negative / trade-offs: the archive key is a fleet-wide secret in `supervisor.toml` on every
  host, protected only by the file's permissions; rotating it means every host and every encrypted
  archive. A `.7z` or `.zip` tree carries no helper executables' modes, and a sloppy `.tar.gz`
  produces an agent that does not start.
- Follow-ups: a retention policy for superseded Packages; warning when a type or Platform fits no
  Agent in the fleet; verifying upstream sigstore provenance instead of a pasted checksum;
  inequality in Selectors; refusing an overlapping Selector at write time once a fleet is large
  enough that a conflict is expensive to notice; a Selector term that only a Server Label
  satisfies, so that a partition can hold against the host it partitions; sourcing the archive
  key from an OS keystore or a tighter file; rotating the verification key across a fleet, and a
  second verification key on the Server for it; scoping an offered header to a path; a directory
  mode for the packing tool; range-request resumption for large artifacts; tightening the
  operator's hash comparison into a check (the first signing-file alternative) once the fleet runs
  releases this way. `SECURITY.md` (the trust-boundary section) and the manual's download-route
  rows in `docs/manual/server.md` state the bound, and `docs/manual/client.md` the Gateway's
  cache.

## Enforcement

Each test of the download route and the Gateway's cache (clauses 33–47) carries a `Verifies:`
marker citing this decision ([ADR-0002](0002-decisions-verified-by-tests.md)).

- `crates/fleet-server/tests/mutual_tls.rs`:
  `a_client_certificate_is_required_in_the_handshake_on_the_agent_plane` (clause 34: the download
  on the Agent plane, behind the handshake and outside the credential check).
- `crates/fleet-server/src/packages.rs` unit tests: `a_set_survives_a_reopen`,
  `fit_is_mandatory_platform_and_type`, `both_sides_of_the_comparison_are_canonicalised`,
  `the_offer_carries_a_download_url_naming_the_identity`, `deletion_frees_entries_and_sets`,
  `a_corrupt_artifact_fails_reopen`, `the_store_and_its_metadata_are_owner_only`,
  `identity_tokens_are_bounded`, `only_the_package_its_ring_holds_is_a_candidate`,
  `a_store_in_an_older_layout_refuses_to_open_and_names_what_is_in_the_way`.
- `crates/fleet-core/src/platform.rs`: `folds_the_spellings_this_project_does_not_control`,
  `what_rust_calls_this_machine_folds_onto_a_canonical_token`,
  `an_unknown_token_passes_through_unchanged`.
- `crates/fleet-server/tests/packages.rs`: `an_uploaded_set_is_offered_downloaded_and_gated`,
  `the_artifact_is_served_where_the_agents_are_and_not_on_the_operator_plane`,
  `no_offer_without_the_capability`, `an_entry_needs_its_set_first`,
  `an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact`,
  `an_artifact_past_the_configured_limit_is_refused`, `the_package_store_has_a_total_size_ceiling`,
  `a_set_reaches_only_agents_of_its_type`.
- `crates/fleet-server/src/labels.rs`: `a_label_never_overrides_what_the_agent_reports`.
- `crates/fleet-agent/src/config.rs`: `the_retired_package_keys_are_refused`.
- `crates/fleet-agent/tests/packages_e2e.rs` and `crates/fleet-agent/tests/self_update_e2e.rs` run
  the real Server and Client against one store across the reported Platform.
- `crates/fleet-server/src/deployments.rs`:
  `two_deployments_matching_one_agent_are_a_conflict_that_names_them`,
  `a_narrower_selector_does_not_win_over_a_wider_one`, `an_agent_no_ring_claims_is_not_a_conflict`,
  `a_deployment_must_name_the_ring_it_aims_at`, `a_deployment_holds_one_package_per_agent_type`,
  `the_selector_stays_editable_and_keeps_what_the_ring_holds`.
- `crates/fleet-server/src/fs/deployments.rs`: `a_deployment_survives_a_reopen`,
  `an_unreadable_file_fails_the_open_and_names_it`, `the_store_and_its_files_are_owner_only`.
- `crates/fleet-server/tests/packages.rs`: `a_selector_aims_a_rollout_at_part_of_the_fleet`,
  `a_canary_ring_is_a_selector_aim_and_two_acts`,
  `an_agent_two_rings_claim_is_offered_nothing_and_the_view_says_why`,
  `a_label_aims_a_set_at_part_of_the_fleet`, `a_deployment_without_a_selector_is_refused`,
  `a_deployment_holds_one_uploaded_package_per_agent_type`,
  `a_deployments_aim_is_editable_and_deleting_it_is_its_own_act`,
  `a_conflict_takes_the_candidate_away_and_leaves_the_assignment_standing`,
  `the_per_agent_act_refuses_to_pick_a_side`,
  `a_deployment_that_released_a_package_refuses_to_be_deleted`,
  `a_packages_entry_shows_the_hash_an_agent_verifies_against`,
  `the_fleet_view_tells_no_ring_apart_from_a_ring_with_nothing_for_this_agent`,
  `a_set_says_how_many_agents_it_reaches`.
- `crates/fleet-server/tests/rest_api.rs`: `the_openapi_document_describes_the_contract` (the
  Package and signature routes are in the contract, and no Selector or rollout route on a Package
  is).
- `crates/fleet-server/src/fleet.rs`: `a_record_without_assignments_loads_assigned_to_nothing`.
- The signatures on the entry and the Server's check (clauses 3, 4, 12, 18, 19, 33, 35, 42, 51):
  tests that carry `Verifies:` for this decision show that a signature is written on an entry,
  offered to an Agent of that Platform, and removed when new bytes are uploaded to the entry; that
  a signature write on an assigned Package is `409` and on a missing entry `404`; that one Package
  held by two Deployments is offered with the same signature through both; that with
  `[packages] verification_key` set on the Server a valid signature reports `valid` and a wrong
  one `invalid`, both stored, and without the key the state is `unchecked`; that a malformed
  `verification_key` in `server.toml` fails startup; that the `…/source` route and the Deployment
  signature routes answer `404`; that the offered `download_url` is always a path and a
  `server.toml` naming `advertised_url` fails startup naming the key; that the Client refuses an
  absolute `download_url` on another origin without fetching, and a `supervisor.toml` with
  `[packages] allowed_sources` fails startup naming the key; that the Gateway caches an artifact
  offered as the Server's path; and that the offer and the download agree on every combination of
  capability, assignment, type, Platform and signature.
- The signing file (clauses 20–24): `opamp-fleetctl`'s tests sign a signing file and verify every
  written signature against `fleet_core::package::statement` with the public key, as the Client
  does; they refuse a file whose `format` is not `opamp-fleet-signing-v1`, whose hash is not 64
  lowercase hex characters, or that names a platform twice, and assert that nothing was signed
  then. A Server test reads the bundled UI and asserts that the download writes
  `opamp-fleet-signing-v1` and the fields the API serves, and that the import calls the signature
  route; the OpenAPI document must still carry those fields. These tests carry `Verifies:` for
  this decision.
- The signature at rollout:
  `a_rollout_of_a_deployment_with_an_unsigned_package_is_refused_naming_it` and
  `the_per_agent_act_refuses_a_deployment_with_an_unsigned_package` in
  `crates/fleet-server/tests/packages.rs`.
- `crates/fleet-server/src/packages.rs`:
  - `the_download_and_the_offer_test_one_predicate`: for every combination, the offer and the
    download agree on what is offered (clause 35).
  - `an_agent_reporting_another_type_is_offered_nothing` (clause 35).
- `crates/fleet-server/src/revocation.rs`:
  `a_host_speaks_for_the_agents_bound_to_it_and_a_gateway_for_any` (clause 37).
- `crates/fleet-server/src/fleet.rs`:
  - `an_artifact_is_offered_to_a_host_only_through_an_agent_it_speaks_for` (clauses 35, 37).
  - `an_offer_still_stands_after_its_hash_is_echoed` (clause 36).
  - `a_version_waiting_for_its_press_is_offered_to_no_host` (clause 36).
- `crates/fleet-server/tests/mutual_tls.rs`:
  - `a_host_fetches_the_artifact_offered_to_its_own_agent` (clause 37).
  - `a_host_is_answered_404_for_an_artifact_offered_only_to_another_host` (clause 37).
  - `an_artifact_not_offered_and_one_not_held_are_answered_alike`, which compares status, body and
    headers byte for byte (clause 37).
  - `a_marked_gateway_fetches_what_is_offered_to_any_agent` (clause 41).
  - `a_certificate_naming_no_host_fetches_nothing` (clause 37).
  - `a_refused_fetch_leaves_one_download_refused_entry_naming_its_check`, which also checks that
    the throttle did not count it (clause 38).
  - `a_malformed_token_is_answered_400_before_the_offer_is_tested` (clause 38).
  - `a_download_costs_a_token_of_the_hosts_bucket` (clause 39).
- `crates/fleet-server/src/main.rs`:
  `a_server_without_client_ca_says_uploaded_artifacts_need_a_host`.
- Tests served over `Admission::open`, where no certificate is presented:
  `crates/fleet-agent/tests/packages_e2e.rs`; `crates/fleet-agent/tests/http_transport_e2e.rs`,
  the two Servers it starts.
- `crates/fleet-server/tests/packages.rs`:
  - `an_uploaded_set_is_offered_downloaded_and_gated` and
    `an_artifact_larger_than_the_framework_default_uploads_and_downloads_intact` use a member
    certificate that names the reporting Agent's host, so that they exercise the offered test.
  - `a_version_waiting_for_its_rollout_cannot_be_fetched` (clause 36).
- `crates/fleet-agent/tests/gateway_tls.rs`:
  `the_gateway_serves_no_artifact_it_relayed_no_offer_for`: the Server serves the artifact, and
  the Gateway, which relayed no offer of it, answers `404`.
- `crates/fleet-agent/tests/gateway_package_cache.rs`, against an upstream the test controls:
  - `a_relayed_artifact_is_fetched_once_before_any_request_and_served_to_its_host`: two Agents of
    one host are offered the same artifact, the Gateway fetches it before any request, requests
    while it runs are answered `503` with `Retry-After: 30`, and the upstream served one fetch
    (clause 41).
  - `another_host_and_a_certificate_naming_no_host_are_answered_as_for_an_artifact_not_held`,
    which compares status, headers and body byte for byte.
  - `a_host_reporting_another_hosts_instance_uid_before_the_reply_is_not_served`.
  - `a_later_offer_replaces_what_an_agent_was_offered`.
  - `a_failed_fetch_is_not_repeated_by_requests_or_re_offers_and_is_retried_when_newly_offered`
    (clauses 41, 46).
  - `a_websocket_downstream_receives_the_offer_and_the_artifact` (clause 41).
  - `an_artifact_that_fails_its_hash_is_neither_stored_nor_served` (clause 43).
  - `an_artifact_larger_than_the_cache_is_refused_and_not_fetched_again` and
    `a_body_without_content_length_is_cut_at_the_limit` (clause 45).
  - `the_download_route_answers_503_while_the_gateway_holds_no_revocation_list` and
    `a_revoked_certificate_is_refused_on_the_download_route`.
  - `a_client_behind_a_gateway_installs_from_an_upstream_slower_than_its_read_timeout`: the
    upstream is held until the Client's own download has been answered `503` by the Gateway, and
    its download then completes and verifies (clause 47).
- `crates/fleet-agent/tests/gateway_package_cache.rs`, against the real Server on mutual TLS:
  `a_client_behind_a_marked_gateway_receives_an_uploaded_artifact_through_it`: the Gateway fetches
  with its own certificate what the Server offers an Agent behind it, and the downstream Client's
  own download code (`download_and_verify`) fetches it from the Gateway and verifies its hash and
  signature (clauses 41, 43).
- `crates/fleet-agent/src/gateway/cache.rs`:
  - `room_is_made_from_artifacts_no_longer_offered_first` (clause 45).
  - `only_a_path_on_the_servers_route_is_cached` (clause 42).
  - `requests_while_fetching_are_answered_busy_and_the_upstream_serves_one` (clause 41).
  - `the_cache_is_emptied_at_start_and_owner_only` (clause 45).
  - `refusals_are_logged_five_a_minute_per_host_and_the_rest_counted` (clause 44).
  - `an_evicted_offered_artifact_is_fetched_again_on_a_request_once_per_offer` (clause 46).
  - `a_file_whose_deletion_fails_stays_counted_until_deleted` and
    `a_held_artifact_whose_file_disappeared_is_forgotten` (clause 45).
  - `a_fetch_cannot_reserve_room_other_fetches_hold` (clause 45).
  - `no_more_than_four_fetches_run_at_once` (clause 41).
  - `an_offer_over_a_certificate_naming_no_host_records_nothing_and_fetches_nothing` (clause 41).
  - `an_unfinished_fetch_leaves_no_entry_behind` (clause 41).
  - `a_fetch_below_the_pace_floor_is_cut` (clause 41).
  - `a_deferred_fetch_gives_its_slot_back_while_it_waits` (clause 41).
  - `a_shutdown_ends_a_fetch_that_waits_out_retry_after` (clause 41).
  - `an_artifact_evicted_between_lookup_and_open_is_fetched_again` (clause 45).
- `crates/fleet-agent/src/packages.rs`: `a_download_waits_out_retry_after_from_its_server_origin`,
  `a_download_gives_up_once_its_waits_reach_the_bound`,
  `a_retry_after_of_zero_does_not_loop_without_bound`, `request_time_counts_against_the_bound` and
  `an_unusable_retry_after_is_not_waited_out` (clause 47).
- `crates/fleet-agent/src/transport/mod.rs`: `a_shutdown_stops_a_download_waiting_out_retry_after`
  (clause 47).
- `crates/fleet-agent/src/config.rs`: `the_package_cache_defaults_to_ten_gib_and_rejects_zero`
  (clause 45).
- [`crates/fleet-core/src/package.rs`](../../crates/fleet-core/src/package.rs):
  `the_statement_names_type_version_and_hash` (clause 52).
- [`crates/fleet-agent/src/packages.rs`](../../crates/fleet-agent/src/packages.rs):
  `a_signature_does_not_carry_over_to_another_type_or_version` (clause 52).
- Download and verification: `content_hash_mismatch_is_refused`, `signature_policy_is_enforced`,
  `a_traversing_package_name_is_refused`, `download_refuses_to_stage_a_traversing_name`,
  `the_cap_triggers_only_past_the_limit`, `the_download_source_drops_whatever_authorises_it`,
  `a_download_never_debug_prints_its_header_values` in
  [`packages.rs`](../../crates/fleet-agent/src/packages.rs);
  `a_body_too_large_by_its_content_length_is_refused`,
  `a_chunked_body_is_stopped_once_it_crosses_the_ceiling`,
  `a_download_carries_the_headers_the_offer_named`,
  `an_unusable_offered_header_fails_the_download_by_name`,
  `the_staging_directory_is_kept_owner_only` in
  [`tests/packages_download.rs`](../../crates/fleet-agent/tests/packages_download.rs);
  `the_artifact_size_limit_defaults_is_configurable_and_rejects_zero` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs); end to end,
  `a_signed_package_is_downloaded_verified_swapped_and_reported_installed` in
  [`tests/packages_e2e.rs`](../../crates/fleet-agent/tests/packages_e2e.rs).
- Offer and status: `an_addon_package_is_refused_instead_of_overwriting_the_binary`,
  `a_package_offer_for_the_named_package_is_acknowledged_installing_and_handed_over`,
  `a_package_offer_hands_its_download_headers_to_the_transport`,
  `a_failed_package_reports_installed_failed_and_keeps_the_old_version` in
  [`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs);
  `every_supervisor_declares_package_acceptance` in
  [`supervisor/mod.rs`](../../crates/fleet-agent/src/supervisor/mod.rs);
  `a_slow_download_is_reported_as_downloading_with_progress` in
  [`transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs).
- Containers, single file and tree: in [`archive.rs`](../../crates/fleet-agent/src/archive.rs) the
  detection tests (`detects_gzip_by_its_leading_bytes_and_anything_else_as_raw`,
  `detects_a_7z_by_its_signature`, `detects_a_zip_by_its_signature_and_an_empty_one_too`),
  `an_encrypted_7z_opens_with_the_key_and_not_without_it`,
  `an_escaping_member_path_still_lands_only_where_we_put_it`,
  `a_bomb_ahead_of_the_target_is_refused_before_it_is_skipped`,
  `the_same_program_path_finds_the_program_under_any_wrapper`,
  `members_outside_the_programs_own_directory_are_left_out_and_counted`,
  `a_member_that_climbs_out_refuses_the_archive_before_writing_anything`,
  `an_absolute_member_refuses_the_archive`, `a_link_member_refuses_the_archive`,
  `a_hard_link_member_refuses_the_archive`,
  `a_7z_member_that_is_a_link_or_an_anti_item_refuses_the_archive`,
  `an_archive_of_too_many_members_is_refused`, `a_tree_that_outgrows_the_total_budget_is_refused`,
  `no_match_and_an_ambiguous_match_are_both_refused_by_name`,
  `a_tree_keeps_the_modes_the_archive_carried`, `a_hostile_zip_member_refuses_the_archive`; for
  the zip container (clause 62) further
  `extracts_the_named_member_from_a_zip_wherever_the_archive_keeps_it`,
  `a_zip_tree_lands_whole_with_the_wrapper_directory_dropped` and
  `a_zip_past_the_budget_is_refused`; `a_program_path_must_stay_inside_the_package` and
  `a_tree_spawns_from_the_path_written_inside_the_package` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs).
- Swap, rollback, hold and retention: in
  [`tests/supervisor_process.rs`](../../crates/fleet-agent/tests/supervisor_process.rs)
  `apply_package_swaps_the_binary_and_acknowledges_installed`,
  `a_package_delivered_as_a_tar_gz_is_unpacked_and_installed`,
  `an_install_with_nothing_to_run_yet_keeps_the_binary_and_succeeds`,
  `a_package_that_will_not_stay_up_is_rolled_back_and_fails`,
  `a_first_install_that_will_not_start_is_kept_not_discarded`,
  `a_program_that_keeps_crashing_is_held_not_looped`,
  `a_successful_update_keeps_the_previous_version_for_the_window`,
  `a_tree_package_lands_whole_and_replaces_the_one_before_it`,
  `a_tree_that_will_not_stay_up_is_rolled_back_whole`,
  `a_tree_missing_the_configured_program_is_refused_and_changes_nothing`; in
  [`supervisor/process.rs`](../../crates/fleet-agent/src/supervisor/process.rs)
  `a_retained_backup_is_swept_only_after_its_deadline`,
  `a_sweep_leaves_an_unmarked_backup_alone`, `dropping_a_backup_clears_its_marker`;
  `retention_defaults_globally_and_is_overridable_per_supervisor` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs).
- Mandatory signature (clauses 48, 52): `without_a_verification_key_no_agent_takes_packages` in
  [`supervisor/mod.rs`](../../crates/fleet-agent/src/supervisor/mod.rs), covering the Client's own
  Agent too; `signature_policy_is_enforced` in
  [`packages.rs`](../../crates/fleet-agent/src/packages.rs);
  `an_unsigned_offer_or_an_unkeyed_client_fetches_nothing` in
  [`tests/packages_download.rs`](../../crates/fleet-agent/tests/packages_download.rs).

**Not mechanically decidable:** that the Server never opens an artifact (clause 5) is an absence:
no test can show a code path that does not exist; review of every change touching
`crates/fleet-server/src/packages.rs` and `crates/fleet-server/src/api.rs` keeps it. That every
download on the Agent is TLS 1.3 and that the client certificate reaches only the Server's own
origin are settled by construction in `download_and_verify`, two download clients built on the
Client's TLS settings, the identified one used only for that origin, and no test stands up a TLS
source that inspects the handshake, so review of every change to
[`packages.rs`](../../crates/fleet-agent/src/packages.rs) keeps them. That the Client names the
missing key at startup is a log line in
[`service/runtime.rs`](../../crates/fleet-agent/src/service/runtime.rs) that no test reads. That
the operator compares the hashes before signing (clause 23): the manual states it and the tool
prints what it signs; no test can decide whether a human looked.
