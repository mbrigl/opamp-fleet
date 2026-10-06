# ADR-0015: Package delivery for Managed Processes — verified, unpacked by the Agent, Supervisor-applied as a file or a tree, health-gated, rolled back only to a kept predecessor

- **Status:** 🟢 accepted
- **Date:** 2026-08-13
- **Deciders:** Markus Brigl

## Context

Goal 10 asks the Server to update *"an agent's binary — the Collector's, and the Client's own —
verifying each Package before it is applied, reporting the outcome, and rolling back on failure. A
failed update is reported, not silent."* Package delivery is the software half of the control loop
that configuration targeting, authentication, and credential rotation already serve.

The Baseline's mechanism is a hash-gated sync, the same shape this project already implements twice
(remote config, connection settings):

- The Agent reports `PackageStatuses.server_provided_all_packages_hash`. The Server compares it to
  its own aggregate and, on mismatch, sends `PackagesAvailable` — a map of `PackageAvailable`
  (type, version, `DownloadableFile{download_url, content_hash, signature, headers}`, per-package
  `hash`) plus `all_packages_hash`.
- For each offered package whose hash differs from what it holds, the Agent downloads the file,
  **verifies** it, installs it, and reports `PackageStatus` through the
  `InstallPending → Installing → Installed | InstallFailed` lifecycle — carrying `agent_has_version`
  /`agent_has_hash` and, on failure, an `error_message`.

**The process boundary.** The specification's own vocabulary says: *"A running process cannot
reliably replace its own binary, so this work is handed off across a process boundary"*
(**Updater**). For a **Managed Process** that boundary already exists — the Supervisor is a separate
process that owns its Managed Process's lifecycle (stop, spawn, health-gate via `apply_grace_secs`;
ADR-0011), so the Supervisor *is* the updater for the binary it manages. For the **Client's own**
binary (goal 11) the boundary is a separate Updater over ADR-0010's versioned layout, decided in
[ADR-0017](0017-client-self-update-and-its-consent.md). Those are two different problems.

**Verification.** The Agent MUST verify a downloaded file before installing it; shipping software
distribution without verification would be indefensible and costly to retrofit. `content_hash` is
the integrity check; `signature` is the authenticity check the Baseline's *Code Signing* section
recommends (method *"is Agent specific"*). The project already carries the rustls **ring** provider
(ADR-0007), which verifies Ed25519 — so signature verification needs no new heavy dependency.

**Where the bytes come from.** For a real agent the artifact is a 400 MiB file an operator first
downloads from an upstream release page and then pushes through the REST API — the Server can serve
a program of that size, but the human in front of it moves the same bytes twice for no reason. The
obvious wish is to point the fleet at the release directly. **The protocol is entirely happy with
it.** The Baseline describes the URLs as pointing *"to package files on a Download Server (which may
be on the same host as the OpAMP Server or a different host)"*, and `DownloadableFile.headers` exists
so an Agent can authenticate to one. The Client's `resolve_url` passes an absolute `http(s)://` URL
through untouched, and verification is anchored in the artifact — content hash always, Ed25519
signature when a key is configured — never in where the bytes came from. What the Server needs is a
way to offer a `download_url` other than its own route
([`packages.rs`](../../crates/server/src/packages.rs)).

**An upstream release is an archive, and that is not incidental.** `opentelemetry-collector-releases`
publishes `.tar.gz`, `.deb`, `.rpm`, and `.msi` — never a bare binary. A Client that writes what it
downloads over the Managed Process's binary would install a tarball as the program: the process
fails to start, the health gate catches it, the binary rolls back, and nothing has been achieved.
**Fetching from a release and unpacking an archive are one feature, not two.** Two further
properties of real releases shape the decision:

- Upstream publishes **`checksums.txt`** with a SHA-256 per asset, so the hash an Agent verifies
  against is something an operator can obtain and paste rather than something anyone has to invent.
- Upstream signatures are **sigstore/cosign keyless** — a Fulcio-issued certificate (`.pem`) and a
  Rekor-logged signature (`.sig`), naming the release workflow's OIDC identity. That is a different
  and much heavier verification story than the operator-held Ed25519 key, and it is not something to
  take on in passing.

**An artifact may be confidential.** A fleet's own agent — built in-house, not published anywhere —
is a program an operator may not want readable by whoever can reach the distribution point,
including the fleet Server's own disk. The `.7z` format answers that with AES-256 and a password,
and it is the format an operator on Windows reaches for. It is *not* an answer to authenticity: that
is what the content hash and the Ed25519 signature already are. Encryption and verification solve
different problems and both are kept. `.7z` cannot replace `.tar.gz`, though: `v0.157.0` publishes
44 `.tar.gz`, 21 `.deb`, 21 `.rpm`, 9 `.msi` — and **no `.7z` at all**. A fleet that supported only
`.7z` could not import an upstream release.

**A program may be more than one file.** Extracting exactly one member — the one whose file name
equals the configured program's (`install_executable` in `crates/client/src/supervisor/process.rs`)
— holds for a Go or Rust agent and for the Collector. It excludes an entire class of agent, and
Fluent Bit is the case that makes it concrete: what upstream ships for Linux is a `.deb`/`.rpm`
installing an executable *plus* the shared objects and plugins it loads. Without a tree, the fleet
can configure Fluent Bit centrally, watch its health centrally, and roll its configuration back
centrally — and then needs configuration management to put the binary there in the first place, on
every host, for every version. Building it statically is not available: `FLB_STATIC_BINARY` exists
only as a draft pull request open since 2020 (`fluent/fluent-bit#2558`), unmerged, with unresolved
GPL concerns about static linking, and it drops LuaJIT and SQLDB. Asking an operator to swap the
upstream artifact for someone else's static rebuild in order to be managed is a worse bargain.

