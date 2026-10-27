# ADR-0034: Releases, attestations and the Server's image

- **Status:** 🟢 accepted
- **Date:** 2026-10-23
- **Deciders:** Markus Brigl
- **Applies to:** `.github/workflows/release.yml` (the `version`, `build`, `sbom`, `image` and `publish` jobs, their permissions, the assets published and the release notes), `packaging/container/Containerfile`, the image `ghcr.io/mbrigl/opamp-fleet-server`, `.github/dependabot.yml`, `crates/fleet-tools/tests/release_workflow.rs`, the names of every release asset, the fleet view's reading of an artifact's file name, and every document that tells an operator how to download, check or run a release asset or the Server's image (`README.md`, `docs/manual/`, `CHANGELOG.md`)

## Context

An operator needs something to install, and a fleet needs something to hand its Server. They are
different questions. The fleet needs one artifact per platform that the Client itself can open and
install as a self-update ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)). A
host needs what its platform expects: native packages, which
[ADR-0019](0019-the-installed-service-and-its-state.md) decides.
`opentelemetry-collector-releases` and Elastic Agent both publish native packages *beside* their
archives, for exactly that reason. Artifact names are a public contract: operators script against
them.

`SHA256SUMS` proves only that a download matches the file next to it on the same release page.
Anyone who can replace one asset can replace both. Two assets carry more weight than the rest: the
Client's archive, which a Server hands to a whole fleet as a self-update, where the fleet's package
signature ([ADR-0027](0027-packages-signatures-and-delivery.md)) is made over whatever the operator
downloaded; and the operator tool, which makes the fleet's certificate authorities and its signing
key ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)). An operator who wants to know that a
file came from this repository's release workflow, at a known commit, needs a way to check it.

GitHub artifact attestations answer that question. A workflow job with `id-token: write` gets a
short-lived Sigstore certificate bound to its workflow identity. It uses that certificate to sign an
in-toto statement with a SLSA v1 build provenance predicate over the files' SHA-256 digests. The
statement is stored in the repository's attestation store and, for a public repository such as
`mbrigl/opamp-fleet`, recorded in the public Rekor transparency log.
`gh attestation verify <file> --repo <owner>/<repo>` checks it. On a public repository this is
available on every plan; on a private one it needs GitHub Enterprise Cloud.

A software bill of materials lists what a binary is built from. An operator runs it through their
own vulnerability scanner without rebuilding, and regulators increasingly ask for one; the EU Cyber
Resilience Act's Annex I asks a manufacturer to draw one up. The shipped programs depend on
different crates on different targets: `windows-sys` and the service code on Windows, other code on
Linux and macOS. A scanner fed with crates that are not in the binary reports findings that do not
apply.

The Server runs in a container. Its whole configuration may come from the environment, with every
secret a file whose path the environment names
([ADR-0030](0030-the-servers-configuration.md)). It has no health endpoint; its two planes listen
on `127.0.0.1:4320` and `127.0.0.1:4321` by default, and off the loopback both require TLS and the
Operator plane requires `[rest.auth]` ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)).
The release workflow builds Linux binaries for `x86_64-unknown-linux-gnu` and
`aarch64-unknown-linux-gnu` on GitHub-hosted runners with the pinned toolchain
(`rust-toolchain.toml`), from the committed lockfile, without a cache; a binary for the GNU targets
links glibc and `libgcc_s` dynamically. A reader without this conversation would expect a
multi-stage `Dockerfile` that compiles the Server inside the image build, a `latest` tag, and a
`HEALTHCHECK`; this record decides against all three.

Constraints from other decisions:

- Third-party actions are pinned to a SHA, and a first-party action (`actions/*`) stays on its
  major tag ([ADR-0010](0010-actions-are-pinned-and-kept-current-by-dependabot.md)).
- The release workflow installs every tool that writes what ships at an exact version raised by a
  reviewed commit (`cargo install --locked cargo-deb@3.8.0 …`);
  [`scripts/check-build-hardening.sh`](../../scripts/check-build-hardening.sh) check 3 holds that
  rule.
- The workflow keeps tokens away from code it does not control: the `advisories` job runs apart
  from the job that can push, and no checkout persists credentials. A build script of any
  dependency runs in `build`. A job holding `id-token: write` could mint an attestation for any
  bytes it chose.
- The specification's Q-1 and Q-3 apply to every release artifact, the image included: nothing it
  starts may send a credential unencrypted beyond the loopback, and nothing in it may enter without
  the supply-chain checks a dependency passes.

## Decision

