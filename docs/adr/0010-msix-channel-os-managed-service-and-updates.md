# ADR-0010: MSIX channel for the Supervisor Host with OS-managed service and App Installer updates

- **Status:** 🟡 proposed
- **Date:** 2026-07-21
- **Deciders:** Maintainer

## Context

The maintainer requires a Windows release channel in which the Supervisor Host ships as an **MSIX**
package that (a) can be installed and **registers the OS service itself**, (b) is **updated by the
operating system** rather than by the application, and (c) receives those updates **from GitHub**.

This collides with three existing decisions:

- **ADR-0006 (accepted)** makes the binary's own `service install|uninstall` subcommands the sole
  owner of the service lifecycle. An MSIX packaged service is registered and removed by the OS at
  package install/uninstall time — the package, not the binary, owns registration.
- **ADR-0007 (accepted)** defines self-update as staging versions under `versions/<sha256>/` and
  atomically repointing a `current` junction the service targets. An MSIX installs into the
  read-only `WindowsApps` tree: the binary cannot be replaced by the Updater, and the service's
  program path is fixed by the package. ADR-0007's mechanism is structurally impossible for an
  MSIX install — updates must come from the OS instead.
- **ADR-0009 (proposed)** rejected MSIX as *the* Windows package format because it is unsuited to
  a SYSTEM-context, machine-wide agent as a *replacement* for the MSI. That reasoning was about
  substituting the primary channel; it did not consider MSIX as an *additional*, differently-owned
  channel.

Research confirms the requirement is technically satisfiable, with sharp constraints:

- **Packaged services exist.** Since Windows 10 2004 an MSIX may declare a service via the
  `desktop6:Service` manifest extension, running as `localService`, `networkService`, or
  `localSystem`. Required capabilities: `runFullTrust`, `packagedServices`, and additionally
  `localSystemServices` for LocalSystem. Installing such a package requires elevation.
- **OS-driven updates exist.** An `.appinstaller` file with `UpdateSettings` makes App Installer
  check for updates on launch and via a background task (roughly every 8 hours), fetching the
  `.appinstaller` from a fixed URL. GitHub Releases provides the needed stable URL via
  `releases/latest/download/<asset>`, while the referenced `.msix` itself may live at its
  versioned release URL.
- **Signing is mandatory.** Unlike an MSI, an unsigned MSIX cannot be installed at all; the
  manifest `Publisher` must equal the signing certificate's subject, and the certificate must be
  trusted by the target device. The `ms-appinstaller:` URI handler is disabled by default since
  2024, so installation is by downloaded `.appinstaller`/`.msix` file or `Add-AppxPackage`.
- **Reliability is the open risk.** Community reports show auto-update of packages *containing
  services* to be the least-proven corner of MSIX (elevation during background update, update
  triggers with no user logged in, service cleanup on uninstall). Prior art for exactly this
  combination is thin.

A new artifact type, a new public install contract, a signing requirement, and an exception to two
accepted ADRs are all architecture-relevant; per AGENTS.md §3 they need an ADR before any code.

## Decision

We will add a **signed MSIX plus `.appinstaller` as an additional, experimental Windows channel**
for the Supervisor Host — alongside, never instead of, the `.zip` archive and the `.msi` of
ADR-0008/0009 — with **install-method-scoped ownership**: for an MSIX install, the OS owns both the
service and updates; for every other install method, ADR-0006 and ADR-0007 continue to apply
unchanged.