Four forces shape the tree:

- **The protocol does not constrain this.** The Baseline is explicit that "the content of the file,
  functionality provided by the packages, how they are stored and used by the Agent side is Agent
  type-specific and is outside the concerns of the OpAMP protocol". A package that unpacks into a
  tree is entirely within it.
- **What a host will run must be readable in its configuration** (ADR-0018), and the program's own
  path stays a bare file name in the Supervisor's directory (ADR-0018 clause 2).
- **A single member never chooses a path**, which is why the single-file case has no traversal
  defence to get wrong: one member, one destination the Client picked. Unpacking a tree gives the
  archive a say in where bytes land, and that is a security boundary this decision creates rather
  than inherits.
- **A failed update must still be undoable.** The single-file rollback is a sibling rename
  (`<binary>.rollback`) — atomic, free, and available because there is exactly one file.

**The rollback lifecycle must not harm the system.** The swap-and-gate lives in
`crates/client/src/supervisor/process.rs` (`InstallTarget` + `Runner::swap_and_gate`): `set_aside`
renames the live program to a `.rollback` sibling, `install` writes the new one, the process is
restarted and gated, and then a predecessor is restored or the result is kept. Operating this against
a real Collector showed three ways it can harm itself:

- **Discarding a failed first install loops.** A first package that installs but cannot survive the
  grace (a port already bound, an empty config, any crash-on-start) is discarded, `program/` goes
  empty, the Server sees `installed hash ≠ desired` and re-offers, the Client re-downloads and
  re-unpacks (hundreds of megabytes each round), it crashes again — indefinitely. Observed live: the
  same 0.157.0 artifact downloaded and unpacked once per second.
- **A rolled-back predecessor that also fails to start loops.** The restored old program is
  respawned by the Runner, which retries **forever** on a capped backoff (`Runner::run`, the
  `exited` arm). If both the new and the old version are broken, the Supervisor spins.
- **Deleting the predecessor the instant the new one is up** leaves no window in which an operator
  (or an automatic later health signal) can fall back to the version that was running an hour ago.

The forces there:

- **"Survives the grace" is a *first* signal, not a *final* one.** A Collector can pass three seconds
  and still be the wrong version — a slow leak, a dropped exporter. Keeping the predecessor briefly
  turns a bad rollout into a one-line fix instead of a re-delivery.
- **A retry that never gives up is a denial of service against the fleet's own Server.** The
  Client's *self*-update rolls back after **three** failed attempts (ADR-0017) rather than
  restarting forever. The Managed-Process path should be no less disciplined.
- **A first install has nothing to roll back to.** Discarding what was just written is not a
  rollback; it is throwing away the only artifact the Server has, which is exactly what makes the
  re-offer loop turn. Leaving it in place — reported `InstallFailed` — stops the churn and keeps the
  bytes that were already verified.
- **Retention is a policy, and policy is not the code's.** How long a superseded version is worth
  keeping depends on the host (disk) and the rollout (risk), so it must be configurable.

## Decision

We will implement OpAMP package delivery **for Managed Processes**: a package is an uploaded archive
or a URL the Agents fetch, verified and unpacked by the Agent, applied by the Supervisor as a single
file or a whole directory tree, health-gated, rolled back only to a real predecessor, never looped
on, and the superseded version retained for a configurable period.

### Delivery and verification

1. **Scope.** Supervisor-backed Agents (ADR-0011) declare `AcceptsPackages` and
   `ReportsPackageStatuses`; every Managed Process is Client-installed, so every such Agent declares
   them (ADR-0018 clause 2). The Client's own update is ADR-0017's, and when the self-Agent declares
   these capabilities is ADR-0017 clause 2's rule (with ADR-0017). One **top-level** package per
   Managed Process — its program, a single file or a tree (clauses 10–15). `Addon` packages,
   download progress details, and HTTP range resumption are recognised on the wire but not acted on
   yet (noted, not silently dropped).

2. **Server** (`OffersPackages`, `AcceptsPackagesStatus`). A **package store**, on the pattern of
   the Configuration store (ADR-0012), holds package artifacts and their metadata (including
   `content_hash` and optional `signature`) under a `packages_dir`, is managed through the OpenAPI
   REST API, and survives restarts; what the store holds and its routes are ADR-0016's. The Server
   computes `all_packages_hash` as the Baseline prescribes — *"an aggregate of all packages names
   and content"* — offers `PackagesAvailable` hash-gated on the reported
   `server_provided_all_packages_hash`, and serves each uploaded artifact at a `download_url` on the
   Agent plane, unauthenticated and outside `Admission` (ADR-0032). What protects an installed
   binary is therefore **verification, not transport secrecy**: the artifact's SHA-256 content hash
   and its Ed25519 signature, both checked before it runs. `OffersPackages` (and
   `AcceptsPackagesStatus`) are declared only while the store is non-empty — an undeclared
   capability is never exercised.