We will publish each release from one workflow run, as one `.tar.gz` per target named
`supervisor_<version>_<os>_<arch>` (the fleet's artifact) beside the native installers, the
operator tool's archives and one CycloneDX SBOM per shipped program and target, together with a
multi-architecture image of the Server built from its release binary, and attest the build
provenance of every asset and of the image, publishing nothing that does not verify.

### The release and its assets

1. **A release is one workflow run that builds five targets from one version.** The version and its
   `version/*` tag are decided as [ADR-0033](0033-the-products-version.md) states. A first job
   decides the number, builds the Client and asks it `--version`; every artifact is named from that
   one answer, and each target's build asserts its own binary reports the same full version (except
   the cross-compiled macOS x86_64 build, which the runner cannot execute). Every job checks out
   with full history so the baked version is the tag's. A `workflow_dispatch` dry run, the default,
   builds and packs everything, uploads it to the workflow run, and tags and publishes nothing.

   | target | runner | `<os>` | `<arch>` |
   |---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `linux` | `amd64` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `arm64` |
   | `aarch64-apple-darwin` | `macos-latest` | `darwin` | `arm64` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `darwin` | `amd64` |
   | `x86_64-pc-windows-msvc` | `windows-latest` | `windows` | `amd64` |

   Both Linux architectures because arm64 servers are ordinary; both macOS architectures because
   Intel Macs are still deployed, the Intel one cross-compiled from the arm runner rather than
   depending on an Intel runner label that keeps being retired. **Windows on arm64 is out** until a
   deployment asks for it; adding it is one row.

2. **Every archive is a `.tar.gz` written by `opamp-fleetctl package pack --format tar.gz`,** the
   operator tool the same run builds and publishes
   ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)). The packer names the single member
   after the program, with its executable mode, so **the Client's release archive is a valid
   package artifact unmodified**: the operator uploads the file they downloaded, and the SHA-256
   the packer prints is both the published checksum and the hash an Agent verifies
   ([ADR-0027](0027-packages-signatures-and-delivery.md)). `.tar.gz` because it is the container
   every other agent's package ships as and the one that carries the executable bit and unpacks the
   same way on every platform. `.7z` remains a package container the Client opens and the packer
   writes, including encrypted ([ADR-0027](0027-packages-signatures-and-delivery.md)); a release
   has no use for encryption.

3. **Every asset of the Client is named `supervisor_<version>_<os>_<arch>.<ext>`,** and the
   operator tool's by the same grammar under its own name
   ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)). Four fields separated by `_`: the
   package name, the base version `MAJOR.MINOR.PATCH` without build metadata, and the platform in
   the vocabulary an Agent reports as `os.type` and `host.arch`
   ([ADR-0027](0027-packages-signatures-and-delivery.md)). **All artifacts of a target share the
   stem** and differ in extension alone:

   | | artifact |
   |---|---|
   | archive | `supervisor_1.2.3_linux_amd64.tar.gz` |
   | Debian | `supervisor_1.2.3_linux_amd64.deb` |
   | RPM | `supervisor_1.2.3_linux_amd64.rpm` |
   | Windows | `supervisor_1.2.3_windows_amd64.msi` |

   `_` because it is the one separator no field contains: the name grammar is `[a-z0-9-]` and SemVer
   spells a pre-release with `-`, while neither admits `_`. It is also the shape an operator
   already reads: GoReleaser's default, the Collector's own `otelcol_<version>_linux_amd64.tar.gz`,
   Debian's `<package>_<version>_<arch>.deb`. Where a platform is a *tag* rather than a field of a
   file name (the store's `<os>-<arch>.bin`, the fleet view's platform column) it stays
   `<os>-<arch>`, for the mirror-image reason. Inside the `.deb` and `.rpm` the architecture is the
   ecosystem's own (`amd64`/`arm64`, `x86_64`/`aarch64`), derived by the packaging tool; nothing
   resolves a package by its file name. **A published asset is never renamed or rewritten**: a
   checksum published against a URL stays true. **A release that changes the asset names or
   containers says so**: the change breaks operators' scripts, and `CHANGELOG.md` and the notes of
   the release that carries it name the old and the new form.

4. **A file name is read from the right, and an unknown tail fills nothing.** The fleet view's
   upload form reads an artifact's name by matching the platform tokens it knows, including the
   aliases `x86_64`, `aarch64`, `macos`, at the end of the stem, separated by `_` or `-`; the
   version starts at the first separator followed by a digit, and the rest is the name. A tail it
   does not know fills no field rather than a guess, because a wrong platform is an entry no Agent
   is ever offered. Accepting `-` serves upstream artifacts named by upstream. The release notes'
   upload loop selects the `supervisor_` stem and splits the rest on `_`, so it needs to be told no
   platform.

