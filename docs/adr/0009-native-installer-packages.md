# ADR-0009: Native installer packages for the Supervisor Host, payload-only and service-neutral

- **Status:** 🟡 proposed
- **Date:** 2026-07-21
- **Deciders:** Maintainer

## Context

ADR-0008 gives every release a `.tar.gz` (Linux, macOS) or `.zip` (Windows) archive per target. An
archive is not an installation: the operator must unpack it, choose a location, and run
`supervisor-host service install` by hand. The maintainer requires the release to additionally carry
**natively installable packages** for the Supervisor Host on all three platforms — `.deb` and `.rpm`
for Linux, an `.msi` for Windows, and a `.pkg` for macOS — so that installing the agent is the
platform-idiomatic act an operator already knows. Per the specification the **Server stays Linux-only
and is deliberately *not* packaged here**: it is not the deployable that lands on fleet machines, and
adding packages for it would widen scope without a present need (YAGNI).

Two accepted ADRs constrain this decision, and one of them appears to forbid it outright.

**ADR-0006 rejected OS-specific installers.** Its Alternatives section dismisses "a separate
service-management binary or an OS-specific installer (`.deb`/`.pkg`/MSI wrapping `sc.exe`)" because it
"spreads the logic across artifacts and contradicts the maintainer's *one binary contains all
functionality*". That rejection is sound *as stated* — it is aimed at an installer that **owns the
service lifecycle**, duplicating in `postinst`/`ServiceControl`/`postinstall` what the binary's
`service install` subcommand already does. It is not an argument against a package that merely
*delivers* the binary. This ADR therefore does not overturn ADR-0006's reasoning; it adopts it as the
governing constraint and derives the package design from it. Where ADR-0006's blanket wording is read
as a prohibition on any package artifact, **this ADR supersedes that reading only** — the one-binary
principle it protects is preserved intact.

**ADR-0007 makes service ownership the load-bearing question.** Self-update works by installing each
version into `versions/<sha256>/` and atomically repointing a `current` symlink/junction that the
registered service's program path targets. A package that registers the service against a fixed path
like `/usr/bin/supervisor-host` breaks that mechanism; worse, a package that installs *into* the
versioned layout puts the package manager and the Updater in a fight over the same files, where the
next `apt upgrade` silently reverts a fleet-driven update and the package database ends up lying about
what is installed. This is not hypothetical: it is why **Elastic Agent disables Fleet-managed upgrade
and manual rollback entirely for DEB/RPM installs**, and why **Datadog** — the one comparable agent
that does both — bypasses the package manager with its own installer and a versioned
`/opt/datadog-packages` `stable`/`experiment` layout that is structurally the same idea as ADR-0007's
`current` pointer.

A package's install layout, its service behaviour, and its artifact names are a public interface that
operators and configuration-management tooling bind to, and reversing them after the first release is
expensive. Per AGENTS.md §3 this needs an ADR.

## Decision

We will publish **native installer packages for the Supervisor Host only** — `.deb`, `.rpm`, `.msi`,
and `.pkg` — built by the ADR-0008 release workflow alongside (never instead of) the existing archives,
and we will make every one of them **payload-only and service-neutral**: a package installs the binary
and nothing else, and **never registers, starts, stops, or removes the operating-system service**.
Service lifecycle remains exclusively the binary's own `service` subcommands from ADR-0006, so
ADR-0007's `current`-pointer layout stays the single owner of what the service runs.

- **Payload and layout.** Each package installs exactly one file — the `supervisor-host` executable —
  plus `README.md` and `LICENSE` as documentation, at the platform-conventional path:
  `/usr/bin/supervisor-host` (deb/rpm), `/usr/local/bin/supervisor-host` (pkg), and
  `%ProgramFiles%\OpAMP Fleet\supervisor-host.exe` with that directory appended to the system `PATH`
  (msi). This path is the **installation source**, deliberately distinct from ADR-0007's runtime
  install root: `service install` copies from it into `versions/<sha256>/` and points the service at
  `current`. The two never overlap, so a package upgrade and a self-update cannot collide.