3. **Client / Supervisor** (`AcceptsPackages`, `ReportsPackageStatuses`). On an offer whose hash
   differs from the installed package: report `Installing`, **download** the file, **verify** it —
   `content_hash` (SHA-256) always, and the Ed25519 `signature` against an operator-configured
   public key when one is present (a signed package offered without a configured key, or a bad
   signature, is `InstallFailed`, never installed) — then hand the verified artifact to the
   Supervisor, which **applies** it exactly as it applies a configuration: stop the Managed Process,
   swap the staged program over the target in `<supervisor_dir>/<name>/program/` (ADR-0018 clause 1;
   the previous program kept for rollback), spawn, and **health-gate** on `apply_grace_secs` —
   surviving the grace is `Installed`; exiting within it is `InstallFailed` and handled as clauses
   16 and 17 state. Before the swap the staged program is run once as a preflight (ADR-0033). The
   installed package (path, version, hash) persists in the Supervisor's state dir, so a restarted
   Client reports the version it runs and is not re-offered it. `PackageStatuses` is hash-gated and
   follows the `InstallPending → Installing → Installed | InstallFailed` lifecycle, mirroring the
   `APPLYING → APPLIED | FAILED` config path and the connection-settings status.

4. **A Port command.** `ProcessCommand::ApplyPackage { staged_path, version, hash }` extends the
   Supervisor Port (ADR-0011) beside `ApplyConfig`; its `PackageApplied` event closes the lifecycle,
   exactly as `ConfigApplied` closes configuration. No new process is spawned — the Supervisor is
   the process boundary the Updater vocabulary requires. How a kind performs the install step behind
   `ApplyPackage` is its plugin's, inside this contract (ADR-0011).

The operational story: an operator uploads a new Collector binary (with its Ed25519 signature) to
the Server through the REST API, or points the Server at the release; the Server offers it to the
matching Agents; each Supervisor downloads, verifies, unpacks, swaps, and health-gates it, rolling
back a program that will not stay up; the outcome is visible per Agent in the fleet view.

### An uploaded archive or a URL, unpacked by the Agent

In the URL case the Server stores the reference, not the bytes: it points the Agents at that URL and
never downloads the artifact itself. Whatever the route, the **Agent** verifies and unpacks, so an
archive travels intact from wherever it was built to the host that runs it.

**The Server never packs, and never encrypts.** Whatever archive a package is, it is finished before
the Server hears of it: built, packed, and — if it is to be confidential — encrypted by whoever
produced it. What the Server is given is the definitive artifact itself, or the address where it
lies; from there it stores or refers, targets, and hands the Agents what they need to fetch it. It
does not create artifacts, does not repack them, and does not open them. Clauses 5–9 follow from
that.

5. **Two kinds of entry, one control loop.** A package entry (ADR-0016 clause 9) is either:
   - **Uploaded** — the artifact as the request body. The Server stores those bytes and serves them
     from its own download route (clause 2).
   - **Referenced** — a source taking `url`, `sha256`, and an optional `headers` map for a private
     source. The Server stores **only that reference** and offers it verbatim: the
     `DownloadableFile` it puts in `PackagesAvailable` carries that `download_url`, that
     `content_hash`, the operator's signature, and those headers. The artifact never touches the
     Server.

   The routes that write either kind are ADR-0016 clause 12's. This is what `DownloadableFile` was
   shaped for — the Baseline's Download Server *"may be on the same host as the OpAMP Server or a
   different host"*, and `headers` exists so an Agent can authenticate to one. Version and Selector
   belong to the package, not the entry, and behave identically for both kinds.

   When a source is set the Server **may probe the URL** — a `HEAD`, or a ranged `GET` — purely to
   catch a typo while the operator is still looking at the screen. That is a convenience and is
   described as one: the probe proves nothing about what an Agent will later receive, because the
   content behind a URL can change and only the `sha256` catches that.

6. **The checksum is supplied, and it is the only thing standing between a URL and a host.** The
   Server refuses a source without a `sha256`. For a referenced package it never sees the bytes at
   all, so nothing central can check them: what protects every Agent is the hash the operator
   supplied — taken from the release's own `checksums.txt` — and the Ed25519 signature when one is
   configured. Deriving the hash by fetching once would record *what the Server happened to
   receive*, which is trust-on-first-use with no anchor, and for a referenced package it would not
   even describe what the Agents get.

7. **The Agent unpacks, and the artifact is never repacked on the way.** The Client recognises
   `.tar.gz` and `.7z` by their magic bytes (ADR-0031 adds `.zip` as a third container), extracts
   the member whose file name matches the Managed Process's binary — or every member, for a tree
   (clause 10) — and swaps that in, through the verify, swap, health-gate, roll-back path of
   clauses 3 and 16–18. Anything that is not an archive is installed directly.

   Nothing altering the artifact between its author and the host is what makes this worth the extra
   work on the Agent: the hash an Agent verifies is **the same SHA-256 the artifact was published
   with**, so integrity holds in one unbroken line from wherever it was built to the binary that
   ends up running. Had anything in the middle unpacked it, that thing would have had to re-hash its
   own output, and every Agent would be verifying a number it invented — the original checksum
   checked once, somewhere else, and never again.