5. **What is published.** A GitHub release named after the version holds:
   - the Client's five archives, the two `.deb`, the two `.rpm` and the `.msi`;
   - the four archives of the operator tool
     ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md));
   - the eleven SBOMs (clause 7);
   - one `SHA256SUMS` covering every asset.

   Beside it, `ghcr.io/mbrigl/opamp-fleet-server:<version>` holds the Server's image. Every asset
   is a subject of the release's build-provenance attestation, and the image of its own (clause 9).
   The notes say:
   - which file is for which purpose, and the image's name and digest;
   - the per-platform install commands, and how to run the image;
   - how to verify an asset: `gh attestation verify <file> --repo <repository> --signer-workflow
     <repository>/.github/workflows/release.yml`, and `sha256sum -c --ignore-missing SHA256SUMS`
     once `SHA256SUMS` has verified;
   - how to verify the image:
     `gh attestation verify oci://ghcr.io/mbrigl/opamp-fleet-server@<digest>
     --repo <repository> --signer-workflow <repository>/.github/workflows/release.yml`;
   - the procedure that uploads the Client's archives, selected by their `supervisor_` stem, as one
     package for Agent type `supervisor`;
   - the full baked version string, as provenance. The upload uses the release number, since
     versions are compared without build metadata ([ADR-0033](0033-the-products-version.md)).

6. **The operator tool's archives are attested, as the Client's artifacts are.** They carry what
   every asset of a release carries: their entry in `SHA256SUMS`, and the release's
   build-provenance attestation (clause 9). Each archive also has its program's SBOM for its target
   beside it (clause 7). Provenance for the release's assets is one decision for all of them, this
   one, not a property of this program alone. No archive carries a code signature.

### SBOMs and attestations

7. **One CycloneDX SBOM per shipped program and target.** The release publishes
   `supervisor_<version>_<os>_<arch>.cdx.json` for each of the Client's five targets,
   `opamp-fleetctl_<version>_<os>_<arch>.cdx.json` for each of the operator tool's four, and
   `server_<version>_linux_<arch>.cdx.json` for each of the Server's two. The stem of the first two
   is that of the archive it describes (clause 3); the Server's names the binary inside the image
   of that architecture. `.cdx.json` is the extension the CycloneDX specification recommends. Each
   SBOM is CycloneDX 1.5 JSON written by `cargo cyclonedx` with `--describe binaries` and
   `--target <triple>`, for:
   - the one binary that ships, `supervisor`, `opamp-fleetctl` or `server`;
   - the default features, which the release builds with;
   - every transitive dependency on that target, resolved from the committed `Cargo.lock`.

   The lockfile must be unchanged afterwards. The `.deb`, `.rpm` and `.msi` of a target package the
   binary of its archive unchanged, so the archive's SBOM describes them, and they get none of
   their own. The image's base layers are Debian packages that `cargo cyclonedx` does not see; the
   image's SBOM is the Server's, and the base is named by its digest (clause 18).

8. **The SBOMs are written by a job of their own, with an exactly pinned tool.** A job `sbom` of
   `release.yml` runs on `ubuntu-latest` after `version`. It writes every SBOM of clause 7, since
   `cargo cyclonedx --target` reads the dependency graph for a target without building for it. It
   writes a `.sha256` beside each SBOM and uploads them to the run as the `build` rows upload
   theirs. It runs on a dry run too, so that every dry run exercises it. Its token can read and do
   nothing else, its checkout keeps no credentials, and it restores no cache. It installs the tool
   with `cargo install --locked cargo-cyclonedx@X.Y.Z`, at an exact version raised by a reviewed
   commit, as the packaging tools are. No action installs it.

9. **One build-provenance attestation covers every published asset, and one covers the image.**
   After `SHA256SUMS` is assembled and before the release is created, the `publish` job runs
   `actions/attest@v4` in provenance mode, on its major tag
   ([ADR-0010](0010-actions-are-pinned-and-kept-current-by-dependabot.md)). The subjects are:
   - every file it then publishes: the `.tar.gz` archives, the `.deb`, `.rpm` and `.msi`, the
     `.cdx.json` SBOMs;
   - `SHA256SUMS` itself.

   A second `actions/attest@v4` step attests the image, with `subject-name`
   `ghcr.io/mbrigl/opamp-fleet-server`, `subject-digest` the index digest from `image`, and
   `push-to-registry: true`, so the attestation travels with the image. The digests of the files
   are computed from the files themselves, not read out of `SHA256SUMS`. The per-file `.sha256`
   files are folded into `SHA256SUMS` and not published, so they are not subjects. Each attestation
   is signed through the Sigstore public-good instance, which this public repository gets, recorded
   in Rekor and stored in the repository's attestation store.

10. **Only the `publish` job may attest.** `id-token: write` and `attestations: write` are granted
    to `publish` alone, beside its `contents: write` and `packages: write`, which it needs to push
    the image's attestation and to tag the image. They appear neither at the workflow's top level,
    which stays `contents: read`, nor on any other job; `image` holds `packages: write` without
    either. `publish` checks out no code and runs no build. The only actions it runs are
    `actions/download-artifact` and `actions/attest`. Every other step, the registry login and the
    tagging included, is shell and `gh`. It stays that way while it holds these permissions.

