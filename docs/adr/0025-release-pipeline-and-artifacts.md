# ADR-0025: A release is a `version/*` tag built for five targets and published as `.7z` artifacts the Client can install

- **Status:** 🟡 proposed
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

## Context

[ADR-0009](0009-version-derivation-and-baking.md) computes the version in `build.rs` and says what a
release *is* — "building a commit that carries a well-formed `version/*` tag" — while deliberately
leaving the pipeline itself to a follow-up: "build targets, archives, checksums, publishing" are
named as that follow-up's subject. This is it.

Everything in CI today builds; nothing publishes. `ci.yml` produces release binaries for Linux,
Windows and macOS as a compile check and throws them away, so an operator who wants to install this
Client has no artifact to install — and a fleet that self-updates (ADR-0020) has nothing to upload
to its Server.

Four forces shape the answer, and three of them are already decided elsewhere:

- **Identity comes from the build, not from the pipeline.** ADR-0009 is explicit: "No pipeline-side
  version plumbing decides identity; the release workflow simply checks out the tag and builds — and
  must fetch tags and **assert the produced binary reports no `-dev` pre-release**, so a shallow
  clone without tags cannot silently publish a development build." It also lists "version from
  `Cargo.toml`, bumped per release" among the alternatives it rejected, because it "makes the
  version a hand-maintained file that must be kept in sync with tags". `[workspace.package] version`
  is `0.1.0` today and has never moved.
- **The artifact must be one the Client can already open.** ADR-0018 lets a package artifact be a
  bare program, a `.tar.gz`, or a `.7z`, decided by leading bytes — and this repository ships the
  tool that builds those, `opamp-package-sign pack`. A release format outside that set would be a
  second thing to maintain and a thing the fleet cannot install.
- **The self-update compares versions exactly.** `selfupdate::probe` refuses a staged binary whose
  reported version is not the offered one, string for string — including the `+<hash>` build
  metadata ADR-0009 always appends. Whatever an operator has to type into
  `PUT /api/v1/packages/<name>?version=` must therefore be the *full* baked string, and the release
  has to say it somewhere.
- **Artifact names are a public contract.** ADR-0009 says so in as many words. Once operators
  script against them, changing them costs more than getting them right now.

## Decision

We will publish a release from a **`version/*` tag**, built for **five targets**, packed as **`.7z`
by this project's own packer**, named
**`opamp-client-<version>-<os>-<arch>.7z`**.

1. **The trigger is the tag.** Pushing `version/*` builds and publishes. `workflow_dispatch` runs
   the same build and packing as a dry run, uploads the artifacts to the workflow, and publishes
   nothing — so the pipeline can be exercised without minting a release.

2. **Five targets, and the reason for each.** The table is written out rather than derived from the
   Rust triple, because the tokens in a file name are a contract and a triple is not.

   | target | runner | `<os>` | `<arch>` |
   |---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `linux` | `x86_64` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `aarch64` |
   | `aarch64-apple-darwin` | `macos-latest` | `macos` | `aarch64` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `macos` | `x86_64` |
   | `x86_64-pc-windows-msvc` | `windows-latest` | `windows` | `x86_64` |

   Both Linux architectures because a fleet's hosts are servers, and arm64 servers are ordinary.
   Both macOS architectures because Intel Macs are still deployed; the x86_64 one is cross-compiled
   from the arm runner, which Apple's toolchain does natively, rather than depending on an Intel
   runner label that keeps being retired. **Windows on arm64 is deliberately out** for now: no
   deployment has asked for it, and the C dependencies underneath TLS are unproven on that target
   here — adding it later is one row.

3. **The container is `.7z`, written by `opamp-package-sign pack --format 7z`.** Not a hand-rolled
   `7z` call: the packer names the single member after the program, which is exactly the name the
   Client looks for (`client` / `client.exe`), so **a release asset is also a valid package artifact
   for a self-update** — an operator uploads the file they downloaded, unmodified, and ADR-0018's
   guarantee that the hash an Agent verifies is the one the release published holds by
   construction. The packer prints the artifact's SHA-256 on stdout, which is where the checksums
   come from.