8. **`.7z` may be encrypted, and the key that opens it lives on the Agent.** `client.toml` carries
   `[packages] archive_key`; the Client uses it to open an encrypted archive, and the Server never
   learns it. That is the point: an artifact whose confidentiality matters is readable only on the
   host that runs it — encrypted in transit, encrypted wherever it is stored, and encrypted on the
   fleet Server's disk in the case where the Server holds it at all.

   The key is one secret for the fleet — a single archive serves every Agent, so every Agent opens
   it with the same key. One thing must not be reused for it: the OpAMP credential from `[auth]`,
   which ADR-0014 has the Server rotate fleet-wide on its own. A rotation would leave every packed
   archive unopenable, with no error until the next install.

   What this buys, plainly: an artifact that anyone able to reach the Server could otherwise fetch
   and read stays unreadable without the key. What it does not buy: protection from someone who can
   read `client.toml`. There the file's permissions are the protection, as they are for every other
   secret an agent holds.

   Both formats are supported. `.7z` is for artifacts an operator packs; `.tar.gz` is what upstream
   publishes, and dropping it would mean no upstream release could be used at all.

9. **Upstream (cosign/sigstore) signatures stay out of scope.** The `sha256` is what is checked, and
   the operator's own Ed25519 signature (clause 3) still covers the artifact as stored — which is
   the archive. Verifying a Fulcio certificate and its Rekor inclusion proof is a decision of its
   own, with its own dependency and trust policy.

### A package may be a directory tree

A tree is unpacked whole **beside the one it replaces** and swapped in by renaming directories — the
same move the single-file path makes with `<binary>.rollback`, one level up. The principle is the
one [ADR-0010](0010-client-os-service-and-installation-layout.md) uses for the Client's own versions and
[ADR-0017](0017-client-self-update-and-its-consent.md) trusts for replacing a running program: build the new one
somewhere else entirely, switch by a single atomic operation, and keep what ran until the new one
has proved itself.

10. **A `[[supervisor]]` block gains one optional key, `program_path`** — a relative path *inside*
    the package, e.g. `bin/fluent-bit`. Absent, the layout is one member, one file. Present,
    **every member of the archive is extracted**, each keeping its own relative path, and
    `program_path` says which of them is the program.

    `binary`/`command` keeps its meaning untouched: a bare file name in this Supervisor's own
    directory (ADR-0018); an absolute path is refused (ADR-0018 clause 2). `program_path` says
    *where inside* the delivered tree the program is. Tree mode is triggered by that key rather
    than by what the archive happens to hold, so what a host will run is readable in the
    configuration before any artifact exists. A wrapped kind states its own `program_path`
    instead of reading it from the block (ADR-0037 clauses 1 and 2).

11. **`program_path` matches a member by its trailing path components, not from the archive root.**
    A release tarball almost always wraps everything in one version-named directory —
    `fluent-bit-3.1.0/bin/fluent-bit` — and a `program_path` that had to name it would be wrong at
    the next release, silently, which is the failure ADR-0018 exists to make unspellable. So
    `program_path = "bin/fluent-bit"` matches any member whose path *ends* with those components,
    exactly as the single-member rule (clause 7) matches a file name "wherever the archive keeps
    it". More than one match is refused, naming the candidates, and answered by writing more of the
    path.

12. **The tree is unpacked to `<supervisor_dir>/<name>/program/tree/`**, keeping whatever directory
    structure the archive has below the stripped prefix, and the tree it replaced is kept beside it
    as `program/tree.rollback`. The Managed Process is spawned from `program/tree/<program_path>` —
    a path that follows from the configuration alone, so it is known at startup, before any package
    has ever arrived. The new tree is built in `program/.staging` and moved into place by a single
    rename, so the live name is either the old tree or the new one and never a mixture; a failed
    install renames the previous one back.

    Two fixed names rather than a version-named directory and a `current` pointer: a directory
    rename is atomic on every platform this Client runs on, while a pointer is a symlink on Unix and
    a junction on Windows — machinery ADR-0010 runs once, as an Administrator, at install time, and
    which would here run on every package. It is also the move the single-file swap already makes
    (`<binary>.rollback`), so there is one mechanism to understand rather than two. What is lost is
    the version being legible on disk; the Agent reports it, which is where an operator reads it
    anyway.

13. **The archive's paths are sanitized, and every member is bounded.** A member is refused, and the
    whole install with it, when its path is absolute, contains a `..` component, or is a symlink or
    hard link. Extraction is bounded by a total byte count and a member count, in addition to the
    per-member limit. Refusing the install is the only correct answer: a partially unpacked agent is
    worse than none.

14. **File modes come from the archive on Unix, plus `program_path` is always made executable.** A
    tree carries its own modes and a `tar` preserves them; the one thing that must not depend on how
    the archive was built is whether the program can be executed at all.

    A `.7z` is the exception: it stores Windows attributes, and a Unix mode survives in them only by
    a convention this Client will not bet an agent's executability on. A tree packed as `.7z` gets
    its program made executable and nothing else, so an agent that ships helper executables beside
    its program is a reason to use `.tar.gz` — the format upstream releases use anyway.

15. **Nothing changes for a single-file package.** No `program_path`, no tree, no `tree.rollback` —
    the single-file path stays exactly as it is, including its own rollback. The tree is a second
    shape; it does not migrate the first.

### Rollback, no restart loop, and retention

