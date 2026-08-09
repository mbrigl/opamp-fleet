# ADR-0017: A package's artifacts — an uploaded archive or a URL the Agents fetch, unpacked by the Agent, `.tar.gz` or encrypted `.7z`

- **Status:** 🟢 accepted
- **Date:** 2026-08-05
- **Deciders:** Markus Brigl

The platform part of this decision supersedes the **artifact-naming** decision of
[ADR-0021](0021-release-pipeline-and-artifacts.md) — the `<os>` and `<arch>` tokens in the release
file name and the spelling of them in its target table. Everything else that ADR decides — the
`version/*` trigger, which five targets are built and on which runners, the `.7z` container written
by this project's own packer, the single job that reads the version, and what a release publishes —
is untouched and still binding. It extends [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) rather
than replacing it: Selector aiming and its specificity rule survive unchanged, with
two mandatory fit steps in front of them — the Agent type, then the platform. The type fit builds
directly on
[ADR-0023](0023-what-describes-an-agent.md), which is what first
gave every Agent a type worth matching.

## Context

### Where a package's bytes come from

Getting an artifact into the fleet means uploading it. For a real agent that is a 400 MiB file an
operator first downloads from an upstream release page, unpacks, and then pushes through the REST
API — the Server can serve a program of that size without breaking a sweat (ADR-0015), but the human
in front of it is moving the same bytes twice for no reason.

The obvious wish is to point the fleet at the release directly: *"can packages come from a GitHub
repository instead?"* Two things about an upload-only design bear on that.

**The protocol is entirely happy with it.** The Baseline describes the URLs as pointing *"to package
files on a Download Server (which may be on the same host as the OpAMP Server or a different
host)"*, and `DownloadableFile.headers` exists so an Agent can authenticate to one. **This Client
already does it**: `resolve_url` passes an absolute `http(s)://` URL through untouched, and
verification is anchored in the artifact — content hash always, Ed25519 signature when a key is
configured — never in where the bytes came from. What upload-only lacks is only that our **Server**
can express it: every `download_url` it offers is built as `{advertised_url}/api/v1/packages/{name}/file`
([`packages.rs`](../../crates/server/src/packages.rs)), so a release asset cannot be named at all.

**An upstream release is an archive, and that is not incidental.** `opentelemetry-collector-releases`
publishes `.tar.gz`, `.deb`, `.rpm`, and `.msi` — never a bare binary. A Client that writes what it
downloads over the Managed Process's binary would install a tarball as the program when pointed at a
release asset: the process fails to start, the health gate catches it, the binary rolls back
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
`.7z` could not import an upstream release, which is the case the archive formats exist for.

### One artifact per platform

ADR-0016 moved the decision *which artifact an Agent gets* from a file on every host to the Server,
and pointed at the heterogeneous fleet as the case it solves: "`host.arch` and `os.type` are already
reported, so one package per platform, each with a Selector, updates every machine from the Server".
That sentence alone is not a platform story, and it does not hold up.

**A package name carrying exactly one artifact.** A store that is a `BTreeMap<String, Package>` keyed
by name, persisted as `<name>.json` and `<name>.bin`, makes "one package per platform" mean one
*name* per platform — `otelcol-linux-amd64`, `otelcol-darwin-arm64`, and so on — and a Selector on
each that repeats, as an equality pair, what the name already says.

