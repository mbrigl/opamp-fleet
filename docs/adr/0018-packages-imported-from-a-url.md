# ADR-0018: A package is an uploaded archive or a URL the Agents fetch — unpacked by the Agent, `.tar.gz` or encrypted `.7z`

- **Status:** 🟡 proposed
- **Date:** 2026-08-05
- **Deciders:** Markus Brigl

## Context

Getting an artifact into the fleet means uploading it. For a real agent that is a 400 MiB file an
operator first downloads from an upstream release page, unpacks, and then pushes through the REST
API — the Server can serve a program of that size without breaking a sweat (ADR-0015), but the human
in front of it is moving the same bytes twice for no reason.

The obvious wish is to point the fleet at the release directly: *"can packages come from a GitHub
repository instead?"* Two things about the current design bear on that.

**The protocol is entirely happy with it.** The Baseline describes the URLs as pointing *"to package
files on a Download Server (which may be on the same host as the OpAMP Server or a different
host)"*, and `DownloadableFile.headers` exists so an Agent can authenticate to one. **This Client
already does it**: `resolve_url` passes an absolute `http(s)://` URL through untouched, and
verification is anchored in the artifact — content hash always, Ed25519 signature when a key is
configured — never in where the bytes came from. What is missing is only that our **Server** can
express it: every `download_url` it offers is built as `{advertised_url}/api/v1/packages/{name}/file`
([`packages.rs`](../../crates/server/src/packages.rs)), so a release asset cannot be named at all.

**An upstream release is an archive, and that is not incidental.** `opentelemetry-collector-releases`
publishes `.tar.gz`, `.deb`, `.rpm`, and `.msi` — never a bare binary. The Client writes what it
downloads over the Managed Process's binary, so pointing it at a release asset would install a
tarball as the program: the process fails to start, the health gate catches it, the binary rolls back
(ADR-0015 works as designed) and nothing has been achieved. **Fetching from a release and unpacking
an archive are one feature, not two.**

Two further properties of real releases shape the decision rather than decorate it:

- Upstream publishes **`checksums.txt`** with a SHA-256 per asset, so the hash an Agent verifies
  against is something an operator can obtain and paste rather than something anyone has to invent.
- Upstream signatures are **sigstore/cosign keyless** — a Fulcio-issued certificate (`.pem`) and a
  Rekor-logged signature (`.sig`), naming the release workflow's OIDC identity. That is a different
  and much heavier verification story than the operator-held Ed25519 key of ADR-0015, and it is not
  something to take on in passing.

And one requirement does not come from upstream at all: **an artifact may be confidential**. A
fleet's own agent — built in-house, not published anywhere — is a program an operator may not want
readable by whoever can reach the distribution point, including the fleet Server's own disk. The
`.7z` format answers that with AES-256 and a password, and it is the format an operator on Windows
reaches for. It is *not* an answer to authenticity: that is what the content hash and the Ed25519
signature already are. Encryption and verification here solve different problems and both are kept.

`.7z` cannot replace `.tar.gz`, though, and the release page says why: `v0.157.0` publishes 44
`.tar.gz`, 21 `.deb`, 21 `.rpm`, 9 `.msi` — and **no `.7z` at all**. A fleet that supported only
`.7z` could not import an upstream release, which is the case this whole ADR exists for.

## Decision

We will let a package be **an uploaded archive or a URL** — and in the URL case the Server stores
the reference, not the bytes: it points the Agents at that URL and never downloads the artifact
itself. Whatever the route, the **Agent** verifies and unpacks, so an archive travels intact from
wherever it was built to the host that runs it.

**The Server never packs, and never encrypts.** Whatever archive a package is, it is finished before
the Server hears of it: built, packed, and — if it is to be confidential — encrypted by whoever
produced it. What the Server is given is the definitive artifact itself, or the address where it
lies; from there it stores or refers, targets, and hands the Agents what they need to fetch it. It
does not create artifacts, does not repack them, and does not open them. Every statement below
follows from that.