16. **Rollback needs a predecessor.** A failed apply with a `.rollback` present restores it. A failed
    apply with **no** predecessor performs **no rollback**: the just-installed (and
    content-verified) program is **left in place**, the package is reported `InstallFailed` with the
    reason, and nothing is discarded. `program/` is never emptied by a failure, so the "installed ≠
    desired, re-offer, re-download" loop cannot start.

17. **A failed apply is terminal for that package hash — no restart loop.** After a failed apply
    (whether it rolled back or not), the Supervisor does **not** respawn the Managed Process in a
    tight loop. It reports the failure as health and `InstallFailed` and then **waits** for a state
    change — a new configuration, a different package hash, or an operator restart — rather than
    retrying the same broken artifact. In particular a rolled-back predecessor that then also fails
    to stay up is reported unhealthy and **not** restarted again. The Server's own gate (it does not
    re-offer a hash the Agent reported `InstallFailed`) is the matching half; this clause is the
    Client half. The give-up threshold mirrors the self-update's **three** attempts (ADR-0017) so
    the two update paths behave alike.

18. **A superseded version is kept for a grace period, then deleted.** On a **successful** apply the
    predecessor is **not** deleted immediately. The `.rollback` is retained and a persisted marker
    records the deadline (`applied_at + retain_previous`); a cleanup pass on startup and on a
    periodic tick deletes any `.rollback` past its deadline. The period is `[updates]
    retain_previous_secs` (default **86400**, one day); who may override it is ADR-0037 clause 5's
    rule. A subsequent update within the window supersedes the marker (each Supervisor keeps at most
    one predecessor — the immediately previous version). `0` deletes the predecessor on success.

    The persisted marker lives in the Supervisor's own directory (ADR-0010/0021), so the deadline
    survives a Client restart the way the self-update outcome marker does. Deletion is best-effort
    and logged; a marker whose `.rollback` is already gone is simply cleared.

## Alternatives considered

- **Implementing the Client self-update (goal 11) in the same decision** — it needs the separate
  Updater process ADR-0010's layout was built for (a process cannot replace its own running
  binary), plus service-restart survival and version pruning. That is a distinct, harder decision
  (ADR-0017); bundling it would double the surface.
- **A separate Updater process for Managed-Process packages too** — symmetric with self-update, but
  redundant: the Supervisor is already a distinct process from its Managed Process and already owns
  the stop/swap/spawn/health-gate it would delegate. Spawning another process buys nothing and
  violates simplicity-first.
- **Content-hash only, signatures later** — tempting for scope, but authenticity is exactly what
  "verifying each Package before it is applied" means for a binary, and retrofitting verification
  onto an install path is precisely the costly-to-reverse change ADRs exist to prevent. Ed25519 via
  the already-present ring provider keeps the cost low, so signatures ship (optional per the
  Baseline, enforced when a key is configured).