- **No maintainer scripts that touch the service.** No `postinst`/`postrm`, no `postinstall`, no
  `ServiceInstall`/`ServiceControl` elements. Installing a package leaves the machine with a binary on
  `PATH` and no running service; the operator (or their configuration-management tool) runs
  `supervisor-host service install` to register it, exactly as with the archive. Uninstalling a package
  removes the binary and leaves any registered service and the state directory untouched — the operator
  runs `service uninstall` first. **Both packages' `description` and the README document this
  two-step contract**, because it is the one place this design surprises an operator who expects
  `apt install` to leave a running daemon.
- **Self-update stays enabled for every install method.** Because no package owns the service or the
  versioned layout, we do **not** need Elastic's "disable self-update when package-installed" rule, and
  we do not adopt it. This is the whole point of the payload-only design: one self-update code path
  works identically whether the binary arrived by archive or by package.
- **Build tooling.** Linux packages are built with **`nfpm`** — a single static Go binary, one YAML
  config producing both `.deb` and `.rpm`, no `dpkg`/`rpmbuild` on the runner, and, decisively, no
  opinion about how the payload binary was produced, so it consumes ADR-0008's already-built,
  target-specific, `OPAMP_FLEET_VERSION`-stamped artifact directly. Windows uses **WiX v3's
  `candle.exe`/`light.exe` directly against a committed `wix/main.wxs`**, using the toolset
  preinstalled on the `windows-latest` runner (reached via the `WIX` environment variable, as it is not
  on `PATH`). macOS uses **`pkgbuild`**, part of the Xcode command line tools already present on the
  macOS runner. Only `nfpm` is downloaded; nothing is added to the Rust toolchain of ADR-0003, and all
  packaging tooling is CI-only.
- **Naming and checksums.** Packages follow ADR-0008's scheme and are checksummed the same way:
  `opamp-fleet-supervisor-host-<version>-<arch>.deb` / `.rpm`, `-<version>-x86_64.msi`, and
  `-<version>-<arch>.pkg`, each with a `.sha256` sidecar. Package versions use ADR-0008's normalised
  dot-separated `MAJOR.MINOR.PATCH` unchanged — it is valid for all four formats, and the MSI
  `ProductVersion` field accepts it directly.
- **Signing is deferred, and unsigned packages are shipped meanwhile.** `.deb` and `.rpm` GPG signing,
  MSI Authenticode, and macOS notarization all require key material and a trust model the
  specification's Non-Goals currently defer; the `.sha256` sidecars remain the integrity bar, as for the
  archives. This is acceptable per format: an unsigned `.deb`/`.rpm` installs without complaint
  (`dpkg -i` does not verify per-file signatures anyway), and an unsigned `.pkg` installs cleanly via
  `sudo installer -pkg … -target /` because Gatekeeper evaluates browser-applied quarantine on
  double-click, not the installer engine. The **unsigned MSI is the weak spot** — SmartScreen warns and
  the UAC prompt shows an unknown publisher — and is recorded as the first signing follow-up.
- **CI builds packages on every release run, published only with `publish: true`**, inheriting
  ADR-0008's dry-run default, so the packaging path is exercised on every dry run rather than only when
  it matters.

Linux packaging covers **`x86_64` only**, matching ADR-0008's target set; `aarch64` follows if and when
that target is added.

## Alternatives considered

- **Let the packages register the service** (`postinst` + `systemctl enable`, WiX
  `ServiceInstall`/`ServiceControl`, a launchd `postinstall`) — the conventional agent packaging and by
  far the nicer first-run experience: one command and the daemon is running. Rejected because it
  directly contradicts ADR-0006's one-binary principle (duplicating `service install` in three
  package-script dialects) and, more concretely, because it must register a **fixed** program path,
  which breaks ADR-0007's `current` pointer — the package would own a service the Updater must
  repoint. Elastic's response to exactly this collision is to switch self-update *off* for packaged
  installs; keeping the package payload-only keeps self-update on everywhere instead, which is worth
  more than saving one command.
- **Packages that lay out the `current` pointer and register the service against it** — the maximal
  convenience option and the only one that gives both a running daemon and a working self-update. It is
  what Datadog effectively built. Rejected as a gross violation of "simplicity first": the package
  scripts would have to encode ADR-0007's layout, detect and preserve an in-flight update across
  `apt upgrade`, and stay correct in three scripting dialects as that layout evolves — a second,
  divergent implementation of the Updater's core invariant, maintained where it cannot be unit-tested.