11. **A dry run attests nothing.** `publish` runs only when the run publishes, so a dry run's
    artifacts never receive provenance that a release could later be confused with.

12. **Nothing is published that does not verify.** After attesting and before `gh release create`,
    `publish` runs `gh attestation verify` on every subject, with `--repo` set to this repository,
    `--signer-workflow` set to `<repository>/.github/workflows/release.yml` and
    `--deny-self-hosted-runners`. A file that does not verify fails the job, and no release is
    created.

### The Server's image

13. **One Containerfile, no compiler in it.** `packaging/container/Containerfile` copies a binary
    that already exists, `server-linux-${TARGETARCH}`, to `/usr/local/bin/server`. It runs no
    `RUN` step, so a multi-architecture build needs no emulation and the image holds exactly the
    binary the release built, tested for its version and described by its SBOM. A developer builds
    the image the same way after `cargo build --release -p fleet-server --bin server`.

14. **The release builds the Server for both Linux targets.** The `build` rows for
    `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` also run
    `cargo build --release -p fleet-server --bin server --target <triple> --locked`, assert that
    `server --version` reports the release's version, and upload the binary to the run. The binary
    is not a release asset of its own; the image is how it ships.

15. **The runtime is `gcr.io/distroless/cc-debian13:nonroot`, pinned by digest.** It carries glibc,
    `libgcc_s`, CA certificates and time zone data, and no shell and no package manager. Debian
    13's glibc is newer than the build runner's, so the binary built there runs on it. The `FROM`
    line names the tag and its digest; a new digest arrives by a reviewed commit.

16. **The image runs unprivileged, and configures nothing that holds a secret.**
    - It runs as UID and GID `65532`, set numerically with `USER 65532:65532`. It needs no
      capability: both planes listen on ports above 1024.
    - It sets `FLEET_SERVER__DATA_DIR=/var/lib/opamp-fleet`, `FLEET_SERVER__LISTEN=0.0.0.0:4320`
      and `FLEET_SERVER__REST__LISTEN=0.0.0.0:4321`, declares `/var/lib/opamp-fleet` a volume
      owned by `65532`, and exposes `4320` and `4321`. Its working directory is `/`, which that
      account cannot write, so the `server.toml` the Server reads by default is never a file it
      could have written to its own volume. It sets nothing else.
    - It contains no `server.toml`, no certificate, no key and no credential. Off the loopback the
      Server refuses to start without TLS and without `[rest.auth]`, so an image started with
      nothing mounted fails, naming the missing key, rather than serving in the clear or without
      authentication (Q-1).
    - Certificates, keys, the PIN, password and credential files are mounted as files, and their
      paths are given through the environment ([ADR-0030](0030-the-servers-configuration.md)). The
      image works with a read-only root filesystem.

17. **No `HEALTHCHECK`; an orchestrator probes TCP.** The image has no shell and the Server has no
    health endpoint, so the image declares no `HEALTHCHECK`. The documented probe is a TCP
    connection to `4320` for liveness and readiness. A health endpoint is a later decision.

18. **Tags are exact versions; the digest is the reference.** The image is tagged with the
    release's version, such as `0.7.0`, and with nothing else: no `latest`, no `0.7`. A tag is set
    only after the image has verified (clause 19). The release notes name the image by its digest,
    and the documentation tells operators to deploy by digest. The image carries the OCI
    annotations `org.opencontainers.image.source`, `.version`, `.revision`, `.licenses`,
    `.base.name` and `.base.digest`.

19. **The image is built and pushed untagged by a job of its own, and tagged after it verifies.**
    - A job `image` runs after `build`. It downloads the two Server binaries, builds the image for
      `linux/amd64` and `linux/arm64` with `docker buildx` from a shell step, on a BuildKit builder
      whose image is pinned by version and digest and raised by a reviewed commit, and checks the
      `linux/amd64` image on its own runner: its `server --version` reports the release's version,
      its user is `65532`, and it has no `/bin/sh`. The `arm64` binary's version was asserted on
      its native runner in its `build` row, and the image copies it unchanged. On a dry run it
      stops there.
    - On a release it logs in to `ghcr.io` with the job's token, pushes the image by digest and
      without a tag, and hands the index digest to `publish`. Its permissions are `contents: read`
      and `packages: write`, and it uses no action beyond `actions/checkout` and
      `actions/download-artifact`.
    - `publish` attests the digest (clause 9), verifies it (clause 12), and only then tags it with
      the version, with `docker buildx imagetools create` from a shell step. An image that does not
      verify keeps no tag, and no release is created.

20. **The base image's digest is kept current.** `.github/dependabot.yml` has a `docker` entry for
    `packaging/container/`, so a new digest of the base arrives as a pull request. Where Dependabot
    does not read a file named `Containerfile`, the digest is raised by a reviewed commit on the
    same schedule as the packaging tools.