Concretely:

1. **Two kinds of package, one control loop.**
   - **Uploaded** — `PUT /api/v1/packages/{name}?version=…` with the artifact as the body, exactly
     as today. The Server stores those bytes and serves them from its own download endpoint.
   - **Referenced** — `PUT /api/v1/packages/{name}/source`, a sub-resource like the Selector, taking
     `url`, `sha256`, and an optional `headers` map for a private source. The Server stores **only
     that reference** and offers it verbatim: the `DownloadableFile` it puts in `PackagesAvailable`
     carries that `download_url`, that `content_hash`, the operator's signature, and those headers.
     The artifact never touches the Server.

   This is what `DownloadableFile` was shaped for — the Baseline's Download Server *"may be on the
   same host as the OpAMP Server or a different host"*, and `headers` exists so an Agent can
   authenticate to one. `version` and the Selector behave identically for both kinds.

   When a source is set the Server **may probe the URL** — a `HEAD`, or a ranged `GET` — purely to
   catch a typo while the operator is still looking at the screen. That is a convenience and is
   described as one: the probe proves nothing about what an Agent will later receive, because the
   content behind a URL can change and only the `sha256` catches that.

2. **The checksum is supplied, and it is the only thing standing between a URL and a host.** The
   Server refuses a source without a `sha256`. For a referenced package it never sees the bytes at
   all, so nothing central can check them: what protects every Agent is the hash the operator
   supplied — taken from the release's own `checksums.txt` — and the Ed25519 signature when one is
   configured. Deriving the hash by fetching once would record *what the Server happened to
   receive*, which is trust-on-first-use with no anchor, and for a referenced package it would not
   even describe what the Agents get.

3. **The Agent unpacks, and the artifact is never repacked on the way.** The Client recognises
   `.tar.gz` and `.7z` by their magic bytes, extracts the member whose file name matches the Managed
   Process's binary, and swaps that in — the existing verify, swap, health-gate, roll-back path
   otherwise unchanged. Anything that is not an archive is installed directly, as today.

   Nothing altering the artifact between its author and the host is what makes this worth the extra
   work on the Agent: the hash an Agent verifies is **the same SHA-256 the artifact was published
   with**, so integrity holds in one unbroken line from wherever it was built to the binary that
   ends up running. Had anything in the middle unpacked it, that thing would have had to re-hash its
   own output, and every Agent would be verifying a number it invented — the original checksum
   checked once, somewhere else, and never again.

4. **`.7z` may be encrypted, and the key that opens it lives on the Agent.** `client.toml` carries
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

5. **Upstream (cosign/sigstore) signatures stay out of scope.** The `sha256` is what is checked, and
   the operator's own Ed25519 signature (ADR-0015) still covers the artifact as stored — which, with
   this decision, is the archive. Verifying a Fulcio certificate and its Rekor inclusion proof is a
   decision of its own, with its own dependency and trust policy.

## Alternatives considered

- **Import instead of reference: have the Server download the URL and serve the bytes itself.** The
  shape this ADR had first, and it is genuinely better in a fleet whose hosts cannot reach the
  internet: one download instead of three hundred, the Server as a cache and as the single reachable
  address, no egress to a third party from every managed machine, and a rollout that does not stop
  when a release page rate-limits. Rejected as the behaviour for a URL because it makes the Server
  carry — and expose — artifacts it has no need to hold: this Server serves package downloads on the
  **unauthenticated** REST plane by design (ADR-0013, ADR-0015), so anything it stores is fetchable
  by anyone who can reach it. An operator who wants the Server in the data path still has one: that
  is exactly what uploading the archive does. Both routes exist, and the choice is per package.
- **Keep upload-only and let the operator script it.** `curl | tar | curl -X PUT` is three commands
  and needs no code. Rejected because it leaves the fleet's software supply chain outside the API
  that goal 5 makes the integration contract: what a portal cannot do, an operator does by hand and
  in a way nothing records.
