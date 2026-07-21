# ADR-0008: Release pipeline with tag-derived strict SemVer and per-platform archives

- **Status:** 🟡 proposed
- **Date:** 2026-07-21
- **Deciders:** Maintainer

## Context

The project now has two deployables to ship: the **Supervisor Host** — which per ADR-0006 installs
itself as a native OS service on Linux, macOS, and Windows — and the **Server**, which the
specification scopes to **Linux only** ("Ship the Agent as a self-updating OS service across all
platforms; the Server on Linux"). There is no release pipeline: CI (ADR-0006) builds and tests but
produces no downloadable artifact, the workspace version is a static `0.1.0`, and the repository has
no tags. Nothing an operator can install exists.

Three forces shape the decision. First, **ADR-0007's self-update consumes exactly these artifacts**:
it stages a new binary and verifies its **SHA-256** before switching the `current` pointer
(`update --new-binary … --hash …`), and the future OpAMP package delivery will carry the same digest
in `DownloadableFile.content_hash`. A release that does not publish per-artifact checksums cannot
feed that mechanism. Second, the **version must be derived from the repository, not hand-maintained**:
the maintainer's convention is a tag `version/<major><sep><minor><sep><patch>` where the separator may
be `.` or `/`, resolved from the **most recent such tag reachable from the branch being built**, under
**strict semantic versioning**. Third, the resolved version must be **available during the build as an
environment variable** so it can be compiled into the binaries — today both places that report a
version (the OpAMP `service.version` identifying attribute and ADR-0007's health report) read
`CARGO_PKG_VERSION`, so a released binary would otherwise announce `0.1.0` to the fleet.

An artifact contract — names, formats, checksums, and the versioning scheme — is a public interface
that is costly to change once operators and installers consume it, so per AGENTS.md §3 it needs an ADR.

**A note on numbering:** ADR-0007's Follow-ups refer to "ADR-0008" for OpAMP package delivery. That
was a forward reference written before this decision existed; ADR numbers are assigned in the order
decisions are taken, not reserved. Package delivery will therefore carry a later number, and
ADR-0007's mention should be read as "a future ADR".

## Decision

We will add a **manually triggered** GitHub Actions release workflow (`workflow_dispatch` on the
chosen branch) that derives the version from git, builds both deployables for their supported
platforms, and publishes archives with checksums as a GitHub Release.

- **Version derivation.** Resolve the most recent `version/*` tag reachable from the built ref
  (`git describe --tags --match 'version/*' --abbrev=0`) and parse it against
  `^version/(0|[1-9][0-9]*)(\.|/)(0|[1-9][0-9]*)(\.|/)(0|[1-9][0-9]*)$` — exactly three non-negative
  integers, no leading zeros, either `.` or `/` (mixed permitted) as separator, and **no pre-release
  or build metadata**. A missing or malformed tag **fails the workflow** (fail closed) rather than
  inventing a version.
- **Normalisation to dot-separated SemVer.** Whatever separator the tag used, the version is
  normalised to **`MAJOR.MINOR.PATCH` with `.` as the only separator** before it is used anywhere —
  in the environment variable, the artifact names, and the release tag. The tag spelling is an input
  convenience; it never leaks into a published identifier. So `version/1.2.3`, `version/1/2/3`, and
  `version/1.2/3` all yield exactly the same version `1.2.3` and byte-identical artifact names.
- **Version propagation.** The normalised version is exported as **`OPAMP_FLEET_VERSION`** for every
  build step. The binaries read it at compile time via `option_env!("OPAMP_FLEET_VERSION")` and fall
  back to `CARGO_PKG_VERSION`, so release builds report the release version to the fleet while local
  and CI builds keep working unchanged. `Cargo.toml` is **not** rewritten by the pipeline.
- **Targets.** The **Supervisor Host** is built for `x86_64-unknown-linux-gnu`,
  `x86_64-pc-windows-msvc`, `x86_64-apple-darwin`, and `aarch64-apple-darwin` (both macOS
  architectures built on the macOS runner). The **Server** is built for `x86_64-unknown-linux-gnu`
  only, per the specification. Every target is built on its native runner; no cross-compilation
  toolchain is introduced.
- **Artifacts.** One archive per component and target, named
  `opamp-fleet-<component>-<version>-<target>.<ext>`, containing the binary plus `README.md` and
  `LICENSE`. The format follows platform convention: **`.tar.gz` for Linux and macOS, `.zip` for
  Windows** — so a release carries both formats. Each archive is accompanied by a **`.sha256`**
  checksum file, which is what ADR-0007's `--hash` and future package delivery verify.
- **Dry run by default.** The `workflow_dispatch` trigger takes a boolean input **`publish`
  (default `false`)**. Without it the workflow builds, archives, and checksums everything and uploads
  the result as **workflow-run artifacts** only — so the pipeline (and a release candidate) can be
  exercised without publishing anything. With `publish: true` the same artifacts are attached to a
  GitHub Release. Publishing is therefore always a deliberate act, never a side effect of a test run.
- **Publishing under a normalised release tag.** When publishing, the workflow creates the annotated
  tag **`v<version>`** (e.g. `v1.2.3`) at the built commit and attaches all archives and checksums to a
  GitHub Release on that tag. The `version/…` tag stays the human-facing input that *selects* the
  version; `v1.2.3` is the published identifier. This keeps asset download URLs flat and stable —
  `…/releases/download/v1.2.3/<asset>` — independent of how the input tag was spelled, which matters
  because a `version/…` tag always contains at least one slash (and up to three) and would otherwise
  push extra path segments into every download URL, including the `DownloadableFile.download_url` a
  future package delivery serves.

## Alternatives considered

- **Trigger on pushing a `version/**` tag** — the classic release trigger, but it couples publishing to
  tagging and cannot be re-run without a new tag; the maintainer wants releases deliberately started
  against a chosen branch, with the tag only supplying the version.
- **Publish on every run (no dry-run switch)** — simpler, but with a manual-only trigger every attempt
  to exercise the pipeline would create a public release; an explicit `publish` input keeps test runs
  harmless while still producing downloadable (if transient) workflow artifacts.
- **Publish the release on the raw `version/…` tag** — avoids creating a second tag, but a
  `version/…` tag always contains at least one slash, so asset URLs gain extra path segments and
  differ depending on whether the tag was written `version/1.2.3` or `version/1/2/3` — two URL shapes
  for one version. A normalised `v<version>` tag gives one flat, predictable download URL per release.
- **Adopt `cargo-dist`** — generates exactly this kind of multi-platform release, but it owns the
  workflow, imposes its own artifact/version conventions (including reading the version from
  `Cargo.toml`), and would fight the `version/`-tag scheme decided here; a small explicit workflow
  keeps the contract ours and adds no framework to the toolchain (ADR-0003).
- **Take the version from `Cargo.toml`** (bumping it per release) — the common Rust convention, but it
  makes the version a hand-maintained file that must be kept in sync with tags; deriving it from the
  tag makes the tag the single source of truth and keeps `Cargo.toml` stable.
- **Publish both `.tar.gz` and `.zip` for every target** — literally maximises format coverage but
  doubles the artifact count for no benefit; the platform-conventional split already puts both formats
  in every release and matches what tooling on each platform expects.
- **Also build `aarch64-unknown-linux-gnu`** — desirable for ARM servers and containers, but needs
  `cross`/`cargo-zigbuild` or an ARM runner; deferred (YAGNI) until an ARM Linux target is actually
  required. macOS arm64 comes free on the macOS runner and is therefore included.
- **Allow pre-release tags** (`version/1.2.3-rc.1`) — SemVer permits them, but "strict semantic
  versioning" here means released versions only; admitting pre-releases would also complicate the
  "latest reachable tag" rule. Deferred until a release-candidate flow is actually wanted.

## Sources / Prior art

- Semantic Versioning 2.0.0 (the `MAJOR.MINOR.PATCH` grammar and no-leading-zero rule):
  <https://semver.org/>.
- `git describe` for resolving the nearest reachable tag:
  <https://git-scm.com/docs/git-describe>.
- GitHub Actions `workflow_dispatch` (manual trigger on a chosen ref):
  <https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows>.
- Conventional Rust release layout — `.tar.gz` for Linux/macOS and `.zip` for Windows, archives named
  with binary, version, and target triple, with a `.sha256` per archive:
  <https://blog.urth.org/2024/10/27/my-new-github-action-for-releasing-rust-projects/> and
  <https://rakhim.exotext.com/how-to-build-and-publish-multi-platform-rust-binaries>.
- `softprops/action-gh-release` for attaching artifacts to a GitHub Release:
  <https://github.com/softprops/action-gh-release>.
- `cargo-dist` (considered as a framework alternative): <https://opensource.axo.dev/cargo-dist/>;
  `cross` for Linux cross-compilation (deferred): <https://github.com/cross-rs/cross>.
- Compile-time environment access in Rust (`option_env!`):
  <https://doc.rust-lang.org/std/macro.option_env.html>.
- Specification (Server is Linux only; the Supervisor Host runs on Linux, macOS, and Windows)
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); ADR-0006 (CI matrix and the platforms the agent
  supports) and ADR-0007 (content-hash verification the published checksums feed).

## Consequences

- Positive: one command produces installable, versioned artifacts for every supported platform, with
  the Server correctly restricted to Linux; the tag is the single source of truth for the version, and
  strict parsing rejects anything ambiguous instead of guessing; normalisation means one version has
  exactly one spelling everywhere, so artifact names and download URLs never depend on how the tag was
  typed; the pipeline can be exercised end to end without publishing; released binaries report their
  real version to the fleet; the published SHA-256 files are exactly the input ADR-0007's self-update
  and the future package delivery verify against, closing that loop without extra work.
- Negative / trade-offs: releases are manual by design, so publishing is a deliberate act and never
  happens automatically on a tag; the version lives only in git tags, so a repository without a
  well-formed `version/*` tag cannot be released (intentional, but it makes the first release depend on
  creating a tag first); a published release carries **two tags** for one version (the `version/…`
  input tag and the `v…` release tag), which is mild duplication accepted in exchange for stable
  download URLs; creating that tag and the release means the workflow needs `contents: write`
  permission, unlike the read-only CI workflow; Linux `aarch64` is not covered; macOS artifacts are
  **unsigned and un-notarized**, so Gatekeeper will block them on download until signing is added —
  the same gap ADR-0007 records for self-update.
- Follow-ups: **signing and notarization** (macOS codesign/notarytool, Windows Authenticode) and, with
  them, signature verification in the self-update path; **Linux `aarch64`** via `cross` when needed;
  and the **OpAMP package delivery** ADR, which will serve these artifacts and their checksums to
  Agents over the wire.
