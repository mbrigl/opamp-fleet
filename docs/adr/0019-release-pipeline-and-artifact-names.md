# ADR-0019: A release is a `version/*` tag built for five targets, published as artifacts the Client can install, and named by four fields separated by `_`

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

## Context

[ADR-0009](0009-version-from-cargo-toml-and-git.md) computes the version in `build.rs` and says what a
release *is* — "building a commit that carries a well-formed `version/*` tag" — while deliberately
leaving the pipeline itself to a follow-up: "build targets, archives, checksums, publishing" are
named as that follow-up's subject. This is that decision.

An operator who wants to install this Client needs an artifact to install, and a fleet that
self-updates (ADR-0017) needs something to upload to its Server.

Four forces shape the answer:

- **Identity comes from the build, not from the pipeline.** ADR-0009 requires that the release
  workflow "must fetch tags and **assert the produced binary reports no `-dev` pre-release**, so a
  shallow clone without tags cannot silently publish a development build". Where the number comes
  from is [ADR-0009](0009-version-from-cargo-toml-and-git.md)'s: `Cargo.toml` decides it, and the pipeline
  creates the tag from it before it builds.
- **The artifact must be one the Client can already open.** ADR-0015 lets a package artifact be a
  bare program, a `.tar.gz`, or a `.7z`, decided by leading bytes — and this repository ships the
  tool that builds those, `opamp-package-sign pack`. A release format outside that set would be a
  second thing to maintain and a thing the fleet cannot install.
- **The self-update compares versions without their build metadata**
  ([ADR-0009](0009-version-from-cargo-toml-and-git.md)), so
  `PUT /api/v1/packages/<name>?version=` takes the release number, and the full baked string is
  provenance the release still has to state somewhere.
- **Artifact names are a public contract.** ADR-0009 says so in as many words. Once operators
  script against them, changing them costs more than getting them right.

**A release file name has four fields, and two of them can contain a hyphen.**

- The name can. ADR-0010's name grammar is `[a-z0-9-]`.
- The version can. ADR-0009 bakes a SemVer string and the file name carries the base version — but a
  base version may carry a prerelease (`0.1.2-dev`, which every build off a tag-less clone reports),
  and SemVer spells that with a hyphen.