4. **The name is `opamp-client-<version>-<os>-<arch>.7z`**, where `<version>` is the base version
   without the `+<hash>` build metadata — `opamp-client-1.2.3-linux-x86_64.7z`. The metadata is
   redundant in the name (the tag identifies the commit) and is a character that download tooling
   handles inconsistently. It is *not* redundant to an operator: the **full** baked string is what
   `?version=` must carry for a self-update to pass its probe, so the release notes and the
   checksum file both state it.

5. **The version is read out of a built binary, once.** A first job builds the Client, asks it
   `--version`, and hands the string to every packing job, so all five artifacts are named by the
   same value and no job derives a version of its own. On a tag build that job **fails if the
   version carries `-dev`**, which is ADR-0009's requirement against publishing from a shallow
   clone; on a dry run `-dev` is expected and allowed. Every job checks out with full history so
   the baked version is the tag's.

6. **What is published:** the five `.7z` artifacts and a `SHA256SUMS` file, on a GitHub release
   named after the version, whose notes state the full baked version string and how to hand the
   artifact to a fleet.

## Alternatives considered

- **Take the version from `Cargo.toml`** — asked for, and it cannot be done without contradicting
  an accepted decision: ADR-0009 rejected exactly this, `[workspace.package] version` is a static
  `0.1.0` that no release process maintains, and an artifact named `0.1.0` while the binary inside
  reports `1.2.3+a1b2c3d` would be refused by the self-update's own version probe on every host it
  reached. Reversing this is possible, but it is a change to ADR-0009 and needs its own ADR — not a
  line in a workflow.
- **`.tar.gz` per platform** (`.tar.gz` on Unix, `.zip` on Windows), the common convention. The
  Client cannot open a `.zip` at all — it would be installed *as* the program, unopened — so the
  convention would split the fleet's install path by platform for no gain. `.7z` is one format
  everywhere, and the one the packer can also encrypt when an operator needs that (ADR-0018).
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

## Sources / Prior art

- **This project's ADR-0009**, which specifies what a release is, what the pipeline may and may not
  decide, and the `-dev` assertion this implements.
- **The Cargo Book, environment variables and targets** — `--target` selects the built triple while
  host tooling (the packer) stays a host build, which is what lets one job produce a cross-compiled
  artifact and pack it locally.
  <https://doc.rust-lang.org/cargo/reference/environment-variables.html>
- **`opentelemetry-collector-releases`** publishes one archive per OS/architecture with the platform
  in the file name — the naming shape adopted here, and the layout ADR-0018's member matching was
  written against.
  <https://github.com/open-telemetry/opentelemetry-collector-releases>
- **Elastic Agent** ships per-platform archives holding the agent, and states that the archive
  distributions are the ones its fleet can upgrade from — the same coupling this decision makes
  between "what an operator downloads" and "what the fleet installs".
  <https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent>

## Consequences

- Positive: there is something to install. An operator downloads one file per host, and the same
  file is what they hand the Server for a fleet-wide self-update — no repacking, so the hash the
  release published is the hash an Agent verifies.
- Positive: the version in the file name comes from the binary inside it, and a build that lost its
  tags fails the release instead of publishing a `0.0.0-dev` artifact.
- Negative / trade-offs: the name carries the base version, so two builds of the same version are
  indistinguishable by name. A release is a tag, so this can only happen by re-tagging — but it
  means the file name is not a unique build identifier, and the notes carry the full string for
  when that matters.
- Negative / trade-offs: five targets means five ways to break, and two of them are not what the
  runner natively is — the cross-compiled macOS x86_64 build and the arm64 Linux runner label. Both
  fail loudly and early rather than producing a bad artifact.
- Negative / trade-offs: nothing is signed yet. An operator who wants provenance beyond the
  checksum has to wait for the signing decision.
- Follow-ups: where the signing key lives and how a Client is told the public half, which would let
  `opamp-package-sign sign` run in this pipeline. Whether the Server should grow an endpoint that
  imports a release by URL directly (ADR-0018 already imports from a URL). And whether the Server
  binary deserves the same treatment — it is deployed by an operator, not by the fleet, which is
  why this decision covers the Client alone.