- **Skip packages entirely and keep only archives** — the status quo, and defensible (OTel Collector,
  Telegraf, and Alloy ship packages *without* any self-update; the archive plus `service install`
  already installs cleanly). Rejected because the maintainer's requirement is explicit and because
  `.deb`/`.rpm`/`.msi`/`.pkg` are what fleet-provisioning tooling consumes; an archive forces every
  operator to write the unpacking themselves.
- **`cargo-deb` + `cargo-generate-rpm` instead of `nfpm`** — Cargo-native, configured in `Cargo.toml`,
  no non-Rust tool. Rejected because both infer their payload from a Cargo build in `target/`, which
  fights ADR-0008's structure where the binary is built in a separate step for an explicit `--target`;
  `nfpm`'s plain `src → dst` mapping consumes that artifact with no coaxing, and one YAML file replaces
  two tool configurations for two formats. `cargo-generate-rpm`'s built-in `--signing_key` is a real
  advantage we forgo, but signing is deferred regardless.
- **`cargo-wix` instead of calling WiX directly** — the idiomatic Rust choice, and genuinely valuable
  for *generating* a sound `main.wxs` (upgrade logic, `PlatformProgramFilesFolder`, the `PATH`
  component). But that value is one-time and is captured by running `cargo wix init` once and
  committing the result. In CI, with the binary already built, it degrades to a wrapper that passes
  `-d` variables to `candle`/`light` while requiring a slow `cargo install cargo-wix` from source and
  the subtle `--no-build` + `--target-bin-dir` pairing — omit the latter and it silently rebuilds or
  looks in the wrong target directory. Calling `candle`/`light` against the committed `.wxs` removes
  the tool, the build step, and that failure mode without losing anything.
- **WiX v4/v5/v6 instead of v3** — newer and better documented, but not on the `windows-latest` image
  (which ships WiX 3.14.1), so it needs a `dotnet tool install` step, and **WiX v6 binaries carry an
  Open Source Maintenance Fee for organisations above $10k annual revenue**. v3 supports everything a
  payload-only MSI needs. Revisit if v3 support lapses.
- **MSIX / a winget manifest instead of an MSI** — the direction Microsoft is pushing (PowerShell 7.7
  drops its MSI for MSIX), but MSIX is a containerised, per-user, Store-oriented model that is
  explicitly unsuited to software running in the SYSTEM context, and `winget` is unsupported there —
  disqualifying for a machine-wide agent. A winget manifest *pointing at* the MSI is a sensible later
  discovery layer, not a replacement.
- **`cargo-bundle` for the macOS artifact** — advertises deb/rpm/msi/osx from one tool, but it is
  self-described early alpha, targets GUI `.app` bundles, and does not produce a `.pkg` at all.
- **A Homebrew tap / apt & yum repositories instead of standalone package files** — the best ongoing
  UX on each platform and the only way `.deb`/`.rpm` signing becomes meaningful (apt verifies the
  *repository* signature, not the file). Deferred: hosting and key management are a separate concern,
  and repositories serve exactly these package files once they exist, so nothing here is wasted.

## Sources / Prior art

- **Elastic Agent — the decisive precedent on packages vs. self-update**: Fleet-managed upgrade and
  manual rollback are **not supported** for DEB/RPM installs, and the tarball distribution is
  recommended when upgrades matter:
  <https://www.elastic.co/docs/reference/fleet/upgrade-elastic-agent>; `install`/`uninstall` subcommand
  design: <https://github.com/elastic/beats/issues/21019>; preserving state across a package upgrade:
  <https://github.com/elastic/elastic-agent/issues/3832>.
- **Datadog Agent — the versioned-layout exception**: `/opt/datadog-packages` with `stable`/
  `experiment` slots, its own installer bypassing the package manager, and the documented conflict with
  configuration-management tools pinning versions:
  <https://docs.datadoghq.com/agent/fleet_automation/upgrade_agents/> and
  <https://pkg.go.dev/github.com/DataDog/datadog-agent/pkg/fleet/installer/paths>.
- **OpenTelemetry Collector releases** — goreleaser + nfpm producing deb/rpm, unit file and
  `config|noreplace` handling, no self-update:
  <https://github.com/open-telemetry/opentelemetry-collector-releases/blob/main/distributions/otelcol-contrib/.goreleaser.yaml>;
  **Grafana Alloy** (packages own the service; Fleet Management is configuration-only):
  <https://grafana.com/docs/alloy/latest/set-up/install/linux/> and
  <https://grafana.com/docs/grafana-cloud/send-data/fleet-management/introduction/>; **Telegraf**
  (package-manager upgrades only): <https://docs.influxdata.com/telegraf/v1/install/>.