With `-` as the separator the name does not parse back, and everything that reads one has to
*guess* where the fields are. This project has two such readers. The fleet view prefills the upload
form's four fields from a picked artifact
([`index.html:739`](../../crates/server/static/index.html#L739)); a `-`-splitting heuristic there
("the last two tokens are the platform, the version is the first remaining token that begins with a
digit") holds for our own artifacts and is wrong in general — an upstream build called
`otelcol-2-1.0.0-linux-amd64` has its name cut after `otelcol`. It is a guess with a
plausible-looking answer, which is the worst kind here: a wrong platform is a package no Agent is
ever offered (ADR-0021 point 3), and the operator sees a filled-in form rather than an error. The
release notes' upload loop ([`release.yml:244`](../../.github/workflows/release.yml#L244)) has the
same problem unless it can split the name without being told the name and version first.

**The project has already solved this once, and the reasoning points at `_`.** The Server's on-disk
variant is `<name>@<os>-<arch>` precisely because of a grammar argument
([`packages.rs:1085`](../../crates/server/src/packages.rs#L1085)): "the ADR-0010 name grammar admits
neither `@` nor `_`, and a canonical platform token admits neither `@` nor `-`, so the parts of this
never run together." Apply the first half of that sentence to the release file name and the answer
is there: a name cannot contain `_`, and neither can a SemVer version — `_` is not in SemVer's
alphabet for a prerelease or for build metadata. A separator the fields cannot contain is what makes
a file name parse back instead of being guessed at.

**And it is what this artifact's neighbourhood already looks like.** The fleet's most common managed
process ships as `otelcol_0.157.0_linux_amd64.tar.gz`; that is GoReleaser's default archive name,
`{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`, which most of the Go-ecosystem releases an
operator downloads are named by. Debian has separated a package's fields with `_` for the same reason
for decades — `hello_2.10-2_amd64.deb`, where the version keeps its hyphen and the fields do not lose
their boundaries. An operator who has ever unpacked a Collector release already knows how to read
this shape.

## Decision

We will publish a release from a **`version/*` tag**, built for **five targets**, packed **by this
project's own packer**, and named by **four fields separated by `_`**.

### The release

1. **The trigger is the tag.** Pushing `version/*` builds and publishes. `workflow_dispatch` runs
   the same build and packing as a dry run, uploads the artifacts to the workflow, and publishes
   nothing — so the pipeline can be exercised without minting a release. Who creates the tag and
   how a run is started are [ADR-0009](0009-version-from-cargo-toml-and-git.md)'s (clauses 2, 4 and 5).

2. **Five targets, and the reason for each.** The table is written out rather than derived from the
   Rust triple, because the tokens in a file name are a contract and a triple is not. The `<os>` and
   `<arch>` tokens are [ADR-0021](0021-one-platform-vocabulary.md)'s platform vocabulary
   (points 5 and 7), and the artifact name is [ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md)
   clause 8's:

   | target | runner | `<os>` | `<arch>` | artifact |
   |---|---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `linux` | `amd64` | `supervisor_1.2.3_linux_amd64.tar.gz` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `arm64` | `supervisor_1.2.3_linux_arm64.tar.gz` |
   | `aarch64-apple-darwin` | `macos-latest` | `darwin` | `arm64` | `supervisor_1.2.3_darwin_arm64.tar.gz` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `darwin` | `amd64` | `supervisor_1.2.3_darwin_amd64.tar.gz` |
   | `x86_64-pc-windows-msvc` | `windows-latest` | `windows` | `amd64` | `supervisor_1.2.3_windows_amd64.tar.gz` |

   Both Linux architectures because a fleet's hosts are servers, and arm64 servers are ordinary.
   Both macOS architectures because Intel Macs are still deployed; the x86_64 one is cross-compiled
   from the arm runner, which Apple's toolchain does natively, rather than depending on an Intel
   runner label that keeps being retired. **Windows on arm64 is deliberately out** for now: no
   deployment has asked for it, and the C dependencies underneath TLS are unproven on that target
   here — adding it later is one row.

3. **The artifact is written by `opamp-package-sign pack`.** Not a hand-rolled archiver call: the
   packer names the single member after the program, which is exactly the name the Client looks
   for, so **a release asset is also a valid package artifact for a self-update** — an operator
   uploads the file they downloaded, unmodified, and ADR-0015's guarantee that the hash an Agent
   verifies is the one the release published holds by construction. The packer prints the
   artifact's SHA-256 on stdout, which is where the checksums come from. The container is
   ADR-0022 clause 8's: `.tar.gz`.

4. **The name is `<name>_<version>_<os>_<arch>`**, with `<name>` `supervisor` (ADR-0022 clause 8),
   `<os>` and `<arch>` ADR-0021's tokens, and `<version>` the base version without the `+<hash>`
   build metadata — `supervisor_1.2.3_linux_amd64.tar.gz`. The metadata is redundant in the name
   (the tag identifies the commit) and is a character that download tooling handles inconsistently.
   `?version=` takes the release number, so an operator never needs the metadata to hand the
   artifact to a fleet (ADR-0009 clause 14).

5. **The version is decided once, and every job is named from it.** One job establishes the version
   and hands it to every packing job, so all five artifacts are named by the same value and no job
   derives a version of its own. Where that number comes from, and how drift between `Cargo.toml`
   and a tag is refused, is ADR-0009's. On a tag build that job **fails if the built binary's
   version carries `-dev`**, which is ADR-0009's requirement against publishing from a shallow
   clone; on a dry run `-dev` is expected and allowed. Every job checks out with full history so
   the baked version is the tag's.

6. **What is published:** the five artifacts and a `SHA256SUMS` file, on a GitHub release named
   after the version, whose notes state the full baked version string and how to hand the artifact
   to a fleet. [ADR-0020](0020-installing-the-client-and-native-installers.md) adds the native installers to
   what a release publishes.

### Reading a release file name

7. **The name is read by splitting, not by guessing.** Neither the ADR-0010 name grammar nor a
   SemVer version admits `_`, so the four fields are the four `_`-separated fields of the stem. The
   fleet view's prefill and the release notes' upload loop both are that split, and neither needs
   to know the product's name or version in advance to find the platform.

8. **Read from the right, and rejoin a platform token that carries the separator.** ADR-0021 point 5
   lets a *canonical* platform token match `[a-z0-9_]{1,16}`, and its compatibility table knows
   `x86_64` — so a foreign artifact named `foo_1.0.0_linux_x86_64.tar.gz` splits into five fields,
   not four. The rule for that: take the arch from the last field, and if it is not a token this
   vocabulary knows, try the last *two* fields joined by `_` before giving up. Nothing this project
   publishes can hit it (`amd64`, `arm64`), and a name that still does not resolve fills nothing
   rather than filling a guess.

9. **Only the release file name uses `_`.** A platform is still `<os>-<arch>` where it is a *tag*
   rather than a field of a file name: the Server's `<name>@<os>-<arch>.json`/`.bin`, the fleet
   view's platform column, the `linux-amd64` in prose. Those keep `-` for the mirror-image reason —
   a canonical token may contain `_` but never `-` — and the API keeps taking `os` and `arch` as two
   separate query parameters, so nothing there has to parse anything at all.

10. **Published artifacts keep their names.** Nothing is renamed and no release asset is rewritten:
    a checksum published against a URL stays true. Both separators are therefore readable — the
    fleet view's prefill keeps accepting `-`, which it must anyway, because upstream artifacts an
    operator uploads are named by upstream and many of them use hyphens.

## Alternatives considered

- **`.tar.gz` on Unix and `.zip` on Windows**, the common split convention. The Client cannot open a
  `.zip` at all — it would be installed *as* the program, unopened — so the convention would split
  the fleet's install path by platform for no gain. The container is one format everywhere.
- **Publish the raw binaries, uncompressed.** Simplest, and it throws away the property that makes
  the asset directly installable by the fleet: a bare binary is a valid artifact, but then the
  release cannot later carry more than one file per target without changing its shape.
- **Sign the artifacts in the pipeline** (`opamp-package-sign sign`). Wanted, and not here: the
  signing key is the decision — where it lives, who may use it, how a Client learns the public half
  — and that is a security decision of its own, not a step to bolt onto a build. The content hash
  is published meanwhile, which is what ADR-0015 always verifies.
- **Build every target on its own native runner.** Cleaner in principle; in practice it makes the
  matrix depend on Intel macOS runner labels that GitHub keeps retiring. Cross-compiling one target
  from an SDK that supports it natively is the smaller risk.
- **Keep `-` and specify the heuristic** — write down "the platform is the last two tokens, the
  version starts at the first token beginning with a digit" as the contract. Rejected: it is not a
  grammar, it is a lookahead that happens to work on our own five names. It cannot state what
  `otelcol-2-1.0.0-linux-amd64` means, and the ambiguity is structural — one separator cannot both
  occur inside a field and delimit it.
- **`name_version_os-arch`** — `_` between the three parts and the platform kept as the Server's
  hyphenated tag. This is, honestly, the tighter grammar: it is unambiguous in *both* directions,
  because a name and a version cannot contain `_` and a platform token cannot contain `-`, so
  clause 8's rejoin rule would not be needed at all. Rejected because it is a fourth shape that
  nothing else in the world writes, against a convention (GoReleaser's default, the Collector's own
  releases, Debian) that operators already read fluently — and because the one case the rejoin rule
  covers is a foreign artifact, never ours.
- **A sidecar manifest per artifact** — publish the four fields as JSON beside the artifact and read
  them from there. Rejected: an operator downloads one file and hands that one file to a Server, so
  it must be self-describing; a second file that can be lost or skipped is a worse contract than a
  parseable name.
- **Rename the assets of releases already published** for one consistent history. Rejected for the
  reason in clause 10: the public-contract argument cuts both ways — change the shape going
  forward, never rewrite what was published.

## Sources / Prior art

- **This project's ADR-0009**, which specifies what a release is, what the pipeline may and may not
  decide, and the `-dev` assertion this implements.
- **The Cargo Book, environment variables and targets** — `--target` selects the built triple while
  host tooling (the packer) stays a host build, which is what lets one job produce a cross-compiled
  artifact and pack it locally.
  <https://doc.rust-lang.org/cargo/reference/environment-variables.html>
- **`opentelemetry-collector-releases`** publishes one archive per OS/architecture with the platform
  in the file name — the naming shape adopted here, and the layout ADR-0015's member matching was
  written against.
  <https://github.com/open-telemetry/opentelemetry-collector-releases>
- **Elastic Agent** ships per-platform archives holding the agent, and states that the archive
  distributions are the ones its fleet can upgrade from — the same coupling this decision makes
  between "what an operator downloads" and "what the fleet installs".
  <https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent>
- [GoReleaser — Archives](https://goreleaser.com/customization/archive/): the default
  `name_template` is `{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`. This is the tool most
  Go-ecosystem projects release with, so it is the de-facto shape of the artifacts an operator of
  this fleet already downloads — including the one that matters most here.
- [OpenTelemetry — Install the Collector on Linux](https://opentelemetry.io/docs/collector/install/binary/linux/):
  the Collector's own releases are `otelcol_<version>_linux_amd64.tar.gz`. The artifact this
  project's Supervisor most often delivers is named the way this decision names ours, which is also
  why the fleet view's prefill gets *more* right, not less, by learning `_`.
- [Debian FAQ § Basics of the package management system](https://www.debian.org/doc/manuals/debian-faq/pkg-basics.en.html)
  and [dpkg-name(1)](https://www.man7.org/linux/man-pages/man1/dpkg-name.1.html):
  `<package>_<version>-<revision>_<architecture>.deb`, and `dpkg-name` renames files *into* that
  shape. The oldest large-scale instance of exactly this trade-off — a version that keeps its hyphens
  inside a field whose boundaries stay legible.
- [Semantic Versioning 2.0.0](https://semver.org/): the alphabet of a prerelease identifier and of
  build metadata is `[0-9A-Za-z-]` — a hyphen is *in* a version and an underscore cannot be, which is
  the whole grammatical basis of clause 7.
- [ADR-0010](0010-client-os-service-and-installation-layout.md) — the `[a-z0-9-]` name grammar, the other half of
  that basis.
- [ADR-0021](0021-one-platform-vocabulary.md) clause 5 and 7 — the platform vocabulary and the
  canonical-token grammar `[a-z0-9_]{1,16}`, which is both what makes the file name state a reported
  platform verbatim and the single ambiguity clause 8 has to answer.
- [`packages.rs:1085`](../../crates/server/src/packages.rs#L1085) — this project's own prior art: the
  separator for the on-disk variant stem was chosen by asking which characters the parts cannot
  contain. Same question, same method, different answer, because a file name's fields are the name
  and the version rather than two platform tokens.

## Consequences

- Positive: there is something to install. An operator downloads one file per host, and the same
  file is what they hand the Server for a fleet-wide self-update — no repacking, so the hash the
  release published is the hash an Agent verifies.
- Positive: the version in the file name is the one the binary inside it was built with, and a
  build that lost its tags fails the release instead of publishing a `-dev` artifact.
- Positive: a release file name parses back into its four fields by splitting on one character, so
  the fleet view's prefill is correct rather than lucky, and the release notes can publish an upload
  loop that reads `os` and `arch` out of the file name without being told the name and version
  first. The shape matches what the Collector and most of the Go ecosystem publish, so an operator
  recognises it — and the prefill, accepting both separators, serves upstream artifacts and ours
  with one code path.
- Negative / trade-offs: the name carries the base version, so two builds of the same version are
  indistinguishable by name. A release is a tag, so this can only happen by re-tagging — but it
  means the file name is not a unique build identifier, and the notes carry the full string for
  when that matters.
- Negative / trade-offs: five targets means five ways to break, and two of them are not what the
  runner natively is — the cross-compiled macOS x86_64 build and the arm64 Linux runner label. Both
  fail loudly and early rather than producing a bad artifact.
- Negative / trade-offs: a platform pair is spelled two ways in this project — `linux_amd64`
  between the fields of a release file name, `linux-amd64` as a tag on disk, in the API's answers
  and in the fleet view — which is one more thing to know, accepted because the two live in
  different grammars and each separator is the one its neighbours cannot contain. Artifacts
  published under `-` keep that shape forever, so both must stay readable. The rejoin rule of
  clause 8 is a wart that the rejected `name_version_os-arch` would not have needed.
- Negative / trade-offs: nothing is signed yet. An operator who wants provenance beyond the
  checksum has to wait for the signing decision.
- Follow-ups: where the signing key lives and how a Client is told the public half, which would let
  `opamp-package-sign sign` run in this pipeline. Whether the Server should grow an endpoint that
  imports a release by URL directly (ADR-0015 already imports from a URL). And whether the Server
  binary deserves the same treatment — it is deployed by an operator, not by the fleet, which is
  why this decision covers the Client alone. If a platform token this project *publishes* ever
  carries `_`, the right-to-left rejoin rule is the one place that has to be revisited; and if a
  release ever ships something other than one binary per platform, the field set — not the
  separator — is what that decision would touch.