**Out of scope:**

- code signatures inside an asset or on the image: Authenticode on the `.msi`, `rpm` and `dpkg`
  signatures, Apple notarization, a cosign signature;
- the Client verifying an attestation before a self-update; the fleet's package signature
  ([ADR-0027](0027-packages-signatures-and-delivery.md)) stays the only check on that path;
- an SBOM-predicate attestation that binds each archive to its SBOM (`actions/attest` with
  `sbom-path`);
- SLSA Build Level 3 and an isolated builder;
- reproducible builds;
- a VEX document;
- an image of the Client, which stays an operating-system service
  ([ADR-0019](0019-the-installed-service-and-its-state.md));
- a Helm chart, Compose file or Kubernetes manifest for production; a health endpoint; a PKCS#11
  module or other library in the image; images for any target but `linux/amd64` and
  `linux/arm64`; a published Server binary outside the image.

## Alternatives considered

- **Publish with `SHA256SUMS` alone, unsigned and unattested.** Anyone who can replace one asset
  can replace both, and an operator has no way to check that a file came from this repository's
  release workflow at a known commit.
- **Leave provenance to each program.** Provenance for the release's assets is one decision for
  all of them; a property of one program alone would leave the others unchecked.
- **No SBOM for the Server, because it is not a release asset.** The Server ships as an image
  built from the release's binary, and its dependencies are what an operator scans.
- **`.7z` releases, or a split container** (`.tar.gz` on Unix, `.zip` on Windows). The Client
  cannot open a `.zip`; `.7z` is a second container where every other agent ships `.tar.gz`.
- **Hyphen-separated file names with a parsing heuristic.** One separator cannot both occur inside
  a field and delimit it; `otelcol-2-1.0.0-linux-amd64` has no right answer.
- **`name_version_os-arch`**, the tighter grammar. A fourth shape nothing else writes, against a
  convention operators read fluently.
- **A sidecar manifest per artifact.** An operator hands one file to a Server; it must describe
  itself.
- **Per-ecosystem file names** (`…-1.2.3-1.x86_64.rpm`). Nothing resolves an RPM by its file name;
  the ecosystem vocabulary survives inside the package metadata.
- **Sign every asset with `cosign sign-blob`.** This uses the same Sigstore keyless signing and
  needs no GitHub to verify. It adds a third-party installer action, a `.sigstore.json` bundle per
  asset, and a verification command with `--certificate-identity` and `--certificate-oidc-issuer`
  that operators type by hand. It signs bytes without saying how they were built. The attestation
  covers both, and its bundle is still checkable with `cosign` (recalled, not re-read). Rejected.
- **The SLSA generator's reusable workflow** (`slsa-framework/slsa-github-generator`,
  `generator_generic_slsa3.yml`). This reaches Build Level 3: the provenance is signed in a
  reusable workflow that the calling job cannot influence. It must be referenced by its version
  tag, not by a SHA, because its verifier checks the builder's ref (recalled, not re-read). That
  contradicts the SHA pinning of third-party workflows
  ([ADR-0010](0010-actions-are-pinned-and-kept-current-by-dependabot.md)). It also adds a
  `.intoto.jsonl` asset and `slsa-verifier` as the operator's tool. Rejected; Level 3 is left
  open.
- **`actions/attest-build-provenance@v4` instead of `actions/attest@v4`.** As of v4 it is a wrapper
  over `actions/attest`, and its README tells new implementations to use `actions/attest`. It makes
  the same attestation. Not chosen, so that the release does not depend on a wrapper kept for
  compatibility.
- **Attest in each `build` row.** This is the pattern GitHub documents, attesting where the file is
  made. It gives `id-token: write` to the jobs that run every build script of the dependency graph,
  on five runners. A build script could then attest bytes of its own choosing. Rejected for
  clause 10.
- **Attest in a separate job between `build` and `publish`.** This keeps `id-token` away from
  `contents: write`. `SHA256SUMS` would then be built there and handed on as another artifact.
  `publish` runs no repository code either, so the separation protects nothing. Not chosen.
- **SPDX instead of CycloneDX.** GitHub's dependency graph exports SPDX, but for the whole
  repository and not per target. `cargo-sbom` writes SPDX per crate. `actions/attest` and the
  common scanners (Grype, Trivy, Dependency-Track) read both. `cargo-cyclonedx` is maintained by
  the CycloneDX project itself, resolves a single target, and describes a single binary. Not
  chosen.
- **One SBOM for the workspace.** One file is simpler to produce. It would list the dependencies of
  every program, the test stubs, dev-dependencies and the crates of every other target, and a
  scanner would report findings for code no asset contains. Rejected.