**For the Client's own update that shape does not work at all.** `[self_update] package` names
**one** package, and the Client refuses any offer under a different name
([`agent.rs:637`](../../crates/client/src/supervisor/agent.rs#L637)) — deliberately, because that
name is the only thing standing between a fleet-wide Collector artifact and every Client binary in
the fleet (ADR-0018). Meanwhile ADR-0021 publishes **five** artifacts per release. So an operator has
exactly two options, and both are bad: put one platform's build in the fleet under
`opamp-fleet-client` — which is what the release notes show
([`release.yml:238`](../../.github/workflows/release.yml#L238)), and what makes uploading the second
platform's build silently overwrite the first for the whole fleet — or give each platform its own
package name and write the matching name into `client.toml` **on every host**. The second is the
per-host wiring ADR-0016 exists to remove, reintroduced at the one place where the blast radius is
the Client itself.

**Without a fit, nothing stops a mismatched binary.** If a package's Selector is the only filter, an
empty Selector reaches every Agent that accepts packages. An operator who uploads a Windows artifact
and forgets the Selector has it downloaded, verified, unpacked and swapped over the binary on every
Linux host in the fleet. The Client's health gate catches it — the process will not stay up and is
rolled back (ADR-0015) — but that is a fleet-wide outage window, discovered per host, for a mistake
the Server had every attribute in hand to refuse. The Selector *can* express the platform, but it is
opt-in, and the failure mode of forgetting it is the worst one this system has.

**And nothing in this project spells a platform the same way twice.** Three vocabularies meet here:

| Where | Linux / macOS / Windows | 64-bit Intel | 64-bit ARM |
|---|---|---|---|
| What the Client reports ([`agent.rs:796`](../../crates/client/src/supervisor/agent.rs#L796)) | `linux` / `darwin` / `windows` | `x86_64` (Rust's `std::env::consts::ARCH`) | `aarch64` |
| Semantic conventions, and what `opampextension` reports (`runtime.GOOS`/`GOARCH`) | `linux` / `darwin` / `windows` | `amd64` | `arm64` |
| ADR-0021 release file names | `linux` / `macos` / `windows` | `x86_64` | `aarch64` |

The operating system is very nearly settled — `os.type` is what the Baseline names
([`opamp.proto:716`](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L716): "the following
attributes SHOULD be included: `os.type`, `os.version`"), our Client already maps Rust's `macos` onto
the convention's `darwin`, and the Collector agrees; only the release file name says `macos`.
The architecture is not settled at all. The Client passes Rust's constant through where the
convention says `amd64`, and that deviation is not academic: a Managed Process's reported attributes
are folded over the Supervisor's
([`agent.rs:826`](../../crates/client/src/supervisor/agent.rs#L826)), so a Collector whose
`opampextension` reports `host.arch=amd64` **overwrites** the Supervisor's `x86_64`. The same
machine reports a different architecture depending on what happens to run on it, and a Selector
written against one spelling stops matching.

So the question is not only how a package learns its platform. It is which words this project uses
for one, everywhere, at once.

### The Agent type a package is built for

Platform fit (point 8) closes one half of "is this artifact meant for this Agent": a
variant whose Platform is not the Agent's is dropped before anything else is considered, because a
binary that cannot run on the machine it is sent to is not a targeting mistake to be resolved by
precedence — it is not a candidate. The Selector *can* express the platform but is opt-in, and the
failure mode of forgetting it is the worst one this system has.

**Without a type fit, the other half is exactly where the platform was.** A package's Agent type
would be expressible only as a Selector pair, and a Selector starts empty and matches everything
([`packages.rs:139`](../../crates/server/src/packages.rs#L139)). Offer resolution filtering on aim
and platform and nothing else
([`resolve()`](../../crates/server/src/packages.rs#L1127-L1138)) looks like this:

```rust
let Some(platform) = Platform::reported(description) else { return Ok(Vec::new()) };
let fitting = packages.values()
    .filter(|p| matches(&p.selector, description))   // aim — opt-in
    .filter_map(|p| p.variants.get(&platform)...)    // fit — mandatory
```

So an operator who uploads a Promtail artifact for the right platform and forgets the Selector has
it downloaded, verified, unpacked and swapped over the **Collector's** binary on every consenting
host. The health gate catches it (ADR-0015) — the process will not stay up and is rolled back — but
that is the fleet-wide outage window the platform fit refuses to accept, still available for the
role. The argument that settles it for the platform settles it here: a rule that is only ever right
when remembered is not the rule this needs.

**A type fit needs something to match against.** This is why the platform fit alone cannot do both
at once. When `service.name` carried the *instance* name, different on every host and destroyed by
any Collector that reported its own type, "the Agent type" was not a fact the Server could read.
ADR-0023 made it one: `service.name` is the type, resolved from what the Managed Process reports,
else the block's `service_name`, else the program's file name — so every Agent this Client presents
reports a type, always.

**And the Client's own binary is protected by exactly one thing on the host.** ADR-0018 makes
`[self_update] package` the whole of the protection, because "a package with an empty Selector
reaches every consenting Agent, so without a name to match, the first fleet-wide Collector artifact
someone uploads would be installed over the Client and take the host out of reach". That check lives
on the host being protected and compares a *package name* — an operator who names a Collector
package `opamp-fleet-client` defeats it. A Server-side type fit is an independent second guard on the
one case where the blast radius is the Client itself.

## Decision

### Uploaded or referenced, unpacked by the Agent

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
   - **Uploaded** — `PUT /api/v1/packages/{name}?version=…` with the artifact as the body. The
     Server stores those bytes and serves them from its own download endpoint.
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
   Process's binary, and swaps that in — the verify, swap, health-gate, roll-back path otherwise
   unchanged. Anything that is not an archive is installed directly.

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

### One platform vocabulary — a package is one name with one artifact per platform

We will make a **Platform** — an operating system and an architecture, spelled the way the semantic
conventions spell them — a property of a package artifact, required wherever an artifact is written,
and make the Server offer each Agent **only** the artifact that fits the platform it reports. The
same two tokens name the platform in the release file name, in the upload, in the API, and in what
the Agent reports.

A **Package** is a name plus one or more **Variants**: one artifact per Platform, under one name.

6. **The store is keyed by `(name, os, arch)`.** A variant carries everything that belongs to bytes —
   version, type, content hash, signature, size, source, and its own one-step history (ADR-0016).
   The Selector belongs to the **name**, shared by every variant of it: **the Selector aims, the
   Platform fits.** On disk a variant is `<name>@<os>-<arch>.json` / `.bin` / `.previous.bin`; the
   ADR-0010 name grammar admits neither `@` nor `_`, so the file name parses back unambiguously.

7. **Platform is required wherever an artifact is written or served, and nowhere else.** The rule is
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

8. **Fit before aim, and fit is not optional.** Offer resolution gains a first step that cannot be
   switched off: every variant whose Platform is not the Agent's reported platform is dropped before
   anything else is considered. Only then does ADR-0016's aiming run over what is left — most
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

9. **The name on the wire is still the name.** `PackagesAvailable` maps the package *name* to the
   fitting variant, so two Agents on different platforms are offered the same name and different
   bytes. `all_packages_hash` is already computed per Agent over its matching set (ADR-0016), which
   is exactly the right granularity for this. **The Client's package handling needs no change**: one
   `[self_update] package = "opamp-fleet-client"` works unmodified on all five release targets, and
   the name check that protects the binary keeps protecting it.

10. **One vocabulary, and it is the semantic conventions'.** The canonical Platform is an `os.type`
   value (`linux`, `darwin`, `windows`, …) and a `host.arch` value (`amd64`, `arm64`, …). Everything
   this project writes or shows uses those two tokens.

   A fixed table canonicalises **both** sides before they are compared — the uploaded Platform and
   the reported attributes alike — so older and foreign spellings keep working: `macos`, `osx` →
   `darwin`; `win`, `win32`, `win64` → `windows`; `x86_64`, `x64`, `x86-64` → `amd64`; `aarch64` →
   `arm64`. Anything else is lower-cased and passed through, so a platform this table has never heard
   of is still serviceable without a code change; a canonical token must match `[a-z0-9_]{1,16}`,
   which is what keeps it a safe file-name component. The canonical pair is what the API answers
   with, so a typo is visible in the response rather than only in a rollout that never happens.

   The table is compatibility, not translation. With points 11 and 12 below,
   nothing this project produces needs it.

11. **The Client reports the convention it already claims to follow.** `host.arch` is `amd64` /
   `arm64` rather than Rust's `x86_64` / `aarch64`
   ([`agent.rs:797`](../../crates/client/src/supervisor/agent.rs#L797)), the same one-line mapping
   `os.type` carries for `macos` → `darwin`. The Supervisor and a Collector's `opampextension` then
   report the same string, so folding a Managed Process's attributes over the Supervisor's cannot
   change a host's architecture.

12. **The release artifacts carry those same two tokens.** `opamp-fleet-client-<version>-<os>-<arch>.7z`,
   with `<os>` and `<arch>` spelled as in point 10 — replacing the `<os>`/`<arch>` columns
   of ADR-0021's target table and the name in its clause 4. The five targets, their runners, and
   everything else about the release are ADR-0021's and stand:

   | target | runner | `<os>` | `<arch>` | artifact |
   |---|---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `linux` | `amd64` | `…-linux-amd64.7z` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `arm64` | `…-linux-arm64.7z` |
   | `aarch64-apple-darwin` | `macos-latest` | `darwin` | `arm64` | `…-darwin-arm64.7z` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `darwin` | `amd64` | `…-darwin-amd64.7z` |
   | `x86_64-pc-windows-msvc` | `windows-latest` | `windows` | `amd64` | `…-windows-amd64.7z` |

   The file name is then literally the platform pair the host reports, which is what makes the upload
   mechanical: the release notes publish the loop that uploads **all five** under one package name,
   with `os` and `arch` taken straight out of each file name. ADR-0021 called artifact names a public
   contract, and they are — which is why this changes them once, together with everything else that
   names a platform, rather than leaving `macos`/`x86_64` as the last two words in the project that
   mean something no Agent ever says.

13. **A stored package without a Platform is refused at startup.** There is no "matches every
   platform" state: the store fails to open, naming the file and saying to re-upload it with `os` and
   `arch` or delete it. A silent "any" would mean the guarantee in point 8 holds for new
   packages and not for old ones, which is not a guarantee an operator can rely on. Configuration
   errors are fatal and named in this project (ADR-0008); this is the same rule applied to state.

### A package states the Agent type it is built for, and reaches no Agent of another

We will make an **Agent type** a property of a package, and offer a package only to Agents whose
reported `service.name` equals it.

14. **The type belongs to the name, not to the bytes.** `Package.service_name`, shared by every
   variant, exactly as the Selector is — a type is platform-independent, and a value repeated on
   each of five artifact uploads is five chances for them to disagree. Point 7 states
   the rule this follows: "a request naming *bytes* names the Platform they are for; a request aiming
   the *package* does not."

   It is set through its own sub-resource, `PUT /api/v1/packages/{name}/type`, beside the Selector's.
   On disk it joins `<name>.json`, the file that already holds what belongs to the name.

15. **Fit before aim, in two steps, neither optional.** Offer resolution drops every package whose
   `service_name` is not the Agent's reported `service.name`, then every variant whose Platform is
   not the Agent's, and only then runs ADR-0016's aiming over what is left. Type first because it is
   the cheaper comparison and the coarser cut.

   The type is compared **raw**, with no canonicalisation table. The Platform can be canonicalised
   (point 10) because the semantic conventions enumerate operating systems and
   architectures; there is no canonical set of Agent types and inventing one would mean this Server
   having an opinion about every collector distribution that exists. The value an Agent reports is
   the value to write.

16. **A package with no type set is offered to nobody**, and the package view says so. This is
   deliberately *not* the platform's "refused at startup" (point 13): there, the unset
   state is dangerous, because a stored package without a Platform would have to mean either "every
   platform" (the hole) or nothing at all. Here the unset state is already the safe one — no type, no
   offer — so refusing to boot would buy no guarantee and cost every operator an outage to migrate.
   Fail closed, stay up, and say why.

   The guarantee is therefore absolute without the store ever being rejected: **a package is offered
   only to an Agent of its type.** An untyped package is inert, not permissive.

17. **An Agent that reports no `service.name` fits nothing**, the same rule point
   8 applies to a missing platform, and for the same reason: "unknown type, so anything
   goes" would put the mismatched-binary failure straight back. Every Client this project ships
   reports a type since ADR-0023; a foreign OpAMP client that does not is told so on its fleet row
   rather than left with a rollout that never starts.

18. **The Client's self-update gains a second, independent guard.** The Server will not offer a
   package typed `otelcol-contrib` to an Agent reporting `opamp-fleet-client`, whatever it is named,
   and the ADR-0018 name check still runs on the host. Neither replaces the other: one is the
   Server refusing to send, the other the Client refusing to install.

## Alternatives considered

### Where the bytes come from, and who unpacks them

- **Import instead of reference: have the Server download the URL and serve the bytes itself.**
  Genuinely better in a fleet whose hosts cannot reach the internet: one download instead of three
  hundred, the Server as a cache and as the single reachable address, no egress to a third party
  from every managed machine, and a rollout that does not stop when a release page rate-limits.
  Rejected as the behaviour for a URL because it makes the Server carry — and expose — artifacts it
  has no need to hold: this Server serves package downloads on the **unauthenticated** REST plane by
  design (ADR-0013, ADR-0015), so anything it stores is fetchable by anyone who can reach it. An
  operator who wants the Server in the data path still has one: that is exactly what uploading the
  archive does. Both routes exist, and the choice is per package.
- **Keep upload-only and let the operator script it.** `curl | tar | curl -X PUT` is three commands
  and needs no code. Rejected because it leaves the fleet's software supply chain outside the API
  that goal 5 makes the integration contract: what a portal cannot do, an operator does by hand and
  in a way nothing records.
- **Let the Server derive the checksum by fetching once.** Rejected — see point 2. It
  looks the most convenient and is worth the least, and for a referenced package it would not even
  describe what an Agent receives.
- **Have the Server verify a referenced artifact by downloading it at set time.** Rejected as
  reassurance rather than a check: it would prove what the URL served at that moment, to that
  requester, and cost a full download to prove it. The probe in point 1 is honest about
  being only a typo catch; the `sha256` is what actually holds.
- **Adopt sigstore verification now, and skip the checksum.** Rejected for this decision, not for
  ever. It is the strongest answer — it verifies *who built the artifact*, not merely that it matches
  a string someone pasted — but it brings a substantial dependency and a policy question (which
  identities, which workflow refs, which transparency log) that deserves its own decision rather
  than a sentence in this one.
- **Unpack on the Server, once, and store a bare binary.** The obvious division of labour: three
  hundred Agents would not each repeat the same extraction, the Client would not change at all, and
  no unpacking code would ship to every managed host. Rejected for two reasons that outweigh it. The
  Server would have to re-hash its own output, so what every Agent verifies would be a number the
  Server produced rather than the one upstream published — the provenance stops at the Server instead
  of reaching the host. And an encrypted artifact would have to be decrypted on the Server to be
  unpacked, which defeats the point of encrypting it: the password, and the plaintext, would live on
  the very machine the encryption is meant to keep them from. The cost of the choice is real and is
  recorded in the consequences.
- **Support only `.7z`.** Tempting for a single code path, and it is the format that carries a
  password. Rejected on the evidence: upstream publishes no `.7z`, so importing an upstream release
  would mean downloading, unpacking, repacking, and re-hashing by hand — exactly the manual work
  this decision removes.
- **Support only `.tar.gz`, and encrypt some other way.** Rejected as a worse version of the same
  thing: an encrypted layer around a tarball is a format someone has to invent and document, where
  `.7z` with AES-256 is understood by every operator and every desktop tool already.
- **Put the archive password in `server.toml`.** Rejected. One secret for all artifacts, rotated for
  all at once — and it would put the password on the Server, which point 4 exists to
  avoid.
- **Distribute the archive key to Agents over OpAMP.** The protocol has room for it
  (`OtherConnectionSettings.other_settings`, or a `CustomMessage`), and it would end the
  secret-on-every-host problem. Rejected because it removes what the key is for: a Server that
  distributes it can open every artifact — and this Server serves package downloads on the
  **unauthenticated** REST plane by design (ADR-0013, ADR-0015), so a readable artifact there is
  readable by anyone who can reach it. If the Server may hold the key, the simpler answer is to let
  it unpack centrally, not to distribute keys. One variant must be avoided outright: carrying the
  key as a *Configuration*, since a Managed Process echoes its effective configuration back and the
  fleet view renders it in full — the key would be readable in the UI.

### Platform

- **Platform as pure metadata on separate package names.** Keep the store keyed by name, add `os` and
  `arch` as fields that hard-filter the offer. Much smaller: no store rework, no per-variant history,
  no download-URL change. Rejected because it leaves the Client self-update exactly where it is —
  five artifacts still need five names, and `[self_update] package` still has to name the right one
  per host. It solves the mismatched-binary problem and not the problem that motivated the question.
- **Leave it to the Selector, and only document it.** A Selector of `{"os.type": "linux",
  "host.arch": "amd64"}` already does the filtering. Rejected: it is opt-in, and the cost of
  forgetting it is a bricked agent on every host of every other platform. It also cannot express the
  Client case at all. A rule that is only ever right when remembered is not the rule this needs.
- **Optional Platform, empty meaning "every platform".** Rejected as point 13 states: it
  is the backward-compatible reading of "filtered", and it makes the filter a property of how
  carefully a package was uploaded rather than of the Server.
- **Encode the platform in the version string** (`3.0.0+linux-amd64`). Rejected. It needs no schema
  change at all, which is its only virtue: ADR-0009 decides that build metadata is provenance and is
  *not* compared, so putting a load-bearing selector there contradicts an accepted ADR, and the
  Client's `self-check` compares the version it is offered against what the binary reports.
- **Canonicalise onto what the Client happens to report** (`x86_64`, `aarch64`), leaving the Client
  untouched. The smaller change: the fleet view and the package view would agree, and only the
  release file name would need translating. Rejected once the file names move: it would make this
  project's canonical spelling of an architecture one that neither the semantic conventions, nor the
  Collector, nor the release artifact uses — a private vocabulary maintained by a table, forever. The
  convention is already what two of the three worlds speak.
- **Keep the release file names as ADR-0021 wrote them** (`macos`, `x86_64`) and let the alias table
  absorb them at upload. Rejected. It works, and it leaves an operator reading `macos-x86_64` off a
  download page, `darwin`/`amd64` in the fleet view, and having to know these are the same machine.
  The table exists for spellings this project does not control; using it to paper over its own is how
  the divergence became load-bearing in the first place.
- **Reject an unknown `os`/`arch` at upload against a closed list.** Rejected. It catches a typo, but
  the list would have to enumerate every platform any Agent in any fleet might report, and being
  wrong about that means an operator cannot serve a platform the Server has no opinion about. The
  canonical pair in the response and the stated refusal on the Agent's fleet row cover the typo at a
  much lower price.
- **One rollback per name rather than per variant.** Rejected on the canary case described in point
  7: it reverts platforms that never took the rollout.

### Agent type

- **Leave it to the Selector and document it.** `{"service.name": "otelcol-contrib"}` already does
  the filtering, at zero cost. Rejected on the platform's own argument: it is opt-in, and forgetting
  it bricks an agent on every host of every other type. This is the same alternative rejected above
  for the platform, and nothing about the role makes the argument weaker — if anything the blast
  radius is larger, since a wrong-platform binary fails to exec while a wrong-role binary may start,
  run, and quietly collect nothing.
- **Default the type to the package name.** A package named `otelcol-contrib` would be for Agents of
  that type unless overridden, which needs no new input at all for the shape operators already use
  and would have made this change invisible. Rejected: it is right most of the time and silently
  wrong for `otelcol-canary`, `collector-3.1.0-rc`, or any name chosen for the rollout rather than
  the target — and a mechanism whose entire purpose is "not optional" cannot rest on a guess that is
  usually correct. The structurally identical "optional Platform, empty meaning every platform" is
  rejected above.
- **Require the type as a query parameter on the artifact upload**, as point 7 does for
  `os`/`arch`. Fewer requests, no window in which a package exists untyped. Rejected on point
  14: five platform uploads under one name would state the type five times and could
  disagree, and resolving that disagreement (last writer wins? refuse a mismatch?) is a rule this
  project would rather not have. The Selector lives off the bytes route for precisely this reason;
  the type is the Selector's kind of thing, not the artifact's.
- **Refuse an untyped package at startup**, mirroring point 13 exactly. The consistent
  choice. Rejected once the asymmetry is clear: an untyped package here is already inert, so the
  startup refusal protects nothing that point 16 does not, and it costs a Server-down
  migration on every deployment — the second one for package fit. Consistency of mechanism is worth
  less than not taking a fleet offline for a state that is safe.
- **Match the type as a prefix or a pattern** (`otelcol*` covering `otelcol` and `otelcol-contrib`).
  Rejected: those are genuinely different binaries with different components, which is the whole
  reason the distinction exists, and a matching language is a decision that outlives its convenience.
  Two packages, two types.
- **Canonicalise types through an alias table**, as point 10 does for platforms. Rejected
  in point 15: there is no authority to canonicalise against, and a table this project
  maintained would encode its opinion of every distribution in the world.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — the Download Server *"may be on the same host as the OpAMP Server or a different host"*, and
  *"The protocol supports only a single downloadable file per package. If the Agent's packages
  conceptually are composed of multiple files then the Agent and Server can agree to store the files
  in any file format that allows storing multiple files in a single file, e.g. a zip or tar file"* —
  the protocol's own answer to archives, and the reason unpacking is an implementation choice rather
  than a protocol one.
  Also "the packages that are available on the Server **for this Agent**", and "there is normally
  only one top-level package": the per-Agent offer this builds on, unchanged. The protocol has no
  notion of platform, which is why the selection has to happen before the offer is composed rather
  than in it.
  And the note that what a package contains "is Agent type-specific and is outside the concerns of
  the OpAMP protocol". The protocol names the concept, declines to model it, and leaves the Server to
  decide — which is the licence for deciding the type fit here.
- [`opentelemetry-collector-releases` v0.157.0](https://github.com/open-telemetry/opentelemetry-collector-releases/releases/tag/v0.157.0)
  — checked directly: assets are `.tar.gz` (44), `.deb` and `.rpm` (21 each), `.msi`, and never a
  bare binary; `opentelemetry-collector-releases_otelcol-contrib_checksums.txt` carries a SHA-256 per
  asset; each asset also has `.sig`/`.pem` companions, which are sigstore keyless signatures (a
  Fulcio certificate naming the release workflow identity, with a Rekor entry) — the evidence behind
  points 2 and 5.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — its example Server offers no packages at
  all, so there is no reference behaviour for where artifacts come from; the model is ours to choose.
- [`sevenz-rust2`](https://crates.io/crates/sevenz-rust2) `0.21.4` — checked on crates.io: a pure-Rust
  7z implementation, Apache-2.0 (this project's licence), last released 2026-08-01, with an `aes256`
  feature for password-protected archives built on the RustCrypto `aes`/`cbc` crates. Its default
  features pull `bzip2`, which binds to C — so it must be taken with `default-features = false` and
  only the codecs actually needed, keeping the pure-Rust build chain ADR-0006 and ADR-0007 insist on.
  Its predecessor `sevenz-rust` has not been released since 2024 and is not the one to build on.
- [OCI Image Index (image-spec)](https://github.com/opencontainers/image-spec/blob/main/image-index.md)
  — the same shape as the platform variants, and the reason to be confident in it: one name resolves
  to a list of manifests each carrying a required `platform` object of `architecture` and `os`, and a
  client picks the entry matching its requirements. It also settles the vocabulary question the same
  way this decision does — "image indexes SHOULD use, and implementations SHOULD understand, values
  listed in the Go Language document for `GOARCH`/`GOOS`" — i.e. one named canonical set that
  everything is understood *as*, rather than a free-for-all of equivalent spellings.
  For the Agent type the same structure is the counter-example, since an image index discriminates
  on platform alone and leaves "what is this image for" to the name. That is the design the type fit
  declines, because a container name is chosen by whoever pulls it and a package name is chosen by
  whoever uploads it.
- The Baseline's own `AgentDescription`
  ([`opamp.proto:690`](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L690)) — "keys/values
  are according to OpenTelemetry semantic conventions", then "the following attributes SHOULD be
  included: `os.type`, `os.version`" and "`host.*` to describe the host the Agent runs on". The
  protocol names the two attributes the platform fit reads *and* names the conventions as the
  vocabulary, which is the direct authority for points 10 and 11.
  It also defines `service.name` as "a reverse FQDN that uniquely identifies the Agent type", which
  is the attribute the type fit reads and the reason it is the right one to read.
- [OpenTelemetry semantic conventions — `host.arch`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/host.md)
  (`amd64`, `arm32`, `arm64`, `ia64`, `ppc32`, `ppc64`, `s390x`, `x86`) and
  [`os.type`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/os.md)
  (`aix`, `darwin`, `dragonflybsd`, `freebsd`, `hpux`, `linux`, `netbsd`, `openbsd`, `solaris`,
  `windows`, `zos`) — the canonical set adopted here, and the evidence for the `x86_64`/`amd64`
  divergence point 11 closes.
- [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — checked as the behavioural oracle for what actually arrives at a Supervisor Endpoint: it reports
  `os.type` from `runtime.GOOS` and `host.arch` from `runtime.GOARCH`, i.e. exactly the canonical set
  above. Since a Managed Process's attributes are folded over the Supervisor's, this is also the
  concrete path by which a host's reported architecture would change spelling without the host
  changing.
- [ADR-0023](0023-what-describes-an-agent.md) — what made a type
  matchable at all, and the resolution order (`opampextension`'s `dist.name` → the block's
  `service_name` → the program's file name) that guarantees every Agent this Client presents has one.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the comparable product models an Agent Type as a first-class object that a collector reports via
  `dist.name`, and hangs what an agent may receive off it rather than off free-form labels. Checked
  as evidence that type-as-a-property, not type-as-a-tag, is the shape that holds up in a shipping
  fleet manager.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) and
  [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) — the delivery, verification, and targeting this
  reuses wholesale. Uploading, referencing and unpacking only change how bytes reach the store.
  ADR-0016 is also the Selector semantics and the specificity rule the fit leaves intact and inserts
  in front of, and the source of the "one package per platform" sentence the variants make true.
- [ADR-0018](0018-client-self-update.md) and [ADR-0021](0021-release-pipeline-and-artifacts.md) — the
  single configured package name and the five published targets whose collision is the concrete case
  driving the platform variants.

## Consequences

- Positive: a package becomes a URL an operator already has. Nothing is uploaded, nothing is
  duplicated, and the fleet's software supply chain moves inside the API a portal or a pipeline can
  drive (goal 5).
- Positive: archives are handled, so "deliver an upstream Collector release" stops being a manual
  unpack step. Without unpacking, the same attempt installs a tarball over the binary, fails the
  health gate, and rolls back — safe, and useless.
- Positive: for a referenced package the artifact never exists on the fleet Server, so there is
  nothing there to leak from an unauthenticated download endpoint, and nothing to store. The bytes
  an Agent verifies are the ones the source published, and the SHA-256 from `checksums.txt` is
  checked on every host.
- Positive: **the Client self-update works across a heterogeneous fleet with no per-host
  configuration** — five artifacts uploaded under one name, each host offered its own. That is the
  case ADR-0018 promised and ADR-0021 made unreachable, and it needs no change to the Client's
  package handling.
- Positive: a mismatched binary is refused by construction rather than caught by a health gate, so
  the worst available operator mistake stops being available.
- Positive: **one spelling of a platform end to end** — release file name, upload, API response,
  package view, fleet row, and what a Collector reports through a Supervisor Endpoint all say
  `linux`/`amd64`. The upload of a full release becomes a loop over the published files with no
  translation step, and the alias table is left holding only foreign spellings.
- Positive: folding a Managed Process's attributes over the Supervisor's can no longer change a
  host's reported architecture, which silently breaks Selectors otherwise.
- Positive: per-variant history means a rollback is per platform, which is what a staged rollout
  across platforms actually needs.
- Positive: **the worst remaining operator mistake stops being available.** A package can no longer
  be installed over an Agent of another type, whatever its Selector says or fails to say, so the
  fleet-wide outage window the platform fit closes for the platform is closed for the role.
- Positive: **the Client's own binary is protected twice, on both sides of the wire** — the Server
  will not offer it a foreign artifact and the Client will not install one. ADR-0018's single point
  of protection stops being single.
- Positive: an untyped package is inert rather than fleet-wide, so the failure mode of an incomplete
  upload changes from "installed everywhere" to "installed nowhere, and the view says why".
- Positive: the Selector goes back to being purely about *aim* — rings, environments, canaries —
  instead of carrying the role as a pair that also perturbs the specificity count. A ladder of
  `{}` / `{rollout: canary}` no longer has to reserve a pair for the type.
- Negative / trade-offs: **every Agent needs egress to the source.** Without referenced packages a
  managed host needs to reach the fleet Server and nothing else, which in many fleets is the whole
  point of the fleet Server. A referenced package changes that host's network requirements, and in a
  closed network it simply cannot be used — those fleets upload instead.
- Negative / trade-offs: a rollout across three hundred hosts becomes three hundred downloads from a
  third party, subject to its rate limits and its availability, at the moment an operator most wants
  predictability. Nothing in this design caches or throttles that; the Server cannot, because it
  never has the bytes.
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
  `sevenz-rust2` (with `default-features = false`, `aes256`) for `.7z`. All pure Rust, but they parse
  untrusted input on the machine that runs the agent — a member path escaping the target directory,
  or an archive that expands without bound, must be refused, and that needs tests rather than
  assumptions.
- Negative / trade-offs: the archive key is a **fleet-wide shared secret on every host**, sitting in
  `client.toml` in the clear, so it is only as protected as that file's permissions. Rotating it
  means touching every host *and* repacking every encrypted archive. Sourcing it from an OS keystore,
  an environment variable, or a file with tighter permissions is a real improvement, left as a
  follow-up rather than pretended away here.
- Negative / trade-offs: **this breaks the published v1 package contract in four places** — `PUT`
  requires `os`/`arch`, the download URL and rollback do too, and `PackageView` moves `version`,
  `previous_version` and `source_url` into a `variants` list. Every generated client and every script
  against those routes changes. The project is pre-1.0 and the alternative is a guarantee with a hole
  in it, but this is the largest API break since ADR-0012.
- Negative / trade-offs: **artifact names change**, and ADR-0021 is right that they are a public
  contract — anything scripted against `…-macos-aarch64.7z` breaks, and older releases' assets keep
  the old names forever. This is the one chance to do it while the number of releases is small.
- Negative / trade-offs: **a Selector written against `host.arch: "x86_64"` stops matching**, because
  the Client reports `amd64`. Selectors are compared raw — the canonicalisation table is for the
  Platform, not for arbitrary attribute matching — so these must be edited on the Server. A
  `CHANGELOG.md` entry has to name this; it is a silent break otherwise, which is the worst kind.
- Negative / trade-offs: **an existing package store will not open** until every stored package is
  re-uploaded with a Platform. This is deliberate (point 13), but it is an operator action
  on every Server, and the Server stays down until it is done.
- Negative / trade-offs: an Agent reporting no platform gets no package. No Client this project ships
  is in that position — the Supervisor always reports both attributes — but a foreign OpAMP client
  connecting directly to the Server may be, and its rollout stops with a message rather than
  proceeding on a guess.
- Negative / trade-offs: the package view and the bundled UI grow a dimension. "Which version is this
  package at" no longer has one answer, and a fleet running five platforms has five, which is more
  honest and more to read.
- Negative / trade-offs: **a fourth breaking change to the v1 package contract.** Every package needs
  its type set before it is offered, `PackageView` grows a field, and any script that uploads and
  expects delivery needs one more request. Existing stores load and keep working, but every package
  in them is inert until typed — a rollout in flight at upgrade time stops.
- Negative / trade-offs: **a mistyped type is a silent no-op.** The platform fit catches a typo by
  canonicalising and echoing the pair back; there is no equivalent here, so `otelcol-contib` is
  indistinguishable from a type no Agent happens to run yet. The package view showing "offered to 0
  Agents" is the mitigation, and it is weaker than the platform's.
- Negative / trade-offs: **an Agent's type can change under a stable configuration.** ADR-0023 notes
  that a Collector gaining the `opampextension` switches its reported type from the program's file
  name to `dist.name`; with the type fit that also switches which packages reach it. The change is
  correct in both cases and surprising in both.
- Negative / trade-offs: one more thing to get right before a first rollout works, in a system whose
  reason for existing is that rollouts should be easy. Type, platform, Selector, consent — four
  things, and only the last is derived rather than stated.
- Follow-ups: distributing the archive key centrally without letting the Server read artifacts — the
  envelope sketched in the alternatives, which needs an Agent keypair. Verifying upstream sigstore
  signatures, which would replace a pasted checksum with real provenance.
- Follow-ups: the Baseline names two more attributes no Agent here reports — `os.version`, and
  `host.*` beyond the architecture. **`host.name` is the pressing one**: ADR-0016 twice offers "a
  Selector matching that host's `host.name`" as the way to pin one host, and no Agent reports it, so
  that Selector never matches; `opampextension` does report it, so a Collector-backed Agent has a
  host name in the same fleet where a Supervisor-backed one has none. Closing that gap is
  implementation within ADR-0012 and ADR-0016 rather than a decision of its own. `cloud.*` is not:
  filling it means probing a metadata service at startup, which is a network call in the start path
  and belongs in its own ADR. A variant matrix in the bundled UI — which platforms a package has,
  against the platforms the fleet actually reports — is a natural next step, as is warning at upload
  time when a Platform fits no Agent currently in the fleet.
- Follow-ups: **warn when a type fits no Agent in the fleet.** The platform equivalent is a
  follow-up too; here it matters more, because there is no canonicalisation to catch a typo, and it
  is the only cheap defence against the silent no-op above. Also open, and more clearly worth it with
  the type fit: the Server-set labelling described in ADR-0023's follow-ups, since with the role
  handled by type fit the Selector's remaining job is almost entirely rollout rings — which is
  exactly the thing that should not live in a file on each host.
