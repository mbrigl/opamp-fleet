# OPS-releases-attestations-server-image: Releases, attestations and the Server's image

- **Status:** ⚪ superseded by OPS-releases-image-probed-by-plane
- **Date:** 2026-11-21
- **Deciders:** Markus Brigl
- **Applies to:** `.github/workflows/release.yml` (the `version`, `build`, `sbom`, `image` and `publish` jobs, their permissions, the release files they attach and the release notes), `packaging/container/Containerfile`, the image `ghcr.io/mbrigl/opamp-fleet-server`, `.github/dependabot.yml`, `crates/fleet-tools/tests/release_workflow.rs`, the names of every release file, the fleet view's reading of an artifact's file name, and every document that tells an operator how to download, check or run a release file or the Server's image (`README.md`, `docs/manual/`, `CHANGELOG.md`)

## Context

An operator needs something to install, and a fleet needs something to hand its Server. These are
two questions:

- The fleet needs one artifact per platform that the Client can open and install as a self-update
  ([CLT-self-update-signed-package](CLT-self-update-signed-package.md)).
- A host needs what its platform expects: native packages, which
  [CLT-installed-service-and-state](CLT-installed-service-and-state.md) decides.

`opentelemetry-collector-releases` and Elastic Agent both ship native packages *beside* their
archives, for that reason. The names of the files of a release are a public contract: operators
script against them.

`SHA256SUMS` proves only that a download matches the file next to it on the same release page.
Whoever can replace one file of a release can replace both. Two files carry more weight than the
rest:

- the Client's archive, which a Server hands to a whole fleet as a self-update. The fleet's package
  signature ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)) is made over
  whatever the operator downloaded;
- the operator tool, which makes the fleet's certificate authorities and its signing key
  ([OPS-operator-program-fleetctl](OPS-operator-program-fleetctl.md)).

An operator who wants to know that a file came from this repository's release workflow, at a known
commit, needs a way to check it.

GitHub artifact attestations answer that question. A workflow job with `id-token: write` gets a
short-lived Sigstore certificate bound to its workflow identity. With it, the job signs an in-toto
statement with a SLSA v1 build provenance predicate over the files' SHA-256 digests. GitHub stores
the statement in the repository's attestation store. For a public repository such as
`mbrigl/opamp-fleet`, the public Rekor transparency log records it too.
`gh attestation verify <file> --repo <repository>` checks it. A public repository has this on every
plan; a private one needs GitHub Enterprise Cloud.

A software bill of materials (SBOM) lists what a binary is built from. An operator runs it through
their own vulnerability scanner without rebuilding. Regulators ask for one: the EU Cyber Resilience
Act's Annex I asks a manufacturer to draw one up. The shipped programs depend on different crates
on different targets: `windows-sys` and the service code on Windows, other code on Linux and macOS.
A scanner fed with crates that are not in the binary reports findings that do not apply.

The Server runs in a container:

- Its whole configuration may come from the environment, with every secret a file whose path the
  environment names
  ([SRV-configuration-toml-and-environment](SRV-configuration-toml-and-environment.md)).
- It has no health endpoint.
- Its two planes listen on `127.0.0.1:4320` and `127.0.0.1:4321` by default. Off the loopback both
  require TLS, and the Operator plane requires the operator's credential, configured as
  `[rest.auth]` ([SEC-tls13-two-planes-bounded-input](SEC-tls13-two-planes-bounded-input.md)).
- The release workflow builds Linux binaries for `x86_64-unknown-linux-gnu` and
  `aarch64-unknown-linux-gnu` on GitHub-hosted runners. It uses the pinned toolchain
  (`rust-toolchain.toml`) and the committed lockfile, without a cache. A binary for a GNU target
  links glibc and `libgcc_s` dynamically.

A common container image compiles the program in a multi-stage `Dockerfile`, carries a `latest`
tag and declares a `HEALTHCHECK`. This record decides against all three, so a reader who expects
them finds the reasons here.

Constraints from other decisions and from the specification:

- Third-party actions are pinned to a SHA, and a first-party action (`actions/*`) stays on its
  major tag ([OPS-actions-pinned-by-sha](OPS-actions-pinned-by-sha.md)).
- The release workflow installs every tool that writes what ships at an exact version, raised by a
  reviewed commit (`cargo install --locked cargo-deb@3.8.0 …`).
  [`scripts/check-build-hardening.sh`](../../scripts/check-build-hardening.sh) check 3 holds that
  rule.
- The workflow keeps tokens away from code it does not control. The `advisories` job runs apart
  from the job that can write the release, and no checkout persists credentials. The build script
  of every dependency runs in `build`. A job holding `id-token: write` could mint an attestation for
  any bytes it chose.
- Q-1 and Q-3 of [`docs/SPECIFICATION.md`](../SPECIFICATION.md) apply to every file of a release,
  the image included. Nothing it starts may send a credential unencrypted beyond the loopback.
  Nothing may enter it without the supply-chain checks a dependency passes.

## Decision

We will make each release from one workflow run, attest the build provenance of all it ships, and
release nothing that does not verify. The run ships:

- one `.tar.gz` per target named `supervisor_<version>_<os>_<arch>`, the fleet's artifact;
- the native installers beside them;
- the operator tool's archives;
- one CycloneDX SBOM per shipped program and target;
- a multi-architecture image of the Server, built from its release binary.