- **nfpm** (single Go binary, one config for deb/rpm, arbitrary `src → dst` contents, no native
  toolchain): <https://nfpm.goreleaser.com/> and <https://nfpm.goreleaser.com/docs/configuration/>;
  cross-distro systemd caveats: <https://nfpm.goreleaser.com/docs/tips/>. Alternatives compared:
  <https://github.com/kornelski/cargo-deb> and <https://github.com/cat-in-136/cargo-generate-rpm>.
- **cargo-wix** and the WiX version landscape: <https://github.com/volks73/cargo-wix>; `ServiceControl`
  (the element deliberately *not* used): <https://wixtoolset.org/docs/v3/xsd/wix/servicecontrol/>; WiX
  v6 Open Source Maintenance Fee: <https://docs.firegiant.com/wix/osmf/>; WiX 3.14.1 on the runner
  image: <https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md>.
  MSIX unsuitability in the SYSTEM context:
  <https://www.techtarget.com/searchenterprisedesktop/tip/Comparing-MSI-vs-MSIX> and
  <https://github.com/microsoft/winget-cli/discussions/2892>.
- **macOS `pkgbuild`/`productbuild`**: <https://keith.github.io/xcode-man-pages/pkgbuild.1.html>;
  that `sudo installer -pkg` does not trigger Gatekeeper regardless of the quarantine flag, unlike a
  double-click: <https://scriptingosx.com/2025/08/installing-packages/>; Sequoia's move of the override
  into System Settings: <https://support.apple.com/en-us/102445>.
- Specification (the Supervisor Host is "the one deployable that a machine runs"; the Server is
  Linux-only and out of scope here) ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); ADR-0006 (the
  `service` subcommands that keep sole ownership of the service, and the installer alternative this ADR
  revisits), ADR-0007 (the `current` pointer and versioned installs the packages must not touch), and
  ADR-0008 (the release workflow, version normalisation, artifact naming, and `.sha256` convention
  these packages extend).

## Consequences

- Positive: an operator installs the Supervisor Host with `apt install` / `dnf install` / a
  double-clicked MSI / `sudo installer -pkg`, on every supported platform, instead of unpacking an
  archive by hand — and fleet-provisioning tooling gets the artifact shape it expects. Because the
  packages are payload-only, **ADR-0007's self-update keeps working identically for every install
  method**, which is strictly better than the prior art we surveyed: Elastic must switch self-update
  off for packaged installs, and Datadog must maintain a bespoke installer to avoid that. ADR-0006's
  one-binary principle survives untouched — the packages contain no service logic to keep in sync with
  the CLI. Packaging is built on every release dry run, so the path is exercised continuously, and the
  archives remain the primary artifact for anyone who wants them.
- Negative / trade-offs: **installing a package does not start anything** — the operator must still run
  `supervisor-host service install`, which contradicts the near-universal expectation that installing an
  agent package yields a running agent, and it is the single most likely source of confusion this
  decision creates (mitigated by the package description and README, not eliminated). Uninstalling a
  package while a service is registered leaves a service pointing into the ADR-0007 install root, so
  `service uninstall` must come first, and nothing enforces that ordering. The release workflow grows
  three platform-specific packaging paths and one new CI tool (`nfpm`) to keep current, and the artifact
  count per release roughly doubles. All four packages are **unsigned**: the MSI in particular will draw
  a SmartScreen warning and an "unknown publisher" UAC prompt, which is a visible trust cost on the
  platform where it hurts most. macOS `.pkg` and Windows `.msi` installation behaviour can only be
  verified on real hosts — CI can build these artifacts but cannot install them, so "the package
  installs correctly" stays a manual smoke test, as with ADR-0006's service runtime.
- Follow-ups: **signing and notarization**, already an ADR-0008 follow-up, now covers four more artifact
  types and should start with Windows Authenticode where the unsigned experience is worst; **apt/yum
  repositories and a Homebrew tap** (plus a winget manifest pointing at the MSI) as the distribution
  layer these package files feed; `aarch64` Linux packages once that build target exists; and a possible
  future `service install --from-package` convenience if the two-step contract proves too sharp an edge
  in practice.