- **The package registers the service.** The committed `AppxManifest.xml` declares the
  `desktop6:Service` extension (StartupType auto, `StartAccount="localSystem"`, starting the
  existing `run --service` SCM entry from ADR-0006's Windows shim), with capabilities
  `runFullTrust`, `packagedServices`, `localSystemServices`, and `allowElevation`. Installing the
  MSIX (elevated) installs and registers the service; uninstalling removes both.
- **The OS owns updates.** A `supervisor-host.appinstaller` file (stable asset name, resolved via
  `releases/latest/download/…`) carries `UpdateSettings` (`OnLaunch`, `AutomaticBackgroundTask`,
  `ForceUpdateFromAnyVersion`) and points at the versioned `.msix` release asset. Publishing a new
  GitHub Release is the whole update mechanism; App Installer applies it.
- **One owner per install method, enforced in code.** When the binary detects it is running from
  an MSIX package (`GetCurrentPackageFullName`), the `service install|uninstall` and `update`
  subcommands refuse with a clear error naming the OS-managed channel. This preserves ADR-0006's
  one-owner principle by *scoping* it: exactly one owner exists per install, never two.
- **This ADR supersedes ADR-0009's rejection of MSIX only as far as it reads as a ban on an
  additional channel**; the MSI remains the primary Windows installer and ADR-0009's payload-only
  design is untouched. It likewise **carves the MSIX install out of ADR-0006's service-lifecycle
  ownership and out of ADR-0007's self-update**, which remain binding for archive and MSI
  installs. (Future OpAMP package delivery will accordingly have to report the MSIX install as
  not accepting packages — capability honesty per the specification.)
- **Signing: self-signed now, real signing later.** CI signs with a self-signed certificate held
  as GitHub secrets (`MSIX_SIGNING_PFX` base64 + `MSIX_SIGNING_PASSWORD`); the public
  `opamp-fleet-supervisor-host.cer` ships as a release asset and must be imported once into the
  target machine's *Trusted People* store (automatable via Group Policy). Azure Trusted Signing
  (or an OV/EV certificate) is the named follow-up that removes that step and would also cover
  the MSI's SmartScreen gap (ADR-0009).
- **Versioning and naming follow ADR-0008.** The MSIX version is the normalised SemVer mapped to
  the required four-part form `MAJOR.MINOR.PATCH.0`; the artifact is
  `opamp-fleet-supervisor-host-<version>-x86_64.msix` with a `.sha256` sidecar, built by the
  existing release workflow's Windows leg on every run and published only with `publish: true`.
- **The channel is labelled experimental** in the README until the service auto-update path has
  proven itself on real hosts; the archive and MSI remain the recommended production paths.

## Alternatives considered

- **Keep the MSIX payload-only like the MSI (no packaged service, no `.appinstaller`)** — no ADR
  conflict, but it satisfies none of the actual requirements (package-registered service,
  OS-driven updates) and would duplicate the MSI's role in a second format for nothing.
- **Hybrid: package registers the service, updates stay with ADR-0007's Updater** — impossible in
  practice: the binary under `WindowsApps` is read-only and the package-registered service path is
  fixed, so the Updater could neither stage nor repoint; two owners would fight over one service.
- **Replace the MSI with the MSIX** — rejected by the maintainer (explicitly an *additional*
  channel) and by ADR-0009's still-valid analysis: MSIX auto-update for SYSTEM services is too
  unproven to be the only Windows installer, and fleet-provisioning tooling widely expects MSI.
- **Distribute via winget / the Microsoft Store instead of a GitHub-hosted `.appinstaller`** —
  winget is unsupported in the SYSTEM context (ADR-0009's sources) and the Store adds a
  certification pipeline and a distribution owner outside the project; GitHub Releases is already
  the project's distribution point (ADR-0008).
- **Azure Trusted Signing from day one** — removes the certificate-distribution step and is the
  better end state, but requires an Azure account and identity validation the maintainer does not
  have today; a self-signed certificate makes the channel shippable now and Trusted Signing is a
  drop-in follow-up (swap the signing step, re-release).
- **A separate update-watcher service polling GitHub (Squirrel/Velopack style)** — reimplements
  what App Installer already does, adds a second process and a second update code path next to
  ADR-0007, and contradicts "the OS updates it" — the stated requirement.

## Sources / Prior art

- `desktop6:Service` manifest element (service accounts, `packagedServices` /
  `localSystemServices` capabilities):
  <https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop6-service>;
  converting installers with services: 
  <https://learn.microsoft.com/en-us/windows/msix/packaging-tool/convert-an-installer-with-services>;
  worked example of a sideloaded MSIX service (manifest, self-signed cert to *Trusted People*,
  caveats): <https://github.com/peterwishart/MsixServiceExample>; background on MSIX services:
  <https://www.advancedinstaller.com/msix-windows-services.html>.
- `.appinstaller` schema and update settings (`OnLaunch`, `AutomaticBackgroundTask`,
  `ForceUpdateFromAnyVersion`, hours between checks):
  <https://learn.microsoft.com/en-us/windows/msix/app-installer/app-installer-file-overview> and
  <https://learn.microsoft.com/en-us/uwp/schemas/appinstallerschema/app-installer-file>.
- `ms-appinstaller:` protocol disabled by default (Dec 2023/2024, malware abuse; Group Policy
  re-enable): <https://techcommunity.microsoft.com/blog/windows-itpro-blog/disabling-the-msix-ms-appinstaller-protocol-handler/3119479>
  and <https://learn.microsoft.com/en-us/windows/msix/app-installer/installing-windows10-apps-web>.
- Signing/sideloading requirements (trusted certificate, *Trusted People* store for self-signed):
  <https://learn.microsoft.com/en-us/windows/msix/package/signing-package-overview>.
- Reported reliability limits of auto-updating service packages (elevation during update, timing
  without a logged-in user):
  <https://techcommunity.microsoft.com/discussions/msix-discussions/service-msix-automatic-update/3453548>
  and <https://techcommunity.microsoft.com/t5/msix/auto-update-an-app-package-with-a-windows-service/m-p/1894555>.
- GitHub `releases/latest/download/<asset>` stable URLs:
  <https://docs.github.com/en/repositories/releasing-projects-on-github/linking-to-releases>.
- Specification (Goal #11 self-update; Capability honesty; Supervisor Host as the one deployable)
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); ADR-0006 (service ownership this ADR scopes),
  ADR-0007 (self-update this ADR excludes for MSIX installs), ADR-0008 (release workflow,
  versioning, naming), ADR-0009 (the MSI channel that stays primary, and the MSIX rejection this
  ADR narrows).

## Consequences

- Positive: on Windows an operator gets the platform's most idiomatic experience — install one
  signed package and a LocalSystem service is running; updates arrive without any agent-side
  update machinery by publishing a GitHub Release; uninstall removes service and binary together.
  The channel is additive: archives and the MSI, and with them ADR-0006/0007 semantics, are
  untouched, and the in-code guard makes the two ownership models impossible to mix on one
  machine install. Signing infrastructure (secrets, workflow step, `.cer` distribution) is
  groundwork the MSI's Authenticode follow-up can reuse.
- Negative / trade-offs: the channel is **experimental where it matters most** — auto-update of a
  package containing a SYSTEM service is the least-proven MSIX path, and its failure mode (update
  never applies) is silent, so real-host smoke tests must gate any "recommended" label. A
  self-signed certificate must be distributed to and trusted by every target machine before
  install — real friction that Group Policy mitigates but does not remove, and a trust bar *lower*
  than a public CA until Trusted Signing lands. The Windows release leg grows a second artifact
  path (`makeappx`/`signtool`, manifest, `.appinstaller` templating) and two repository secrets;
  losing the PFX breaks the update chain for existing installs (a new cert means re-trusting).
  MSIX installs cannot take part in ADR-0007 self-update or future OpAMP package delivery — the
  Agent must truthfully report the lacking capability. CI can build and sign the MSIX but cannot
  install it or exercise the packaged service and App Installer update — the same manual-smoke-test
  caveat as ADR-0006/0007/0009, now including the update loop itself.
- Follow-ups: **Azure Trusted Signing** (replacing the self-signed certificate, covering the MSI
  too); a real-host smoke-test checklist for the update loop; verifying `desktop6:Service`
  argument passing on current Windows builds (fallback: SCM-launch detection in `run`); reporting
  the MSIX install's package-capability honestly once OpAMP package delivery lands; revisiting
  the *experimental* label once the update path has proven itself.