- **Let the Server derive the checksum by fetching once.** Rejected — see decision 2. It looks the
  most convenient and is worth the least, and for a referenced package it would not even describe
  what an Agent receives.
- **Have the Server verify a referenced artifact by downloading it at set time.** Rejected as
  reassurance rather than a check: it would prove what the URL served at that moment, to that
  requester, and cost a full download to prove it. The probe in decision 1 is honest about being
  only a typo catch; the `sha256` is what actually holds.
- **Adopt sigstore verification now, and skip the checksum.** Rejected for this ADR, not for ever.
  It is the strongest answer — it verifies *who built the artifact*, not merely that it matches a
  string someone pasted — but it brings a substantial dependency and a policy question (which
  identities, which workflow refs, which transparency log) that deserves its own decision rather
  than a sentence in this one.
- **Unpack on the Server, once, and store a bare binary.** The obvious division of labour, and the
  first shape this ADR had: three hundred Agents would not each repeat the same extraction, the
  Client would not change at all, and no unpacking code would ship to every managed host. Rejected
  for two reasons that outweigh it. The Server would have to re-hash its own output, so what every
  Agent verifies would be a number the Server produced rather than the one upstream published — the
  provenance stops at the Server instead of reaching the host. And an encrypted artifact would have
  to be decrypted on the Server to be unpacked, which defeats the point of encrypting it: the
  password, and the plaintext, would live on the very machine the encryption is meant to keep them
  from. The cost of the choice is real and is recorded in the consequences.
- **Support only `.7z`.** Tempting for a single code path, and it is the format that carries a
  password. Rejected on the evidence: upstream publishes no `.7z`, so importing an upstream release
  would mean downloading, unpacking, repacking, and re-hashing by hand — exactly the manual work
  this ADR removes.
- **Support only `.tar.gz`, and encrypt some other way.** Rejected as a worse version of the same
  thing: an encrypted layer around a tarball is a format someone has to invent and document, where
  `.7z` with AES-256 is understood by every operator and every desktop tool already.
- **Put the archive password in `server.toml`.** Rejected. One secret for all artifacts, rotated for
  all at once — and it would put the password on the Server, which decision 4 exists to avoid.
- **Distribute the archive key to Agents over OpAMP.** The protocol has room for it
  (`OtherConnectionSettings.other_settings`, or a `CustomMessage`), and it would end the
  secret-on-every-host problem. Rejected because it removes what the key is for: a Server that
  distributes it can open every artifact — and this Server serves package downloads on the
  **unauthenticated** REST plane by design (ADR-0013, ADR-0015), so a readable artifact there is
  readable by anyone who can reach it. If the Server may hold the key, the simpler answer is to let
  it unpack centrally, not to distribute keys. One variant must be avoided outright: carrying the
  key as a *Configuration*, since a Managed Process echoes its effective configuration back and the
  fleet view renders it in full — the key would be readable in the UI.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — the Download Server *"may be on the same host as the OpAMP Server or a different host"*, and
  *"The protocol supports only a single downloadable file per package. If the Agent's packages
  conceptually are composed of multiple files then the Agent and Server can agree to store the files
  in any file format that allows storing multiple files in a single file, e.g. a zip or tar file"* —
  the protocol's own answer to archives, and the reason unpacking is an implementation choice rather
  than a protocol one.