### The release and its files

1. **A release is one workflow run that builds five targets from one version.**
   - [OPS-cargo-decides-product-version](OPS-cargo-decides-product-version.md) decides the version
     and its `version/*` tag.
   - A first job decides the number, builds the Client and asks it `--version`. Every artifact is
     named from that one answer.
   - Each target's build asserts that its own binary reports the same full version. The
     cross-compiled macOS x86_64 build is exempt, because its runner cannot execute it.
   - Every job checks out with full history, so the baked version is the tag's.
   - A `workflow_dispatch` dry run is the default. It builds and packs everything and uploads it to
     the workflow run. It sets no tag and releases nothing.

   | target | runner | `<os>` | `<arch>` |
   |---|---|---|---|
   | `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `linux` | `amd64` |
   | `aarch64-unknown-linux-gnu` | `ubuntu-24.04-arm` | `linux` | `arm64` |
   | `aarch64-apple-darwin` | `macos-latest` | `darwin` | `arm64` |
   | `x86_64-apple-darwin` | `macos-latest` (cross) | `darwin` | `amd64` |
   | `x86_64-pc-windows-msvc` | `windows-latest` | `windows` | `amd64` |

   Both Linux architectures ship, because arm64 servers are ordinary. Both macOS architectures ship,
   because Intel Macs are still in use. The arm runner cross-compiles the Intel build, so the
   release does not depend on an Intel runner label that keeps being retired. **Windows on arm64 is
   out** until an operator asks for it. Adding it is one row.

2. **Every archive is a `.tar.gz` written by `opamp-fleetctl package pack --format tar.gz`.** The
   same run builds and releases that operator tool
   ([OPS-operator-program-fleetctl](OPS-operator-program-fleetctl.md)).
   - The packer names the single member after the program and gives it its executable mode.
   - So **the Client's release archive is a valid package artifact unmodified**. The operator
     uploads the file they downloaded.
   - The SHA-256 the packer prints is both the checksum the release lists and the hash an Agent
     verifies ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)).
   - `.tar.gz` is the container every other agent's package ships as. It carries the executable bit
     and unpacks the same way on every platform.
   - `.7z` remains a package container that the Client opens and the packer writes, encrypted too
     ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)). A release has no use
     for encryption.

3. **Every file of the Client's release is named `supervisor_<version>_<os>_<arch>.<ext>`.** The
   operator tool's files follow the same grammar under its own name
   ([OPS-operator-program-fleetctl](OPS-operator-program-fleetctl.md)).
   - The name has four fields separated by `_`: the package name, the base version
     `MAJOR.MINOR.PATCH` without build metadata, and the platform as `<os>` and `<arch>`.
   - The platform uses the vocabulary an Agent reports as `os.type` and `host.arch`
     ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)).
   - **All artifacts of a target share the stem** and differ in extension alone:

     | | artifact |
     |---|---|
     | archive | `supervisor_1.2.3_linux_amd64.tar.gz` |
     | Debian | `supervisor_1.2.3_linux_amd64.deb` |
     | RPM | `supervisor_1.2.3_linux_amd64.rpm` |
     | Windows | `supervisor_1.2.3_windows_amd64.msi` |

   - `_` is the one separator no field contains. The name grammar is `[a-z0-9-]`, and SemVer spells
     a pre-release with `-`. Neither admits `_`.
   - `_` is also the shape an operator already reads: GoReleaser's default, the Collector's own
     `otelcol_<version>_linux_amd64.tar.gz`, and Debian's `<package>_<version>_<arch>.deb`.
   - Where a platform is a *tag* rather than a field of a file name, it stays `<os>-<arch>`, for
     the mirror-image reason. Examples are the store's `<os>-<arch>.bin` and the fleet view's
     platform column.
   - Inside the `.deb` and `.rpm` the architecture is the ecosystem's own (`amd64`/`arm64`,
     `x86_64`/`aarch64`). The packaging tool derives it. Nothing resolves a package by its file
     name.
   - **A released file is never renamed or rewritten.** A checksum listed against a URL stays true.
   - **A release that changes the names or containers of its files says so.** The change breaks
     operators' scripts. `CHANGELOG.md` and the notes of the release that carries it must name the
     old and the new form.

4. **A file name is read from the right, and an unknown tail fills nothing.**
   - The upload form of the fleet view matches the platform tokens it knows at the end of the stem,
     separated by `_` or `-`. It knows the aliases `x86_64`, `aarch64` and `macos` too.
   - The version starts at the first separator followed by a digit. The rest is the name.
   - A tail the form does not know fills no field rather than a guess. A wrong platform makes an
     Entry that no Agent is ever offered.
   - The form accepts `-` for upstream artifacts named by upstream.
   - The upload loop of the release notes selects the `supervisor_` stem and splits the rest on
     `_`. It needs no platform from the operator.

5. **What a release holds.** A GitHub release named after the version holds:
   - the Client's five archives, the two `.deb`, the two `.rpm` and the `.msi`;
   - the four archives of the operator tool
     ([OPS-operator-program-fleetctl](OPS-operator-program-fleetctl.md));
   - the eleven SBOMs (clause 7);
   - one `SHA256SUMS` covering every file of the release.

   Beside it, `ghcr.io/mbrigl/opamp-fleet-server:<version>` holds the Server's image. Every file of
   the release is a subject of the release's build-provenance attestation. The image has an
   attestation of its own (clause 9). The notes say:
   - which file is for which purpose, and the image's name and digest;
   - the per-platform install commands, and how to run the image;
   - how to verify a file:
     `gh attestation verify <file> --repo <repository> --signer-workflow
     <repository>/.github/workflows/release.yml`, then `sha256sum -c --ignore-missing SHA256SUMS`
     once `SHA256SUMS` has verified;
   - how to verify the image:
     `gh attestation verify oci://ghcr.io/mbrigl/opamp-fleet-server@<digest>
     --repo <repository> --signer-workflow <repository>/.github/workflows/release.yml`;
   - the procedure that uploads the Client's archives, selected by their `supervisor_` stem, as one
     Package for Agent type `supervisor`;
   - the full baked version string, as provenance.

   The upload uses the release number, because versions are compared without build metadata
   ([OPS-cargo-decides-product-version](OPS-cargo-decides-product-version.md)).

6. **The operator tool's archives are attested like the Client's artifacts.**
   - They carry what every file of a release carries: their entry in `SHA256SUMS` and the release's
     build-provenance attestation (clause 9).
   - Each archive has its program's SBOM for its target beside it (clause 7).
   - Provenance is one decision for every file of the release, this one. It is not a property of
     this program alone.
   - No archive carries a code signature.

### SBOMs and attestations

7. **One CycloneDX SBOM per shipped program and target.** The release holds:
   - `supervisor_<version>_<os>_<arch>.cdx.json` for each of the Client's five targets;
   - `opamp-fleetctl_<version>_<os>_<arch>.cdx.json` for each of the operator tool's four;
   - `server_<version>_linux_<arch>.cdx.json` for each of the Server's two.

   The stem of the first two is the stem of the archive the SBOM describes (clause 3). The
   Server's names the binary inside the image of that architecture. `.cdx.json` is the extension
   the CycloneDX specification recommends. Each SBOM is CycloneDX 1.5 JSON. `cargo cyclonedx`
   writes it with `--describe binaries` and `--target <triple>`, for:
   - the one binary that ships: `supervisor`, `opamp-fleetctl` or `server`;
   - the default features, which the release builds with;
   - every transitive dependency on that target, resolved from the committed `Cargo.lock`.

   The lockfile must be unchanged afterwards. The `.deb`, `.rpm` and `.msi` of a target package the
   binary of its archive unchanged. So the archive's SBOM describes them, and they get none of their
   own. The image's base layers are Debian packages that `cargo cyclonedx` does not see. The image's
   SBOM is the Server's, and the image names its base by digest (clause 18).

8. **A job of its own writes the SBOMs, with an exactly pinned tool.**
   - The job `sbom` of `release.yml` runs on `ubuntu-latest` after `version`.
   - It writes every SBOM of clause 7. `cargo cyclonedx --target` reads the dependency graph for a
     target without building for it.
   - It writes a `.sha256` beside each SBOM and uploads them to the run, as the `build` rows upload
     theirs.
   - It runs on a dry run too, so every dry run exercises it.
   - Its token may read and do nothing else. Its checkout keeps no credentials. It restores no
     cache.
   - It installs the tool with `cargo install --locked cargo-cyclonedx@X.Y.Z`. The version is
     exact and a reviewed commit raises it, as for the packaging tools. No action installs it.

9. **One build-provenance attestation covers every file of the release, and one covers the image.**
   - After `SHA256SUMS` is assembled and before the release is created, the `publish` job runs
     `actions/attest@v4` in provenance mode, on its major tag
     ([OPS-actions-pinned-by-sha](OPS-actions-pinned-by-sha.md)).
   - Its subjects are every file the job then releases and `SHA256SUMS` itself. The files are the
     `.tar.gz` archives, the `.deb`, `.rpm` and `.msi`, and the `.cdx.json` SBOMs.
   - A second `actions/attest@v4` step attests the image. Its `subject-name` is
     `ghcr.io/mbrigl/opamp-fleet-server`, and its `subject-digest` is the index digest from `image`.
     It sets `push-to-registry: true`, so the attestation travels with the image.
   - The step computes the digests of the files from the files themselves, not from `SHA256SUMS`.
   - The per-file `.sha256` files are folded into `SHA256SUMS` and not released. So they are not
     subjects.
   - Each attestation is signed through the Sigstore public-good instance, which this public
     repository gets. Rekor records it, and the repository's attestation store keeps it.

10. **Only the `publish` job may attest.**
    - `publish` alone holds `id-token: write` and `attestations: write`. Beside them it holds
      `contents: write` and `packages: write`. It needs `packages: write` to store the image's
      attestation in the registry and to tag the image.
    - Neither of the two attesting permissions appears at the workflow's top level, which stays
      `contents: read`, nor on any other job. `image` holds `packages: write` without either.
    - `publish` checks out no code and runs no build.
    - The only actions it runs are `actions/download-artifact` and `actions/attest`. Every other
      step, the registry login and the tagging included, is shell and `gh`.
    - It must stay that way while it holds these permissions.

11. **A dry run attests nothing.** `publish` runs only when the run makes a release. So a dry run's
    artifacts never receive provenance that anyone can later confuse with a release's.

12. **Nothing is released that does not verify.** After attesting and before `gh release create`,
    `publish` runs `gh attestation verify` on every subject, with:
    - `--repo` set to this repository;
    - `--signer-workflow` set to `<repository>/.github/workflows/release.yml`;
    - `--deny-self-hosted-runners`.

    A file that does not verify fails the job, and no release is created.

### The Server's image

13. **One Containerfile, with no compiler in it.**
    - `packaging/container/Containerfile` copies a binary that already exists,
      `server-linux-${TARGETARCH}`, to `/usr/local/bin/server`.
    - It runs no `RUN` step. So a multi-architecture build needs no emulation.
    - The image holds exactly the binary the release built, tested for its version and described by
      its SBOM.
    - A developer builds the image the same way after
      `cargo build --release -p fleet-server --bin server`.

14. **The release builds the Server for both Linux targets.**
    - The `build` rows for `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu` also run
      `cargo build --release -p fleet-server --bin server --target <triple> --locked`.
    - They assert that `server --version` reports the release's version, and upload the binary to
      the run.
    - The binary is not a file of the release on its own. The image is how it ships.

15. **The runtime is `gcr.io/distroless/cc-debian13:nonroot`, pinned by digest.**
    - It carries glibc, `libgcc_s`, CA certificates and time zone data. It has no shell and no
      package manager.
    - Debian 13's glibc is newer than the build runner's, so the binary built there runs on it.
    - The `FROM` line names the tag and its digest. A new digest arrives by a reviewed commit.

16. **The image runs unprivileged and configures nothing that holds a secret.**
    - It runs as UID and GID `65532`, set numerically with `USER 65532:65532`. It needs no
      capability: both planes listen on ports above 1024.
    - It sets `FLEET_SERVER__LISTEN=0.0.0.0:4320` and `FLEET_SERVER__REST__LISTEN=0.0.0.0:4321`. It
      declares `/var/lib/opamp-fleet` a volume owned by `65532`, and exposes `4320` and `4321`.
    - Its working directory is `/`, which that account cannot write. So the `server.toml` the Server
      reads by default lies outside its volume, where the Server cannot write it.
    - It sets nothing else.
    - It names no data directory. The operator gives `data_dir = "/var/lib/opamp-fleet"` to a Server
      that keeps state on its own disk, in `server.toml` or as `FLEET_SERVER__DATA_DIR`. Such a
      Server runs on the filesystem record store or with the filesystem blob store. The declared
      volume is that place.
    - A Server given no data directory falls back to the default `fleet-data`. That path resolves
      under `/`, where `65532` cannot write, so startup fails naming `data_dir`. The Server never
      writes outside the volume.
    - A Server on PostgreSQL and S3 is given no data directory, and the volume stays unused
      ([SRV-state-stores-contract-pair §1](SRV-state-stores-contract-pair.md)).
    - The image contains no `server.toml`, no certificate, no key and no credential.
    - Off the loopback the Server refuses to start without TLS and without `[rest.auth]`. So an
      image started with nothing mounted fails, naming the missing key. It never serves in the
      clear or without authentication (Q-1).
    - Certificates, keys, the PIN, password and credential files are mounted as files. The
      environment gives their paths
      ([SRV-configuration-toml-and-environment](SRV-configuration-toml-and-environment.md)).
    - The image works with a read-only root filesystem.

17. **No `HEALTHCHECK`; an orchestrator probes TCP.**
    - The image has no shell and the Server has no health endpoint. So the image declares no
      `HEALTHCHECK`.
    - The documented probe is a TCP connection to `4320`, for liveness and readiness.
    - A health endpoint is a later decision.

18. **Tags are exact versions; the digest is the reference.**
    - The image is tagged with the release's version, such as `0.7.0`, and with nothing else: no
      `latest`, no `0.7`.
    - A tag is set only after the image has verified (clause 19).
    - The release notes name the image by its digest. The documentation tells operators to run the
      image by digest.
    - The image carries the OCI annotations `org.opencontainers.image.source`, `.version`,
      `.revision`, `.licenses`, `.base.name` and `.base.digest`.

19. **A job of its own builds the image and uploads it untagged; `publish` tags it after it
    verifies.**
    - A job `image` runs after `build`. It downloads the two Server binaries.
    - It builds the image for `linux/amd64` and `linux/arm64` with `docker buildx` from a shell
      step. The BuildKit builder's image is pinned by version and digest, and a reviewed commit
      raises it.
    - It checks the `linux/amd64` image on its own runner. Its `server --version` reports the
      release's version, its user is `65532`, and it has no `/bin/sh`.
    - The `arm64` binary's version was asserted on its native runner in its `build` row. The image
      copies that binary unchanged.
    - On a dry run the job stops there.
    - On a release it logs in to `ghcr.io` with the job's token. It uploads the image to the
      registry by digest and without a tag, and hands the index digest to `publish`.
    - Its permissions are `contents: read` and `packages: write`. It uses no action beyond
      `actions/checkout` and `actions/download-artifact`.
    - `publish` attests the digest (clause 9) and verifies it (clause 12). Only then does it tag the
      image with the version, with `docker buildx imagetools create` from a shell step.
    - An image that does not verify keeps no tag, and no release is created.

20. **The base image's digest is kept current.**
    - `.github/dependabot.yml` has a `docker` entry for `packaging/container/`. So a new digest of
      the base arrives as a pull request.
    - Where Dependabot does not read a file named `Containerfile`, a reviewed commit raises the
      digest. It does so on the same schedule as for the packaging tools.

**Out of scope:**

- code signatures inside a file of the release or on the image: Authenticode on the `.msi`, `rpm`
  and `dpkg` signatures, Apple notarization, a cosign signature;
- the Client verifying an attestation before a self-update. The fleet's package signature
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)) stays the only check on
  that path;
- an SBOM-predicate attestation that binds each archive to its SBOM (`actions/attest` with
  `sbom-path`);
- SLSA Build Level 3 and an isolated builder;
- reproducible builds;
- a VEX document;
- an image of the Client, which stays an operating-system service
  ([CLT-installed-service-and-state](CLT-installed-service-and-state.md));
- a Helm chart, Compose file or Kubernetes manifest for production;
- a health endpoint;
- a PKCS#11 module or other library in the image;
- images for any target but `linux/amd64` and `linux/arm64`;
- a released Server binary outside the image.

## Alternatives considered

- **`SHA256SUMS` alone, unsigned and unattested.** Whoever can replace one file of a release can
  replace both. An operator then has no way to check that a file came from this repository's
  release workflow at a known commit. Rejected.
- **Provenance left to each program.** Provenance for the files of a release is one decision for
  all of them. A property of one program alone would leave the others unchecked. Rejected.
- **No SBOM for the Server, because the release holds no Server binary.** The Server ships as an
  image built from the release's binary, and its dependencies are what an operator scans.
  Rejected.
- **`.7z` releases, or a split container** (`.tar.gz` on Unix, `.zip` on Windows). The Client
  cannot open a `.zip`. `.7z` is a second container where every other agent ships `.tar.gz`.
  Rejected.
- **Hyphen-separated file names with a parsing heuristic.** One separator cannot both occur inside
  a field and delimit it: `otelcol-2-1.0.0-linux-amd64` has no right answer. Rejected.
- **`name_version_os-arch`**, the tighter grammar. It is a fourth shape nothing else writes,
  against a convention operators read fluently. Rejected.
- **A manifest file beside each artifact.** An operator hands one file to a Server, so the file
  must describe itself. Rejected.
- **Per-ecosystem file names** (`…-1.2.3-1.x86_64.rpm`). Nothing resolves an RPM by its file name,
  and the ecosystem's vocabulary lives on inside the package metadata. Rejected.
- **Sign every file with `cosign sign-blob`.** It uses the same Sigstore keyless signing and needs
  no GitHub to verify. It adds a third-party installer action and a `.sigstore.json` bundle per
  file. Its verification command takes `--certificate-identity` and `--certificate-oidc-issuer`,
  which operators type by hand. It signs bytes without saying how they were built. The attestation
  covers both, and `cosign` can still check its bundle (recalled, not re-read). Rejected.
- **The SLSA generator's reusable workflow** (`slsa-framework/slsa-github-generator`,
  `generator_generic_slsa3.yml`). It reaches Build Level 3: a reusable workflow that the calling
  job cannot influence signs the provenance. It must be referenced by its version tag, not by a
  SHA, because its verifier checks the builder's ref (recalled, not re-read). That contradicts the
  SHA pinning of third-party workflows ([OPS-actions-pinned-by-sha](OPS-actions-pinned-by-sha.md)).
  It also adds an `.intoto.jsonl` file and `slsa-verifier` as the operator's tool. Rejected; Level
  3 stays open.
- **`actions/attest-build-provenance@v4` instead of `actions/attest@v4`.** As of v4 it is a wrapper
  over `actions/attest`, and its README tells new implementations to use `actions/attest`. It makes
  the same attestation. Not chosen, so that the release does not depend on a wrapper kept for
  compatibility.
- **Attest in each `build` row.** GitHub documents this pattern: attest where the file is made. It
  gives `id-token: write` to the jobs that run every build script of the dependency graph, on five
  runners. A build script could then attest bytes of its own choosing. Rejected for clause 10.
- **Attest in a separate job between `build` and `publish`.** It keeps `id-token` away from
  `contents: write`, and builds `SHA256SUMS` there as one more artifact to hand on. `publish` runs
  no repository code either, so the separation protects nothing. Not chosen.
- **SPDX instead of CycloneDX.** GitHub's dependency graph exports SPDX, but for the whole
  repository and not per target. `cargo-sbom` writes SPDX per crate. `actions/attest` and the
  common scanners (Grype, Trivy, Dependency-Track) read both formats. `cargo-cyclonedx` is
  maintained by the CycloneDX project itself, resolves a single target, and describes a single
  binary. Not chosen.
- **One SBOM for the workspace.** One file is simpler to produce. It would list the dependencies of
  every program, the test stubs, the dev-dependencies and the crates of every other target. A
  scanner would report findings for code no file of the release contains. Rejected.
- **An SBOM per row, written in the `build` job.** It lies next to the build. It would install the
  tool on five runners, Windows and macOS included, while the dependency graph needs no runner of
  the target. Not chosen.
- **`taiki-e/install-action` for `cargo-cyclonedx`.** Its manifest lists `cargo-cyclonedx` 0.5.9
  with a checksum, and a prebuilt binary is faster. The release workflow's rule for the tools that
  write what ships is `cargo install --locked` at an exact version, built from crates.io under the
  tool's own lockfile. A second rule for one tool is not worth the minutes saved. Not chosen.
- **No SBOM.** An operator could derive one from the tagged source. That needs a Rust toolchain,
  this repository's checkout and the knowledge of which binary ships, and the result is not
  attested. Rejected.
- **A multi-stage build that compiles the Server inside the image.** It needs nothing but the
  Containerfile. But the image's binary is then a second build, not the one the `build` row checked
  for its version and the `sbom` job described. An `arm64` image would also compile under emulation
  for an hour. Rejected; the Containerfile copies the release's binary.
- **A musl build on `distroless/static`.** A static binary needs no glibc, and the base would be
  smaller. The release builds GNU binaries for the Client and the operator tool. A third toolchain
  target, with its own allocator and DNS resolver, for one image is more to keep than the few
  megabytes it saves. Rejected for now.
- **`debian:13-slim` or Alpine as the base.** A shell and a package manager help debugging. They
  are also what an attacker with code execution in the container uses next, and every package in
  them is a CVE to track. Rejected; `distroless` has a `debug` variant for local debugging.
- **`cc-debian12`.** The build runner's glibc is newer than Debian 12's, so the binary might not
  load. Rejected for `cc-debian13`.
- **A `latest` tag, or a floating `0.7` tag.** It is convenient. But a running Server's image then
  moves without a change in its own configuration, and the attestation names a digest, not a tag.
  Rejected.
- **A `HEALTHCHECK` with a probe binary in the image.** It needs a second program in the image or a
  shell. An orchestrator's TCP probe needs neither. Rejected until the Server has a health endpoint.
- **Upload and tag the image in the `image` job.** It is simpler. But the tag would exist before
  the image's attestation verified, against clause 12. Rejected.
- **A cosign signature on the image beside the attestation.** It adds a second signing scheme and a
  third-party action for what `gh attestation verify` already proves: which workflow built which
  digest from which commit. Rejected.
- **Docker Hub instead of `ghcr.io`.** It needs an account and a token outside GitHub. The job's
  own token reaches `ghcr.io`. Rejected.
- **Change the names or containers of a release's files without saying so.** Operators fetch the
  files of a release by name from their own scripts. Such a change breaks them on the update that
  takes it. Rejected; clause 3 has the release name the old and the new form.
- **The image sets `FLEET_SERVER__DATA_DIR=/var/lib/opamp-fleet`.** A Server on PostgreSQL and S3
  keeps nothing on its disk and refuses a data directory its configuration names
  ([SRV-state-stores-contract-pair §1](SRV-state-stores-contract-pair.md)). So the image could not
  run it. Rejected; clause 16 names none, and the operator gives one to a Server that keeps state on
  its disk.

## Sources / Prior art

-
  [`opentelemetry-collector-releases`](https://github.com/open-telemetry/opentelemetry-collector-releases)
  and
  [Install the Collector on Linux](https://opentelemetry.io/docs/collector/install/binary/linux/):
  per-platform archives plus `.deb`, `.rpm`, `.msi`, named `otelcol_<version>_linux_<arch>`.
- [Elastic Agent install
  documentation](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent):
  native packages beside archives, and archives as what its fleet upgrades from.
- [GoReleaser — Archives](https://goreleaser.com/customization/archive/): the default
  `{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`.
- [Debian FAQ, package basics](https://www.debian.org/doc/manuals/debian-faq/pkg-basics.en.html)
  and [dpkg-name(1)](https://www.man7.org/linux/man-pages/man1/dpkg-name.1.html);
  [Semantic Versioning 2.0.0](https://semver.org/): `-` is in a version's alphabet, `_` is not.
- [The Cargo Book, environment
  variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html): `--target`
  builds the triple while the packer stays a host build.
- GitHub, *Using artifact attestations to establish provenance for builds*, recalled, not re-read:
  <https://docs.github.com/en/actions/security-guides/using-artifact-attestations-to-establish-provenance-for-builds>.
  And
  [Using artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations):
  `subject-name` without a tag, `subject-digest`, `push-to-registry: true`, `packages: write`, and
  `gh attestation verify oci://…`.
- The `actions/attest` README at `v4` (latest `v4.2.2`), read 2026-10-18:
  <https://github.com/actions/attest>. It documents:
  - the permissions `id-token: write` and `attestations: write`. It also lists
    `artifact-metadata: write` for storage records. Those need `push-to-registry` and a repository
    of an organization, so the file attestation does not need it;
  - `subject-path` with globs and lists, up to 1024 subjects in one attestation;
  - `subject-checksums`, and the SBOM mode;
  - public-good Sigstore for public repositories;
  - availability on private repositories only under Enterprise Cloud.
- The `actions/attest-build-provenance` README at `v4` (latest `v4.2.2`), read 2026-10-18: "As of
  version 4, `actions/attest-build-provenance` is simply a wrapper on top of `actions/attest`."
- `gh attestation verify --help`, gh 2.102.0, read 2026-10-18. The SLSA v1 predicate is the
  default. The help documents `--repo`, `--signer-workflow`, `--deny-self-hosted-runners`,
  `--source-digest`, `--source-ref`, and `--bundle` and `--custom-trusted-root` for offline
  verification.
- SLSA v1.0, *Build provenance*, recalled: <https://slsa.dev/spec/v1.0/provenance>.
- The CycloneDX 1.5 specification and its file-name recommendation `*.cdx.json`, recalled:
  <https://cyclonedx.org/specification/overview/>.
- The `cargo-cyclonedx` README and `src/generator.rs`, read 2026-10-18:
  <https://github.com/CycloneDX/cyclonedx-rust-cargo>. They document:
  - `--describe binaries`, `--target`, `--format json`, and `--spec-version` 1.3 to 1.5 (default
    1.3);
  - the output name `<bin>_bin.cdx.json` beside the package's `Cargo.toml`;
  - that it runs `cargo metadata`, which can rewrite an outdated `Cargo.lock`.

  crates.io lists 0.5.9 (2026-03-19, MSRV 1.85) as the latest. The workflow pins 0.5.9.
- NTIA, *The Minimum Elements For a Software Bill of Materials* (2021), and the EU Cyber
  Resilience Act, Annex I. Recalled, not re-read.
- [Distroless README](https://github.com/GoogleContainerTools/distroless/blob/main/README.md): the
  Debian 13 images `static-debian13`, `base-debian13` and `cc-debian13`, the `nonroot` and `debug`
  tags, no shell, keyless cosign signatures;
  [cc](https://github.com/GoogleContainerTools/distroless/blob/main/cc/README.md) (base plus
  `libgcc1`) and [base](https://github.com/GoogleContainerTools/distroless/blob/main/base/README.md)
  (glibc, libssl, CA certificates, tzdata, `/tmp`).
- OCI Image Format,
  [Pre-Defined Annotation Keys](https://github.com/opencontainers/image-spec/blob/main/annotations.md).
- [dependabot-core issue 6067](https://github.com/dependabot/dependabot-core/issues/6067) and
  [Chainguard on Dependabot and image digests](https://edu.chainguard.dev/chainguard/containers/staying-secure/updating-images/dependabot/):
  Dependabot's support for a file not named `Dockerfile` is not established, hence clause 20's
  fallback.
- The code: the `build`, `sbom`, `image` and `publish` jobs of `.github/workflows/release.yml`,
  `DEFAULT_LISTEN` and `DEFAULT_REST_LISTEN` in `crates/fleet-server/src/config.rs`, and the
  absence of a health route in `crates/fleet-server/src/api.rs`.

## Consequences

- Positive: the fleet installs the Client's release archive unmodified, so the hash the release
  lists is the hash an Agent verifies. A build that lost its tags fails instead of releasing a
  `-dev` artifact. The Client's release is an ordinary fleet Package, in the container every agent
  uses.
- Positive: a file name parses back into its four fields. The fleet view reads upstream artifacts
  and this project's with one code path.
- Positive: with one command, an operator proves that a downloaded file or the Server's image was
  built by this repository's `release.yml`, at a named commit, on a GitHub-hosted runner. That
  includes the archive they hand their Server and the tool that makes their keys. `SHA256SUMS`
  becomes trustworthy once it verifies. A scanner gets the exact dependency set of the binary on
  the platform the operator runs, the Server's included.
- Positive: the image starts only with TLS and the operator's credential configured, runs
  unprivileged with no shell, and carries no secret.
- Negative / trade-offs: a platform is spelled `linux_amd64` in a file name and `linux-amd64` as a
  tag. The version in a file name is the base version, so the name is not a unique build
  identifier. Of the five targets, two are not what their runner natively is.
- Negative / trade-offs: the attestation proves where and from what a file was built, not that the
  source is benign. It cannot be revoked: an attested file stays attested. Verifying needs `gh` and
  network access to GitHub, or a downloaded bundle and trusted root for offline verification. The
  Rekor log records the digests and the workflow identity publicly, which a public repository
  already shows. No file carries a code signature: Windows shows an unknown publisher, and `rpm`
  reports no signature.
- Negative / trade-offs: the `version` job sets the tag before anything is built
  ([OPS-cargo-decides-product-version](OPS-cargo-decides-product-version.md)). An attestation or
  verification failure in `publish` leaves a tag without a release. Re-running the failed job
  completes the release. A new run refuses the spent number, as for any failure of `publish`.
- Negative / trade-offs: a script that downloads `supervisor_<version>_*` without an extension also
  matches the SBOMs. The notes' own upload loop and `package fetch` select `.tar.gz` only.
- Negative / trade-offs: the release builds the Server twice more and uploads to a registry, and
  `publish` holds `packages: write`. The registry is a second place a release appears. The base
  image's own packages are not in the SBOM; the base digest names them, and its updates track them.
  With no shell in the image, debugging inside it is harder. The `debug` variant of the base serves
  that, locally.
- Follow-ups: an SBOM attestation per archive; the Client checking an attestation on self-update;
  SLSA Build Level 3; code signatures for the installers; a health endpoint and a readiness probe
  on it; a musl build on `static`; examples for running the Server in production; whether
  Dependabot updates the `Containerfile`'s digest.

## Enforcement

- [`release.yml`](../../.github/workflows/release.yml) holds these steps:
  - The step `The binary agrees` of the `version` job, and each target's step
    `The binary agrees with the name it will be given`, hold clause 1.
  - The pack step holds clauses 2 and 3.
  - The step `The RPM carries its scriptlets, not their file names` asserts with
    `rpm -qp --scripts` that the RPM's architecture is the ecosystem's (clause 3).
- Tests in `crates/fleet-tools/tests/release_workflow.rs` cite this ADR. They read the workflow and
  the Containerfile as text:
  - `only_the_publish_job_may_attest` (clauses 10, 11):
    - `id-token: write` and `attestations: write` each occur exactly once, inside the `permissions:`
      of the `publish` job.
    - The top-level `permissions:` is `contents: read` alone.
    - The `publish` job has no `actions/checkout` and no `cargo`.
    - It has no `uses:` other than `actions/download-artifact@` and `actions/attest@`.
    - It runs only when the run makes a release.
  - `the_publish_job_attests_and_verifies_every_asset_before_it_publishes` (clauses 5, 9, 12):
    - The steps of the `publish` job run in this order: `Collect the checksums`, the attestation,
      the verification, `Publish the release`.
    - The attestation's `subject-path` names `*.tar.gz`, `*.deb`, `*.rpm`, `*.msi`, `*.cdx.json`
      and `SHA256SUMS`.
    - The verification step runs `gh attestation verify` over the same set, with
      `--signer-workflow` and `--deny-self-hosted-runners`.
    - `gh release create` releases `*.cdx.json`.
  - `the_publish_job_attests_and_verifies_the_image_before_it_tags_it` (clauses 9, 10, 19):
    - The image's attestation step has `subject-name`, `subject-digest` and
      `push-to-registry: true`.
    - Its verification runs `gh attestation verify oci://` with `--signer-workflow`.
    - The tagging step comes after the verification and before `Publish the release`.
    - The workflow sets no `latest` tag (clause 18).
  - `the_sbom_job_writes_one_sbom_per_program_and_target_with_a_pinned_tool` and
    `the_sbom_job_describes_the_server_for_both_linux_targets` (clauses 7, 8):
    - The `sbom` job has `needs: version`, no `if:`, and no `permissions:` beyond the top level.
    - It installs `cargo-cyclonedx@<MAJOR.MINOR.PATCH>` with `cargo install --locked`.
    - It writes `supervisor_…_<os>_<arch>.cdx.json` for every matrix row.
    - It writes `opamp-fleetctl_…_<os>_<arch>.cdx.json` for every `operator_tool: true` row.
    - It writes `server_…_linux_<arch>.cdx.json` for both Linux rows.
    - It writes each with `--describe binaries --target <triple> --spec-version 1.5`, beside a
      `.sha256`.
    - It fails when `Cargo.lock` changed.
  - `the_notes_say_how_to_verify` and
    `the_notes_name_the_image_by_its_digest_and_say_how_to_verify_it` (clauses 5, 18): the release
    notes contain `gh attestation verify` and `--signer-workflow`, for the files and for the image
    by its digest.
  - `the_containerfile_copies_the_release_binary_onto_a_pinned_distroless_base_and_runs_nothing`
    (clauses 13, 15, 16, 17):
    - `packaging/container/Containerfile` has exactly one `FROM`, naming
      `gcr.io/distroless/cc-debian13:nonroot@sha256:`.
    - It has no `RUN`, `USER 65532:65532`, no `HEALTHCHECK`, and a last `WORKDIR` of `/`.
    - It sets no `FLEET_SERVER__` variable beyond `LISTEN` and `REST__LISTEN`.
  - `both_linux_rows_build_the_server_and_assert_its_version` (clause 14): both Linux rows build
    `-p fleet-server --bin server` with `--locked` and assert its version.
  - `the_image_job_builds_checks_and_pushes_by_digest_alone` (clauses 10, 19):
    - The `image` job's permissions are `contents: read` and `packages: write` alone.
    - It has no `id-token` and no `attestations`.
    - Its builder's image is pinned by digest.
    - It runs the version, user and shell checks.
    - It sets no tag.
- [`scripts/check-build-hardening.sh`](../../scripts/check-build-hardening.sh) check 3 fails on a
  `cargo install` in `release.yml` without an exact version. It covers the tool of clause 8. Its
  check 2 fails on a cache in `release.yml`.
- A test that carries `Verifies: OPS-releases-attestations-server-image` holds clause 16:
  `a_data_dir_the_account_cannot_write_fails_startup_naming_it` in
  [`config.rs`](../../crates/fleet-server/src/config.rs). The test of the Containerfile above
  allows no `FLEET_SERVER__DATA_DIR`.

**Not mechanically decidable:** these hold only against a real release on GitHub, or only in
the judgement of a review, which no test makes.

- That a real release's files and the image's digest verify on GitHub. Clause 12's step in
  `publish` holds it at release time: it fails the release and leaves the image untagged.
- That each SBOM is complete for its binary. Review holds it, against
  `cargo tree --target <triple> -e normal -p <package>`.
- That the image runs on both architectures. The `image` job's check holds it for `amd64`. The
  `build` row's version check of the same binary holds it for `arm64`. A maintainer checks by hand
  on the first release that the `arm64` image starts.
- The reading of a file name by the fleet view (clause 4). No automated test covers it.
- That a release which changes the names or containers of its files names the change in
  `CHANGELOG.md` and its notes (clause 3). Review holds it.