- **An SBOM per row, written in the `build` job.** It lies next to the build. It would install the
  tool on five runners, Windows and macOS included, while the dependency graph needs no runner of
  the target. Not chosen.
- **`taiki-e/install-action` for `cargo-cyclonedx`.** Its manifest lists `cargo-cyclonedx` 0.5.9
  with a checksum, and a prebuilt binary is faster. The release workflow's rule for the tools that
  write what ships is `cargo install --locked` at an exact version, built from crates.io under the
  tool's own lockfile. A second rule for one tool is not worth the minutes saved. Not chosen.
- **No SBOM.** An operator could derive one from the tagged source. Doing so needs a Rust
  toolchain, this repository's checkout and knowledge of which binary ships, and its result is not
  attested. Rejected.
- **A multi-stage build that compiles the Server inside the image.** It needs nothing but the
  Containerfile, but the image's binary is then a second build, not the one the `build` row
  checked for its version and the `sbom` job described, and an `arm64` image would compile under
  emulation for an hour. Rejected; the Containerfile copies the release's binary.
- **A musl build on `distroless/static`.** A static binary needs no glibc and the base would be
  smaller. The release builds GNU binaries for the Client and the operator tool, and a third
  toolchain target with its own allocator and DNS resolver for one image is more to keep than the
  few megabytes it saves. Rejected for now.
- **`debian:13-slim` or Alpine as the base.** A shell and a package manager help debugging, but
  they are what an attacker with code execution in the container uses next, and every package in
  them is a CVE to track. Rejected; `distroless` has a `debug` variant for local debugging.
- **`cc-debian12`.** The build runner's glibc is newer than Debian 12's, so the binary might not
  load. Rejected for `cc-debian13`.
- **A `latest` tag, or a floating `0.7` tag.** Convenient, but a deployment then moves without a
  change in its own configuration, and the attestation names a digest, not a tag. Rejected.
- **A `HEALTHCHECK` with a probe binary in the image.** It needs a second program in the image or
  a shell. An orchestrator's TCP probe needs neither. Rejected until the Server has a health
  endpoint.
- **Push and tag the image in the `image` job.** Simpler, but the tag would exist before the
  image's attestation verified, against clause 12. Rejected.
- **A cosign signature on the image beside the attestation.** It adds a second signing scheme and a
  third-party action for what `gh attestation verify` already proves: which workflow built which
  digest from which commit. Rejected.
- **Docker Hub instead of `ghcr.io`.** It needs an account and a token outside GitHub; the job's
  own token reaches `ghcr.io`. Rejected.
- **Change asset names or containers without saying so.** Operators fetch release assets by name
  from their own scripts, and such a change breaks them on the update that takes it. A release that
  changes them names the old and the new form in `CHANGELOG.md` and in its notes (clause 3).
  Rejected.

## Sources / Prior art