- [`opentelemetry-collector-releases` v0.157.0](https://github.com/open-telemetry/opentelemetry-collector-releases/releases/tag/v0.157.0)
  — checked directly: assets are `.tar.gz` (44), `.deb` and `.rpm` (21 each), `.msi`, and never a
  bare binary; `opentelemetry-collector-releases_otelcol-contrib_checksums.txt` carries a SHA-256 per
  asset; each asset also has `.sig`/`.pem` companions, which are sigstore keyless signatures (a
  Fulcio certificate naming the release workflow identity, with a Rekor entry) — the evidence behind
  decisions 2 and 5.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — its example Server offers no packages at
  all, so there is no reference behaviour for where artifacts come from; the model is ours to choose.
- [`sevenz-rust2`](https://crates.io/crates/sevenz-rust2) `0.21.4` — checked on crates.io: a pure-Rust
  7z implementation, Apache-2.0 (this project's licence), last released 2026-08-01, with an `aes256`
  feature for password-protected archives built on the RustCrypto `aes`/`cbc` crates. Its default
  features pull `bzip2`, which binds to C — so it must be taken with `default-features = false` and
  only the codecs actually needed, keeping the pure-Rust build chain ADR-0006 and ADR-0007 insist on.
  Its predecessor `sevenz-rust` has not been released since 2024 and is not the one to build on.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) and
  [ADR-0017](0017-selector-targeted-packages.md) — the delivery, verification, and targeting this
  reuses wholesale. This ADR only changes how bytes reach the store.

## Consequences

- Positive: a package becomes a URL an operator already has. Nothing is uploaded, nothing is
  duplicated, and the fleet's software supply chain moves inside the API a portal or a pipeline can
  drive (goal 5).
- Positive: archives are handled at last, so "deliver an upstream Collector release" stops being a
  manual unpack step. Today the same attempt installs a tarball over the binary, fails the health
  gate, and rolls back — safe, and useless.
- Positive: for a referenced package the artifact never exists on the fleet Server, so there is
  nothing there to leak from an unauthenticated download endpoint, and nothing to store. The bytes
  an Agent verifies are the ones the source published, and the SHA-256 from `checksums.txt` is
  checked on every host.
- Negative / trade-offs: **every Agent needs egress to the source.** Today a managed host needs to
  reach the fleet Server and nothing else, which in many fleets is the whole point of the fleet
  Server. A referenced package changes that host's network requirements, and in a closed network it
  simply cannot be used — those fleets upload instead.
- Negative / trade-offs: a rollout across three hundred hosts becomes three hundred downloads from a
  third party, subject to its rate limits and its availability, at the moment an operator most wants
  predictability. Nothing in this design caches or throttles that; the Server cannot, because it
  never has the bytes.
- Negative / trade-offs: `headers` for a private source travel to **every Agent** in the offer.
  A token that opens a private repository is then a fleet-wide secret in flight and at rest on every
  host — the same class of exposure as the archive key, and worth the same care.
- Negative / trade-offs: the Server can no longer answer "what exactly will my Agents install?" for
  a referenced package. It knows a URL and a hash; it has never seen the artifact. A wrong hash, a
  moved release, a revoked token — all of these surface as `InstallFailed` on Agents rather than as
  an error when the operator set the source. The probe softens the most common case (a typo) and
  nothing more.
- Negative / trade-offs: **every Agent now unpacks**, so unpacking code ships to every managed host
  on three platforms, and the Client grows dependencies it did not have: `flate2` and `tar` for
  `.tar.gz`, `sevenz-rust2` (with `default-features = false`, `aes256`) for `.7z`. All pure Rust,
  but they parse untrusted input on the machine that runs the agent — a member path escaping the
  target directory, or an archive that expands without bound, must be refused, and that needs tests
  rather than assumptions.
- Negative / trade-offs: the archive key is a **fleet-wide shared secret on every host**, sitting in
  `client.toml` in the clear, so it is only as protected as that file's permissions. Rotating it
  means touching every host *and* repacking every encrypted archive. Sourcing it from an OS keystore,
  an environment variable, or a file with tighter permissions is a real improvement, left as a
  follow-up rather than pretended away here.
- Follow-ups: distributing the archive key centrally without letting the Server read artifacts — the
  envelope sketched in the alternatives, which needs an Agent keypair. Verifying upstream sigstore
  signatures, which would replace a pasted checksum with real provenance. And a deliberate rollback,
  still a re-upload or a re-pointed source, because the store keeps one version per package name.