- **Authenticating the download route** — the Client presents neither credential nor certificate
  when downloading, and a `download_url` may point at a mirror; ADR-0032 keeps the route
  unauthenticated for that reason. The Ed25519 signature is the load-bearing protection against a
  substituted binary (a MITM cannot forge it without the operator's private key), so an
  unauthenticated download of a *signed* artifact is defensible.
- **Import instead of reference: have the Server download the URL and serve the bytes itself.**
  Genuinely better in a fleet whose hosts cannot reach the internet: one download instead of three
  hundred, the Server as a cache and as the single reachable address, no egress to a third party
  from every managed machine, and a rollout that does not stop when a release page rate-limits.
  Rejected as the behaviour for a URL because it makes the Server carry — and expose — artifacts it
  has no need to hold: this Server serves package downloads on the **unauthenticated** Agent plane
  by design (ADR-0032), so anything it stores is fetchable by anyone who can reach it. An operator
  who wants the Server in the data path still has one: that is exactly what uploading the archive
  does. Both routes exist, and the choice is per package.
- **Keep upload-only and let the operator script it.** `curl | tar | curl -X PUT` is three commands
  and needs no code. Rejected because it leaves the fleet's software supply chain outside the API
  that goal 5 makes the integration contract: what a portal cannot do, an operator does by hand and
  in a way nothing records.
- **Let the Server derive the checksum by fetching once.** Rejected — see clause 6. It looks the
  most convenient and is worth the least, and for a referenced package it would not even describe
  what an Agent receives.
- **Have the Server verify a referenced artifact by downloading it at set time.** Rejected as
  reassurance rather than a check: it would prove what the URL served at that moment, to that
  requester, and cost a full download to prove it. The probe in clause 5 is honest about being
  only a typo catch; the `sha256` is what actually holds.
- **Adopt sigstore verification now, and skip the checksum.** Rejected for now, not for ever. It is
  the strongest answer — it verifies *who built the artifact*, not merely that it matches a string
  someone pasted — but it brings a substantial dependency and a policy question (which identities,
  which workflow refs, which transparency log) that deserves its own decision.
- **Unpack on the Server, once, and store a bare binary.** The obvious division of labour: three
  hundred Agents would not each repeat the same extraction, the Client would not need unpacking,
  and no unpacking code would ship to every managed host. Rejected for two reasons that outweigh
  it. The Server would have to re-hash its own output, so what every Agent verifies would be a
  number the Server produced rather than the one upstream published — the provenance stops at the
  Server instead of reaching the host. And an encrypted artifact would have to be decrypted on the
  Server to be unpacked, which defeats the point of encrypting it: the password, and the plaintext,
  would live on the very machine the encryption is meant to keep them from. The cost of the choice
  is real and is recorded in the consequences.
- **Support only `.7z`.** Tempting for a single code path, and it is the format that carries a
  password. Rejected on the evidence: upstream publishes no `.7z`, so importing an upstream release
  would mean downloading, unpacking, repacking, and re-hashing by hand — exactly the manual work
  this decision removes.
- **Support only `.tar.gz`, and encrypt some other way.** Rejected as a worse version of the same
  thing: an encrypted layer around a tarball is a format someone has to invent and document, where
  `.7z` with AES-256 is understood by every operator and every desktop tool already.
- **Put the archive password in `server.toml`.** Rejected. One secret for all artifacts, rotated for
  all at once — and it would put the password on the Server, which clause 8 exists to avoid.
- **Distribute the archive key to Agents over OpAMP.** The protocol has room for it
  (`OtherConnectionSettings.other_settings`, or a `CustomMessage`), and it would end the
  secret-on-every-host problem. Rejected because it removes what the key is for: a Server that
  distributes it can open every artifact — and this Server serves package downloads on the
  **unauthenticated** Agent plane by design (ADR-0032), so a readable artifact there is readable by
  anyone who can reach it. If the Server may hold the key, the simpler answer is to let it unpack
  centrally, not to distribute keys. One variant must be avoided outright: carrying the key as a
  *Configuration*, since a Managed Process echoes its effective configuration back and the fleet
  view renders it in full — the key would be readable in the UI.
- **Use the protocol's Addon packages for the supplementary files** — top-level for the executable,
  addons for the rest. It is the mechanism's own vocabulary, and it is the wrong shape here: nothing
  expresses that a set of packages belongs together, so there is no version they are consistent at,
  no ordering, and no atomicity — an Agent could sit with a new executable and old shared objects,
  which is precisely the state that will not start. ADR-0016 also targets *one* top-level package
  per Agent by Selector; addons would need a second targeting model to answer "which addons, at
  which version, for this Agent".
- **A `strip_components` key**, as `tar` has, to drop the release tarball's leading directory.
  Stable across versions — it describes how a publisher builds archives, not which version — and one
  more number an operator has to get right by inspecting an artifact first. Suffix matching needs
  nothing written down and fails loudly when it is ambiguous, which is the better trade for a value
  nobody can verify until a rollout runs.
- **Strip a leading directory automatically** when every member shares one. It reads as the obvious
  convenience and is wrong on an archive whose top level is *meaningful* — one holding only `lib/`
  would be flattened into the root, and the failure would be a program that starts and cannot find
  its libraries. Behaviour that changes with the archive's contents is exactly what should not sit
  under a rollout.
- **Find the program by searching the unpacked tree**, with no `program_path` at all — the natural
  extension of the match-by-file-name rule. It removes a key and it removes the answer to "what will
  this host run" from the configuration file: before the first package there is nothing to search,
  so the spawn path would exist only after an install, discovered rather than written. ADR-0018 put
  that value in the file on purpose.
- **A self-extracting single file** — a script carrying an embedded payload, installed as the
  program by the single-file mechanism, unpacking itself on first run. It needs no change at all,
  which is its whole appeal. It also re-extracts on every update, hides what is installed from
  everything that inspects the host, makes the reported version a property of the wrapper rather
  than the agent, and turns a rollback into "run the old wrapper again and hope". A supported
  mechanism should not be an unsupported one wearing a costume.
- **Unpack the tree over the existing `program/` directory in place**, with no version directory.
  Smaller diff, and it destroys the only copy of the working agent at the moment it is most needed:
  a failed unpack halfway through leaves a tree that is neither version. The rollback the health
  gate depends on would stop being available exactly when it fires.
- **Let the operator name a subdirectory in `binary`/`command`** (`bin/fluent-bit`) instead of
  adding a key. Rejected by ADR-0018 on their own terms: anything that is not a bare
  file name is refused precisely so the program's place is unambiguous, and quietly admitting
  another form would make a fleet-visible capability depend on parsing rather than on shape.
- **Leave it, and let configuration management install multi-file agents.** Defensible — but it
  splits the fleet into agents this project can manage and agents it can only watch, and the split
  follows how an agent happens to be linked rather than anything an operator chose.
- **Delete the predecessor immediately, retry forever, discard on first install.** Rejected: each
  of the three behaviours was observed to harm a running deployment — an unbounded re-download
  loop, a spin between two broken versions, and no rollback window at all.
- **Keep every superseded version, not just the last.** Rejected: it turns `program/` into an
  unbounded version store on a host chosen for its disk budget, and the Managed-Process rollback is
  a *one-step* safety net, not the versioned layout the Client's own self-update keeps (ADR-0010).
  One predecessor is what a rollback needs.
- **Give up after one failed apply rather than three.** Rejected for consistency: the self-update
  path settled on three (ADR-0017), and a single transient failure (a briefly-held port) should not
  permanently strand a rollout.
- **Retention as time-to-live on the artifact store rather than the `.rollback`.** Rejected: the
  predecessor a rollback needs is the *installed* program, not the downloaded artifact; tying
  retention to the staged download would delete the very thing a fallback restores.

## Sources / Prior art

- [OpAMP specification — Packages / Downloadable Packages and Code Signing](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#packages)
  — the hash-gated sync, the `InstallPending/Installing/Installed/InstallFailed` lifecycle, and the
  code-signing recommendation; packages are opaque to the protocol: "The content of the file,
  functionality provided by the packages, how they are stored and used by the Agent side is Agent
  type-specific and is outside the concerns of the OpAMP protocol." `PackagesAvailable`,
  `PackageAvailable`, `DownloadableFile`, `PackageStatuses`, `PackageStatus`,
  `PackageType_TopLevel`/`PackageType_Addon` in the pinned Baseline proto
  (`crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`).
- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — the Download Server *"may be on the same host as the OpAMP Server or a different host"*, and
  *"The protocol supports only a single downloadable file per package. If the Agent's packages
  conceptually are composed of multiple files then the Agent and Server can agree to store the files
  in any file format that allows storing multiple files in a single file, e.g. a zip or tar file"* —
  the protocol's own answer to archives, and the reason unpacking is an implementation choice rather
  than a protocol one.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) `PackagesSyncer` — the reference
  download-and-report component: compare `all_packages_hash`, per-package hash check, download,
  verify, set `server_offered_version`/`server_offered_hash`, report status. Its example Server
  offers no packages at all, so there is no reference behaviour for where artifacts come from; the
  model is ours to choose.
- [Collector Supervisor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — signs the package hash server-side and verifies before applying, the model adopted here.
- [`opentelemetry-collector-releases` v0.157.0](https://github.com/open-telemetry/opentelemetry-collector-releases/releases/tag/v0.157.0)
  — checked directly: assets are `.tar.gz` (44), `.deb` and `.rpm` (21 each), `.msi`, and never a
  bare binary; `opentelemetry-collector-releases_otelcol-contrib_checksums.txt` carries a SHA-256 per
  asset; each asset also has `.sig`/`.pem` companions, which are sigstore keyless signatures (a
  Fulcio certificate naming the release workflow identity, with a Rekor entry) — the evidence behind
  clauses 6 and 9.
- [`sevenz-rust2`](https://crates.io/crates/sevenz-rust2) `0.21.4` — checked on crates.io: a pure-Rust
  7z implementation, Apache-2.0 (this project's licence), last released 2026-08-01, with an `aes256`
  feature for password-protected archives built on the RustCrypto `aes`/`cbc` crates. Its default
  features pull `bzip2`, which binds to C — so it must be taken with `default-features = false` and
  only the codecs actually needed, keeping the pure-Rust build chain ADR-0006 and ADR-0007 insist on.
  Its predecessor `sevenz-rust` has not been released since 2024 and is not the one to build on.
- **Elastic Agent** distributes a `.tar.gz` holding a directory tree and states that the archive
  distributions — not the system packages — are the ones Fleet can upgrade: the same conclusion
  reached here, that remote lifecycle management wants a self-contained tree.
  <https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent>
- **Datadog Fleet Automation** keeps two installs side by side under `/opt/datadog-packages` "in
  case a rollback is needed" — the versioned-directory-plus-pointer shape, in a fleet product, for
  the same reason.
  <https://docs.datadoghq.com/agent/fleet_automation/upgrade_agents/>
- **Fluent Bit static linking** — `FLB_STATIC_BINARY` is an unmerged draft (open since 2020) with
  GPL concerns raised by a maintainer, and incompatible with `FLB_LUAJIT` and `FLB_SQLDB`. The
  reason "just ship it statically" is not an answer for this agent.
  <https://github.com/fluent/fluent-bit/pull/2558>
- This project's own [ADR-0010](0010-client-os-service-and-installation-layout.md) install layout (`versions/`,
  `current`, side-by-side, pointer move) and [ADR-0017](0017-client-self-update-and-its-consent.md)'s use of it to
  replace a running program (staged version, three-attempt give-up, marker across restart) — the
  mechanism trusted for the harder case of the Client replacing itself, the precedent the rollback
  rules align to, and the contrast that justifies keeping only *one* predecessor here rather than a
  full version store.
- ADR-0011 — the Supervisor Port and the `apply_grace_secs` health gate reused as the package health
  gate; ADR-0012 — the store + REST + hash-gating pattern the package store follows;
  [ADR-0016](0016-a-package-is-a-versioned-set.md) — the targeting reused for both entry kinds;
  ADR-0032 — the Agent plane the download is served on; ADR-0007 — the ring provider that verifies
  Ed25519.

## Consequences

- Positive: goal 10 holds for Managed Processes — the Server updates a Collector's binary
  fleet-wide, every artifact hash- and signature-verified before it runs, health-gated on the same
  grace that gates a configuration, rolled back when it will not stay up, and reported per Agent.
  Four matrix rows (`AcceptsPackages`, `ReportsPackageStatuses`, `OffersPackages`,
  `AcceptsPackagesStatus`) are implemented; the package store follows the ADR-0012 pattern and the
  health gate reuses ADR-0011, so little new machinery is invented.
- Positive: a package can be a URL an operator already has. Nothing is uploaded, nothing is
  duplicated, and the fleet's software supply chain moves inside the API a portal or a pipeline can
  drive (goal 5).
- Positive: archives are handled, so "deliver an upstream Collector release" needs no manual unpack
  step.
- Positive: for a referenced package the artifact never exists on the fleet Server, so there is
  nothing there to leak from an unauthenticated download route, and nothing to store. The bytes an
  Agent verifies are the ones the source published, and the SHA-256 from `checksums.txt` is checked
  on every host.
- Positive: the class of agent that is an executable plus its libraries is not second-class. An
  operator installs Fluent Bit the way they install anything else in the fleet — upload the
  artifact, aim its Selector — and the rollback, health gate, and version reporting come with it.
  The artifact stays the one upstream published, which is what clause 7 exists to protect. A `.deb`
  still is not openable, but the `.tar.gz` many projects publish alongside it is — wrapper directory
  and all, since `program_path` is written against the part of the path that does not change between
  releases.
- Positive: the two loops end. A first package that will not start stays installed and
  `InstallFailed` instead of triggering an endless re-download; a pair of broken versions is
  reported, not spun on. Token, bandwidth, and disk churn against the Server stop.
- Positive: a rollout gains a real fallback window — for `retain_previous_secs` after a successful
  update, the previous version is still on disk and an operator can put it back.
- Positive: the Managed-Process update path and the Client self-update path behave alike
  (three-attempt give-up, marker across restart), which is one rule to reason about instead of two.
- Negative / trade-offs: addon packages, download-progress reporting, and range-request resumption
  are on the wire but inert; the Server stores uploaded binary artifacts, growing its disk footprint
  and making it a distribution point that must be secured like one (the existing auth and TLS
  apply).
- Negative / trade-offs: **every Agent needs egress to the source** of a referenced package. A
  managed host otherwise needs to reach the fleet Server and nothing else, which in many fleets is
  the whole point of the fleet Server. A referenced package changes that host's network
  requirements, and in a closed network it simply cannot be used — those fleets upload instead.
- Negative / trade-offs: a rollout of a referenced package across three hundred hosts becomes three
  hundred downloads from a third party, subject to its rate limits and its availability, at the
  moment an operator most wants predictability. Nothing in this design caches or throttles that;
  the Server cannot, because it never has the bytes.
- Negative / trade-offs: `headers` for a private source travel to **every Agent** in the offer.
  A token that opens a private repository is then a fleet-wide secret in flight and at rest on every
  host — the same class of exposure as the archive key, and worth the same care.
- Negative / trade-offs: the Server cannot answer "what exactly will my Agents install?" for a
  referenced package. It knows a URL and a hash; it has never seen the artifact. A wrong hash, a
  moved release, a revoked token — all of these surface as `InstallFailed` on Agents rather than as
  an error when the operator set the source. The probe softens the most common case (a typo) and
  nothing more.
- Negative / trade-offs: **every Agent unpacks**, so unpacking code ships to every managed host on
  three platforms, and the Client carries dependencies for it: `flate2` and `tar` for `.tar.gz`,
  `sevenz-rust2` (with `default-features = false`, `aes256`) for `.7z`. All pure Rust, but they
  parse untrusted input on the machine that runs the agent.
- Negative / trade-offs: **the archive gains a say in where bytes land** for a tree. The single-file
  case's "no traversal to defend against" property does not hold for a tree, and is traded for a
  sanitizer that has to be right — absolute paths, `..`, symlinks, hard links, and an archive that
  expands without bound all become refusals that must be tested rather than assumed. This is the
  real cost of the tree and the part most worth reviewing.
- Negative / trade-offs: the archive key is a **fleet-wide shared secret on every host**, sitting in
  `client.toml` in the clear, so it is only as protected as that file's permissions. Rotating it
  means touching every host *and* repacking every encrypted archive. Sourcing it from an OS keystore,
  an environment variable, or a file with tighter permissions is a real improvement, left as a
  follow-up rather than pretended away here.
- Negative / trade-offs: disk. Two unpacked trees per Supervisor for a tree package, not two files —
  an agent with a few hundred megabytes of plugins doubles, on hosts where `supervisor_dir` was
  already the reason ADR-0018 made the location movable. A superseded version occupies disk for up
  to a day by default (one extra program or tree per Supervisor); `retain_previous_secs = 0` exists
  for hosts that cannot spare it. A persisted marker and a periodic cleanup tick are moving parts on
  the Managed-Process side.
- Negative / trade-offs: a second layout to explain. `program/<file>` and
  `program/tree/<program_path>` coexist, and which one a Supervisor has depends on whether
  `program_path` is set.
- Negative / trade-offs: mode and ownership semantics come partly from the archive, so an artifact
  built with sloppy modes produces an agent that does not start — a failure whose cause is in the
  archive rather than on the host.
- Follow-ups: download-progress details and range-request resumption for large artifacts; addon
  packages when a plugin needs them; key distribution/rotation for signature verification.
  Distributing the archive key centrally without letting the Server read artifacts — an envelope
  that needs an Agent keypair. Verifying upstream sigstore signatures, which would replace a pasted
  checksum with real provenance. Whether `opamp-package-sign pack` should grow a directory mode, so
  the tool that builds artifacts can build a tree too. Whether the reported `service.version`
  should be re-probed after an install, since a tree's version is even less likely to match what
  was probed at startup. A regression test per rollback behaviour (no-predecessor keeps the binary
  and does not loop; a twice-failing pair is reported and not respawned; a retained predecessor is
  deleted only after its deadline and survives a restart until then).