-
  [`opentelemetry-collector-releases`](https://github.com/open-telemetry/opentelemetry-collector-releases)
  and [Install the Collector on
  Linux](https://opentelemetry.io/docs/collector/install/binary/linux/)
  — per-platform archives plus `.deb`, `.rpm`, `.msi`, named `otelcol_<version>_linux_<arch>`.
- [Elastic Agent install
  documentation](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent)
  — native packages beside archives, and archives as what its fleet upgrades from.
- [GoReleaser — Archives](https://goreleaser.com/customization/archive/) — the default
  `{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`.
- [Debian FAQ, package basics](https://www.debian.org/doc/manuals/debian-faq/pkg-basics.en.html)
  and [dpkg-name(1)](https://www.man7.org/linux/man-pages/man1/dpkg-name.1.html);
  [Semantic Versioning 2.0.0](https://semver.org/) — `-` is in a version's alphabet, `_` is not.
- [The Cargo Book, environment
  variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html)
  — `--target` builds the triple while the packer stays a host build.
- GitHub, *Using artifact attestations to establish provenance for builds* —
  <https://docs.github.com/en/actions/security-guides/using-artifact-attestations-to-establish-provenance-for-builds>.
  Recalled, not re-read. And
  [Using artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations):
  `subject-name` without a tag, `subject-digest`, `push-to-registry: true`, `packages: write`, and
  `gh attestation verify oci://…`.
- `actions/attest` README at `v4` (latest `v4.2.2`), read 2026-10-18 —
  <https://github.com/actions/attest>. It documents:
  - the permissions `id-token: write` and `attestations: write`. The `artifact-metadata: write` it
    also lists is for storage records, which need `push-to-registry` and an organization-owned
    repository, so it is not needed for the file attestation;
  - `subject-path` with globs and lists, up to 1024 subjects in one attestation;
  - `subject-checksums`, and the SBOM mode;
  - public-good Sigstore for public repositories;
  - availability on private repositories only under Enterprise Cloud.
- `actions/attest-build-provenance` README at `v4` (latest `v4.2.2`), read 2026-10-18: "As of
  version 4, `actions/attest-build-provenance` is simply a wrapper on top of `actions/attest`."
- `gh attestation verify --help`, gh 2.102.0, read 2026-10-18. The SLSA v1 predicate is the
  default, and the help documents `--repo`, `--signer-workflow`, `--deny-self-hosted-runners`,
  `--source-digest`, `--source-ref`, `--bundle` and `--custom-trusted-root` for offline
  verification.
- SLSA v1.0, *Build provenance* — <https://slsa.dev/spec/v1.0/provenance>. Recalled.
- CycloneDX 1.5 specification and its file-name recommendation `*.cdx.json` —
  <https://cyclonedx.org/specification/overview/>. Recalled.
- `cargo-cyclonedx` README and `src/generator.rs`, read 2026-10-18 —
  <https://github.com/CycloneDX/cyclonedx-rust-cargo>. They document:
  - `--describe binaries`, `--target`, `--format json` and `--spec-version` 1.3 to 1.5 (default
    1.3);
  - the output name `<bin>_bin.cdx.json` beside the package's `Cargo.toml`;
  - that it runs `cargo metadata`, which can rewrite an outdated `Cargo.lock`.

  crates.io lists 0.5.9 (2026-03-19, MSRV 1.85) as the latest; the toolchain is 1.99.0.
- NTIA, *The Minimum Elements For a Software Bill of Materials* (2021), and the EU Cyber
  Resilience Act, Annex I. Recalled, not re-read.
- [Distroless README](https://github.com/GoogleContainerTools/distroless/blob/main/README.md): the
  Debian 13 images `static-debian13`, `base-debian13` and `cc-debian13`, the `nonroot` and `debug`
  tags, no shell, keyless cosign signatures;
  [cc](https://github.com/GoogleContainerTools/distroless/blob/main/cc/README.md) (base plus
  `libgcc1`) and
  [base](https://github.com/GoogleContainerTools/distroless/blob/main/base/README.md) (glibc,
  libssl, CA certificates, tzdata, `/tmp`).
- OCI Image Format,
  [Pre-Defined Annotation Keys](https://github.com/opencontainers/image-spec/blob/main/annotations.md).
- [dependabot-core issue 6067](https://github.com/dependabot/dependabot-core/issues/6067) and
  [Chainguard on Dependabot and image digests](https://edu.chainguard.dev/chainguard/containers/staying-secure/updating-images/dependabot/):
  Dependabot's support for a file not named `Dockerfile` is not established, hence clause 20's
  fallback.
- The code: the `build`, `sbom` and `publish` jobs of `.github/workflows/release.yml`,
  `DEFAULT_LISTEN` and `DEFAULT_REST_LISTEN` in `crates/fleet-server/src/config.rs`, and the
  absence of a health route in `crates/fleet-server/src/api.rs`.

## Consequences

- Positive: the Client's release archive is installable by the fleet unmodified, so the hash
  published is the hash an Agent verifies; a build that lost its tags fails instead of publishing
  a `-dev` artifact. The Client's release is an ordinary fleet package in the container every
  agent uses.
- Positive: a file name parses back into its four fields, and the fleet view reads upstream
  artifacts and ours with one code path.
- Positive: an operator can prove, with one command, that a downloaded asset or the Server's image
  was built by this repository's `release.yml` at a named commit on a GitHub-hosted runner. This
  includes the archive they hand their Server and the tool that makes their keys. `SHA256SUMS`
  becomes trustworthy once it verifies. A scanner gets the exact dependency set of the binary on
  the platform the operator runs, the Server's included.
- Positive: the image starts only with TLS and authentication configured, runs unprivileged with no
  shell, and carries no secret.
- Negative / trade-offs: a platform is spelled `linux_amd64` in a file name and `linux-amd64` as a
  tag; the version in a file name is the base version, so the name is not a unique build
  identifier. Five targets, of which two are not what their runner natively is.
- Negative / trade-offs: the attestation proves where and from what a file was built, not that the
  source is benign. It cannot be revoked, and an attested file stays attested. Verifying needs `gh`
  and network access to GitHub, or for offline verification a downloaded bundle and trusted root.
  The Rekor log records the digests and the workflow identity publicly, which a public repository
  already shows. No asset carries a code signature: Windows shows an unknown publisher and `rpm`
  reports no signature.
- Negative / trade-offs: the `version` job pushes the tag before anything is built
  ([ADR-0033](0033-the-products-version.md)). An attestation or verification failure in `publish`
  leaves a pushed tag without a release. Re-running the failed job publishes it. A new run refuses
  the spent number, as it does for any `publish` failure.
- Negative / trade-offs: a script that downloads `supervisor_<version>_*` without an extension also
  matches the SBOMs. The notes' own upload loop and `package fetch` select `.tar.gz` only.
- Negative / trade-offs: the release builds the Server twice more, pushes to a registry, and
  `publish` holds `packages: write`. The registry is a second place a release appears. The base
  image's own packages are not in the SBOM; they are named by the base digest and tracked by its
  updates. No shell in the image makes debugging inside it harder; the `debug` variant of the base
  is for that, locally.
- Follow-ups: an SBOM attestation per archive; the Client checking an attestation on self-update;
  SLSA Build Level 3; code signatures for the installers; a health endpoint and a readiness probe
  on it; a musl build on `static`; production deployment examples; whether Dependabot updates the
  `Containerfile`'s digest.

## Enforcement

- [`release.yml`](../../.github/workflows/release.yml): the version job's "The binary agrees" step
  and each target's "The binary agrees with the name it will be given" (clause 1); the pack step
  (clauses 2, 3); "The RPM carries its scriptlets, not their file names", which asserts with
  `rpm -qp --scripts` that the RPM's architecture is the ecosystem's (clause 3).
- Tests in `crates/fleet-tools/tests/release_workflow.rs` that cite this ADR read the workflow and
  the Containerfile as text:
  - **Permissions.** `id-token: write` and `attestations: write` each occur exactly once, inside
    the `publish` job's `permissions:`. The top-level `permissions:` is `contents: read` alone
    (clause 10).
  - **The `publish` job.** It has no `actions/checkout`, no `cargo` and no `uses:` other than
    `actions/download-artifact@` and `actions/attest@`. Its steps run in this order: `Collect the
    checksums`, the attestation, the verification, `Publish the release`. The attestation's
    `subject-path` names `*.tar.gz`, `*.deb`, `*.rpm`, `*.msi`, `*.cdx.json` and `SHA256SUMS`
    (clauses 9, 10). Its image attestation step has `subject-name`, `subject-digest` and
    `push-to-registry: true`; its verification runs `gh attestation verify oci://` with
    `--signer-workflow`; the tagging step comes after the verification and before `Publish the
    release` (clauses 9, 10, 19).
  - **Verification.** The verification step runs `gh attestation verify` with `--signer-workflow`
    and `--deny-self-hosted-runners` over the same set (clause 12). `gh release create` publishes
    `*.cdx.json` (clause 5).
  - **The `sbom` job.** It `needs: version`, has no `if:` and no `permissions:` beyond the top
    level, and installs `cargo-cyclonedx@<MAJOR.MINOR.PATCH>` with `cargo install --locked`. For
    every matrix row it writes `supervisor_…_<os>_<arch>.cdx.json`, for every
    `operator_tool: true` row `opamp-fleetctl_…_<os>_<arch>.cdx.json`, and for both Linux rows
    `server_…_linux_<arch>.cdx.json`, each with `--describe binaries --target <triple>
    --spec-version 1.5`, beside a `.sha256`. It fails when `Cargo.lock` changed (clauses 7, 8).
  - **The notes.** The release notes contain `gh attestation verify` and `--signer-workflow`
    (clause 5).
  - **The Containerfile.** `packaging/container/Containerfile` has exactly one `FROM`, naming
    `gcr.io/distroless/cc-debian13:nonroot@sha256:`, no `RUN`, `USER 65532:65532`, no
    `HEALTHCHECK`, a last `WORKDIR` of `/`, and sets no `FLEET_SERVER__` variable beyond
    `DATA_DIR`, `LISTEN` and `REST__LISTEN` (clauses 13, 15, 16, 17).
  - **The build rows.** Both Linux rows build `-p fleet-server --bin server` with `--locked` and
    assert its version (clause 14).
  - **The `image` job.** Its permissions are `contents: read` and `packages: write` alone, it has
    no `id-token` and no `attestations`, its builder's image is pinned by digest, it runs the
    version, user and shell checks, and it pushes no tag (clauses 10, 19).
- [`scripts/check-build-hardening.sh`](../../scripts/check-build-hardening.sh) check 3 fails on a
  `cargo install` in `release.yml` without an exact version, which covers the tool of clause 8.
  Its check 2 fails on a cache in `release.yml`.

**Not mechanically decidable:** that a real release's assets and the published image's digest
verify on GitHub, that each SBOM is complete for its binary, and that the image runs on both
architectures. The first is held at release time by clause 12's step in `publish`, which fails the
release and leaves the image untagged. The second is held by review of a release's SBOMs against
`cargo tree --target <triple> -e normal -p <package>`. The third is held for `amd64` by the `image`
job's check and for `arm64` by its `build` row's version check of the same binary; that the
`arm64` image starts is checked by hand on the first release. The fleet view's reading of a file
name (clause 4) has no automated test. That a release changing the asset names or containers
names the change in `CHANGELOG.md` and its notes (clause 3) is held by review.
