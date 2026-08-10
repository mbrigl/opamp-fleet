# ADR-0046: A release also ships native installers — `.deb`, `.rpm`, and an `.msi` that asks for the install root and the endpoint

- **Status:** 🟢 accepted
- **Date:** 2026-08-10
- **Deciders:** Markus Brigl

## Context

[ADR-0025](0025-release-pipeline-and-artifacts.md) gave the project something to install: five `.7z`
archives, one per target, packed by `opamp-package-sign` so that the file an operator downloads is
also a valid package artifact for a fleet self-update ([ADR-0020](0020-client-self-update.md),
[ADR-0031](0031-per-platform-package-variants.md)). That coupling is the whole point of the format
and nothing here weakens it.

It answers one question and not the other. `.7z` is the shape the **fleet** needs. It is not the
shape a **host** needs. An operator on Ubuntu or RHEL has `apt` and `dnf` — an inventory of what is
installed, an upgrade path, a removal that leaves nothing behind — and gets none of it from an
archive unpacked by hand. An operator on Windows has Intune, Group Policy, and SCCM, all of which
deploy an `.msi` and none of which deploy a `.7z`. The documented first contact with this product is
currently `7z x`, `service install --interactive`, `service start` — three commands, typed as root,
on every host.

The prior art this project already cites is unanimous on the point.
`opentelemetry-collector-releases` — ADR-0025's naming source — publishes `.deb`, `.rpm` and `.msi`
alongside its archives. Elastic Agent, ADR-0027's closest prior art, does the same. Neither replaces
the archive; both add to it, because the two artifacts answer different questions.

Four forces shape the answer, and three of them are already decided elsewhere.

**`service install` already owns the install, and it must keep owning it.**
[ADR-0010](0010-client-os-service-and-cli.md) lays out `<root>/versions/<version>/`, a `current`
pointer, and `<root>/state/`, registers the unit or the SCM entry, and absolutizes every path into
the installed command line; [ADR-0030](0030-one-service-name-on-every-platform.md) gives the
registration its single-token name and its Windows display name through a post-install call the
`service-manager` backend cannot make; ADR-0020 sets the SCM recovery actions that same backend
discards. A native package that registered a service of its own would produce a registration missing
both of those, on the one platform where they were hardest to get right.

**Self-update and a package manager must not own the same bytes.** ADR-0020's self-update writes a
new version directory under `<root>` and swaps `current`. If `dpkg`, `rpm` or the MSI owned those
paths, every fleet-driven update would put the host into a state `dpkg -V` reports as modified and
the next `apt upgrade` silently reverts — the Server's decision undone by the host's. The two
ownerships have to be disjoint, and they can be: the package owns the delivered binary, the fleet
owns what `service install` staged from it.

**A Client with no configuration is a defect, not a default.**
[ADR-0027](0027-interactive-install-writes-the-first-configuration.md) named this precisely: with no
config file the Client dials `ws://127.0.0.1:4320/v1/opamp` forever, and "the host is not broken and
it is not managed either. That silent half-state is the defect this decision closes." A package
installed on a thousand hosts must not manufacture that state a thousand times.

**The MSI has to ask, and `install` has to be able to be told.** ADR-0027 made the questionnaire
`--interactive` only, and made `--interactive` an *error* without a terminal — explicitly so that
"an Ansible play, an MDM profile, or an MSI wrapper" cannot hang a deploy. It named the MSI wrapper
as a caller and left it no way to pass an endpoint. That is the gap this decision closes: an MSI
dialog is not a terminal, and the answer it collects has to reach the configuration through a flag.

## Decision

We will publish, **in addition to** the five `.7z` artifacts and without changing them, a **`.deb`
and an `.rpm` for each Linux architecture** and **one `.msi` for Windows**, in which the package
delivers the binary and **`opamp-fleet-client service install` still performs the install** — and the
MSI collects the **install root** and the **Server endpoint** and passes both to it.

### 1. Two classes of release asset, and only one of them is a package artifact

| asset | opened by | named by | parsed by |
|---|---|---|---|
| `.7z` | the Client, on a self-update | ADR-0032 | the fleet view's prefill, the upload loop |
| `.deb` / `.rpm` / `.msi` | `dpkg` / `rpm` / Windows Installer | ADR-0032 (see clause 4) | nothing |

The `.7z` set is untouched: same five targets, same packer, same names, same role as the artifact a
Server holds and offers (ADR-0025 clause 3, ADR-0031). The Client cannot open a `.deb` and no Server
will ever be handed one — the installers are **operator** artifacts, and nothing in the fleet path
reads them. The release notes must therefore say which file is for which purpose, because a release
that offers seven Linux files without saying so is worse than one that offers two.

### 2. The package delivers; `service install` installs

Each native package places exactly one file — the release binary, the same bytes the sibling `.7z`
carries — and then invokes the Client's own install:

| | delivered to | post-install runs | pre-removal runs |
|---|---|---|---|
| `.deb` / `.rpm` | `/usr/bin/opamp-fleet-client` | `opamp-fleet-client service install` | `service stop`, then `service uninstall` |
| `.msi` | `INSTALLFOLDER\opamp-fleet-client.exe` | `… service install --root <INSTALLFOLDER> --endpoint <ENDPOINT>` | `service stop`, then `service uninstall` |

No package ships a systemd unit, a `LaunchDaemon`, or an MSI `ServiceInstall` element. `cargo-deb`'s
`systemd-units` integration and WiX's `ServiceInstall`/`ServiceControl` elements are **deliberately
unused**: each would write a second registration that knows nothing of ADR-0030's name, ADR-0020's
recovery actions, or ADR-0010's `current` pointer. There is one install path on four operating
systems, and it stays the one that already exists.

The two ownerships stay disjoint by construction. `dpkg` owns `/usr/bin/opamp-fleet-client` and
never anything under the root; `service install` stages a *copy* into
`<root>/versions/opamp-client-<version>-<hash>/`, which is what the service actually runs and what a
self-update replaces. A fleet update therefore never touches a package-owned file, and
`dpkg -V` stays quiet. The delivered binary and the running binary drift apart after a self-update —
that is correct, and the Consequences record it as the cost it is.

### 3. The Linux packages register the service and do not start it

`postinst` / `%post` runs `service install` and stops. It does not `systemctl enable --now`.

This breaks with Debian Policy's rule on init scripts and services, and with what `dh_installsystemd`
does by default, and the reason is ADR-0027's: `service install` on a host with no configuration
succeeds, and the service it would start dials the development default. Starting it would mean every
`apt install` of this package
produces the exact silent half-state ADR-0027 exists to eliminate — and produces it at fleet scale,
where nobody is watching a terminal. A registered, stopped service is honest: the host has the
Client, and it is not yet managed.

ADR-0027 clause 7 already makes `service install` warn, naming the path that will be baked into the
unit, so the operator gets the right message without the package printing one of its own. The two
remaining steps are the same on every distribution:

```console
sudo apt install ./opamp-fleet-client_1.2.3_linux_amd64.deb
sudo opamp-fleet-client service install --endpoint wss://fleet.example.com/v1/opamp
sudo systemctl start opamp-fleet-client
```

The second command is a re-install, which ADR-0010 already requires to be idempotent, and it writes
the configuration ADR-0027 refuses to overwrite once it exists. An operator who prefers to be asked
runs `service install --interactive` instead.

### 4. One naming rule for the whole release

Every asset is named `<name>_<version>_<os>_<arch>.<ext>` — ADR-0032's four `_`-separated fields,
with ADR-0031's platform vocabulary, extended over the new extensions rather than beside them:

| | artifact |
|---|---|
| archive | `opamp-fleet-client_1.2.3_linux_amd64.7z` |
| Debian | `opamp-fleet-client_1.2.3_linux_amd64.deb` |
| RPM | `opamp-fleet-client_1.2.3_linux_amd64.rpm` |
| Windows | `opamp-fleet-client_1.2.3_windows_amd64.msi` |

…and the same four for `arm64`, minus the MSI (ADR-0025 keeps Windows on arm64 out; it is one row
when a deployment asks for it).

**The file name is not what a package manager reads.** `rpm` and `dpkg` resolve architecture from the
metadata *inside* the package, and there each ecosystem keeps its own vocabulary — the `.rpm` records
`x86_64` / `aarch64`, the `.deb` records `amd64` / `arm64`, both derived by the packaging tool from
the Rust target triple. Only the name is uniform, and the name is the thing an operator globs and a
release page sorts. `otelcol_0.158.0_linux_amd64.deb` is exactly this shape, from the project ADR-0025
took its naming from.

The `<version>` is the base version, as everywhere else (ADR-0025 clause 4,
[ADR-0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md)). It needs no epoch
and no `~` pre-release mangling, because [ADR-0026](0026-version-from-cargo-toml.md)'s pipeline
already refuses to release anything that is not `MAJOR.MINOR.PATCH`. The RPM `Release` field is `1`
and stays `1`: a version is released once (ADR-0026), so there is never a second build of one.

### 5. The MSI asks two questions, and both are ordinary public properties

The UI is the stock **`WixUI_InstallDir`** dialog set with one dialog inserted before the directory
page:

| property | dialog | passed to |
|---|---|---|
| `INSTALLFOLDER` | `InstallDirDlg` (stock), selected by setting `WIXUI_INSTALLDIR` | `service install --root` |
| `ENDPOINT` | one inserted dialog with an `Edit` control | `service install --endpoint` |

`INSTALLFOLDER` is **one directory for everything** — the delivered `.exe`, the written
`client.toml`, `versions/`, `current`, and `state/` all live under it, because ADR-0010's `--root`
already anchors all of them and the operator should configure one path, not half of one.

Both names are **all uppercase**, which is not cosmetic: Windows Installer resets private properties
when execution crosses from the UI sequence to the execute sequence, so a value typed in a dialog
reaches a custom action only if the property is public. Both are additionally listed in
`SecureCustomProperties`, which makes the identical MSI an unattended one — the same two answers on
the command line, which is how Intune and SCCM will actually deploy it:

```console
msiexec /i opamp-fleet-client_1.2.3_windows_amd64.msi /qn ^
  INSTALLFOLDER="C:\Program Files\OpAMP Fleet Client" ^
  ENDPOINT="wss://fleet.example.com/v1/opamp"
```

The registration runs as a **deferred custom action with `Impersonate="no"`**, because it writes
under `Program Files` and registers a service, and neither is something the invoking user is
guaranteed to be allowed to do. It is a type 18 action — an executable installed by this package —
whose command line "commonly contains properties that are designated dynamically": the `Target`
field is a formatted string resolved when the installation script is written, so both properties
reach it without a `CustomActionData` round trip. It is sequenced after `InstallFiles`, which that
action type requires, since the executable it runs is the one being installed.

An `ENDPOINT` left empty is allowed and means "no `--endpoint`": the install then behaves exactly as
a Linux one, warning and leaving the file to be written later. That is two custom actions under
opposite conditions rather than one with an empty flag, because `--endpoint ""` would be *rejected*
by the loader's endpoint rule while no flag at all is the ordinary deferred-configuration install.

The sequence is `WelcomeDlg → InstallDirDlg → EndpointDlg → VerifyReadyDlg`: **no licence page.**
Apache-2.0 requires no click-through, and `LicenseAgreementDlg` wants the licence as RTF — a second
copy of `LICENSE` in a format nothing else here uses and that would have to be kept in sync.
Dropping it is the worked example in WiX's own customization guide. The set is a *copy* of the
toolset's `WixUI_InstallDir` fragment with the navigation re-pointed, which is what that guide
prescribes; overriding a stock set's rows in place would depend on control-event ordering the
toolset is free to renumber, and a wrong guess there is a wizard that silently skips a page.

The MSI installs the **`default` instance** only. Multiple instances (ADR-0010) stay a CLI matter;
an MSI that enumerated them would need a product code per instance, which is a different decision
and not one anyone has asked for.

**The `UpgradeCode` is a GUID minted once and never changed again.** It is the identity by which
Windows Installer recognises version 1.2.4 as an upgrade of 1.2.3 rather than a second product
installed beside it; changing it later strands every host that already has the old one. It is
therefore a constant in the WiX source with a comment saying exactly that, paired with
`MajorUpgrade` so a new version removes the old. The `ProductCode` is regenerated per version, which
is what `MajorUpgrade` requires.

### 6. What builds them, and where

| | tool | version | licence | runs on |
|---|---|---|---|---|
| `.deb` | `cargo-deb` | 3.7.0 (2026-05-02) | MIT | the two Linux runners |
| `.rpm` | `cargo-generate-rpm` | 0.21.0 (2026-05-04) | MIT | the two Linux runners |
| `.msi` | WiX Toolset, as the `wix` .NET tool | 6.x | MS-RL | the Windows runner |

All three are **additional steps in the existing `build` matrix job**, after the release binary is
built and the `.7z` is packed, and all three consume the binary that job already produced —
`cargo deb --no-build`, `cargo generate-rpm` pointed at the same target directory, and a WiX source
that harvests one file. Nothing is compiled twice, so the bytes in the `.deb` are provably the bytes
in the `.7z`. The arm64 Linux runner packages arm64 natively; no cross-packaging is needed.

`cargo-generate-rpm` builds the RPM through the `rpm` crate and needs no `rpmbuild` on the runner,
which is why the RPM can be produced on an Ubuntu runner at all. `cargo-deb` derives `Depends` from
the binary's actual shared-library needs (`$auto`), so the glibc floor in the package is the one the
build really has rather than one somebody typed.

Package metadata lives in `crates/client/Cargo.toml` under `[package.metadata.deb]` and
`[package.metadata.generate-rpm]` — read from the workspace's own `version`, `license` and
`description` rather than restated. The WiX sources live in `packaging/windows/`.

The `SHA256SUMS` file covers the new assets too, and a `workflow_dispatch` dry run builds and packs
all of them and publishes nothing, exactly as it does today (ADR-0025 clause 1).

### 7. `service install` gains `--endpoint`

A new non-interactive flag on `service install`: it writes the same first configuration
`--interactive` writes — through ADR-0027's existing renderer and its `write_new`, so the file's
shape, its `0600` mode, and the never-overwrite rule are unchanged — with the endpoint **given**
rather than asked. It is validated by the loader's own rule, the same one the questionnaire uses. It
is mutually exclusive with `--interactive`; with neither flag, `install` behaves as it does today.

The MSI does not write TOML. ADR-0027 clause 4 already flagged the questionnaire as "a second place
that has to follow the configuration schema"; a WiX custom action emitting TOML would be a third,
written in a language nobody here tests. One flag on the command that already owns the file is the
smaller thing.

`--endpoint` deliberately does **not** grow siblings for the credential, the CA file, or
`[self_update]`. ADR-0027 put the credential behind a hidden prompt precisely so it never lands in a
process list or a shell history, and an MSI property is written to `%WINDIR%\Installer` logs. The
endpoint alone gets a fresh host to the point where it is visibly aimed at the right Server, which
is the half a packaged install can honestly automate.

## Alternatives considered

- **The package owns the whole install** — `/usr/bin/opamp-fleet-client`, a shipped
  `/lib/systemd/system/opamp-fleet-client.service`, `/etc/opamp-fleet/client.toml`, `%ProgramFiles%`
  plus an MSI `ServiceInstall`. The conventional distro shape, and the one most operators would
  predict. Rejected on two counts, either sufficient: it forks the install path per platform, which
  ADR-0010 exists to avoid and which would leave `service install` as a fourth, differently-behaving
  variant; and it puts ADR-0020's self-update in direct conflict with the package manager over the
  same files. A packaged product whose fleet updates are reverted by `apt upgrade` is worse than one
  that ships no package.
- **`nfpm`** — one tool and one YAML file for `.deb` and `.rpm` (and `.apk`), and what
  `opentelemetry-collector-releases` uses via goreleaser. A genuinely good tool and a close call. Not
  chosen because it is a second ecosystem's binary to install and pin in CI, and because its config
  restates the version, licence and description that the two cargo subcommands read straight out of
  `Cargo.toml` — a duplicate of exactly the kind ADR-0026 spent a decision removing. Worth
  reconsidering the moment a third format (`.apk`, Arch) is wanted, where one config beats three.
- **`cargo-wix`** — the in-ecosystem choice, and it does drive WiX from `Cargo.toml`. Rejected: its
  value is generating a `.wxs` and shelling out to the toolset, and this MSI needs a custom dialog, a
  custom action and two public properties, so the `.wxs` is hand-written either way. Its current
  release (0.3.9, 2025-03-13) also predates WiX v6 and defaults to the legacy v3 toolset. Calling
  `wix build` on our own source is one layer fewer.
- **A WiX Burn bundle (`.exe`) instead of a plain `.msi`** — richer UI, can chain prerequisites.
  Rejected: the ask was an MSI, and MSI is what Intune, Group Policy and SCCM ingest; a Burn bundle
  is awkward in all three.
- **Traditional per-ecosystem file names** — `opamp-fleet-client-1.2.3-1.x86_64.rpm`, the shape a
  RHEL administrator expects. Rejected for one naming rule across the release: nothing resolves an
  RPM by its file name, ADR-0032 gave this project a rule and a reason, and the prior art ADR-0025
  cites publishes `_linux_amd64.rpm`. The ecosystem vocabulary survives where it is load-bearing —
  inside the package metadata.
- **Preseed the endpoint on Linux too** (debconf on Debian, an RPM macro or a config file read by
  `%post` on RHEL) — symmetry with the MSI, and rejected for now: two more preseed mechanisms to
  write, document and test, when `service install --endpoint` already works unattended on both and is
  one line in the Ansible play that installed the package. Recorded as a follow-up, not as a gap.
- **A macOS `.pkg`** — the same argument would justify it, and it is genuinely out of scope here: it
  needs a Developer ID, notarization, and a stapling step, which is the signing decision ADR-0025
  already deferred for the archives. Naming it as a follow-up is honest; bolting an unsigned `.pkg`
  onto this would not be.
- **Replace the `.7z` artifacts with the installers** — considered and refused. The `.7z` is the only
  format the Client can open, so dropping it removes the fleet's ability to self-update (ADR-0020,
  ADR-0025 clause 3). The installers are additive.
- **Publish to an apt/yum repository instead of attaching files to a release** — the better long-term
  answer for `apt upgrade` to mean anything, and a hosting and signing decision of its own (which
  key, which host, which retention). The files have to exist before a repository can carry them; this
  is that step.

## Sources / Prior art

- [`opentelemetry-collector-releases`](https://github.com/open-telemetry/opentelemetry-collector-releases)
  — publishes `.deb`, `.rpm` and `.msi` beside its archives, and names them
  `otelcol_<version>_linux_<arch>.deb`. ADR-0025 already took its archive naming from here; clause 4 takes
  the extension of that rule to native packages from the same place.
- [Install the Collector on Linux](https://opentelemetry.io/docs/collector/install/binary/linux/) —
  the operator-facing shape of that release: one command per distribution family.
- [Elastic Agent install documentation](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent)
  — ships `.deb`, `.rpm` and `.msi` as well as archives, and states that the archive distributions are
  the ones its fleet can upgrade from: the same split between an operator artifact and a fleet
  artifact clause 1 makes.
- [`cargo-deb`](https://crates.io/crates/cargo-deb) — 3.7.0, 2026-05-02, MIT; and its
  [systemd integration notes](https://github.com/kornelski/cargo-deb/blob/main/systemd.md), which
  document the `systemd-units` + `maintainer-scripts` + `#DEBHELPER#` mechanism clause 2 deliberately
  declines.
- [`cargo-generate-rpm`](https://crates.io/crates/cargo-generate-rpm) — 0.21.0, 2026-05-04, MIT;
  builds through the `rpm` crate, so no `rpmbuild` is needed on the runner.
- [`cargo-wix`](https://crates.io/crates/cargo-wix) — 0.3.9, 2025-03-13; the rejected in-ecosystem
  alternative.
- [WiX Toolset](https://github.com/wixtoolset/wix) and the
  [`wix` .NET tool on NuGet](https://www.nuget.org/packages/wix) — the toolset is installed on the
  runner with `dotnet tool install --global wix`; 6.x is the current stable line.
- [WixUI dialog library](https://docs.firegiant.com/wix/tools/wixext/wixui/) and the
  [`WixUI_InstallDir` reference](https://documentation.help/WiX-Toolset/WixUI_installdir.html) — the
  dialog set, and the requirement to set `WIXUI_INSTALLDIR` to an all-uppercase directory ID "because
  it must be passed from the UI to the execute sequence to take effect".
- [Adding a custom dialog to a stock WiX dialog set](https://github.com/orgs/wixtoolset/discussions/8075)
  — the documented approach (clone the dialog set's `UI` element and insert), and the confirmation
  that MSI UI authoring is unchanged from v3 to v4+.
- [Windows Installer: Public Properties](https://learn.microsoft.com/en-us/windows/win32/msi/public-properties)
  — "Properties that are to be set by the user interface during the installation and then passed to
  the execution phase of the installation must be public", and public names cannot contain lowercase
  letters. The reason both property names in clause 5 are uppercase.
- [Custom Action Type 18](https://learn.microsoft.com/en-us/windows/win32/msi/custom-action-type-18)
  — an executable installed by the package, whose command line "commonly contains properties that
  are designated dynamically", and which "must be sequenced after the InstallFiles action" when the
  executable is the one being installed. Both are why clause 5's action looks the way it does.
- [Deferred execution custom actions](https://learn.microsoft.com/en-us/windows/win32/msi/deferred-execution-custom-actions)
  — why the registration is deferred rather than immediate: deferred actions are "the only types of
  actions that can run outside the users security context", which is what registering a service
  under `Program Files` needs.
- [Customizing built-in WixUI dialog sets](https://docs.firegiant.com/wix3/wixui/wixui_customizations/)
  — copy the dialog set's fragment and re-point its `Publish` navigation, rather than overriding a
  stock set's rows in place; also the source of the worked example that removes the licence page.
- [Debian Policy, system run levels and `init.d` scripts](https://www.debian.org/doc/debian-policy/ch-opersys.html#system-run-levels-and-init-d-scripts)
  — the enable-and-start convention clause 3 knowingly departs from, and the reason it is stated rather
  than quietly skipped.
- **This project's ADR-0010, ADR-0020, ADR-0025, ADR-0027, ADR-0030, ADR-0031 and ADR-0032** — the
  install layout, the self-update this must not collide with, the artifact set this extends, the
  configuration rule the MSI must obey, the service registration no package may duplicate, the
  platform vocabulary, and the naming grammar.

## Consequences

- **Positive:** the documented first contact becomes one command per platform — `apt install`,
  `dnf install`, double-click — instead of unpack, install, start. On Windows the operator is asked
  the two things a fresh host cannot guess, in a dialog, and the same MSI deploys unattended through
  Intune with the same two answers on the command line.
- **Positive:** removal becomes real. `apt remove` and Add/Remove Programs stop the service,
  unregister it, and take the binary — which today has no counterpart at all.
- **Positive:** there is now one install path on four operating systems and one place where a bug in
  it can be fixed. The packages are wrappers, not a second implementation.
- **Positive:** a self-update never fights the package manager, because no package-owned file is
  under the install root.
- **Negative / trade-offs:** the delivered binary at `/usr/bin/opamp-fleet-client` and the running
  binary under `<root>/current/` diverge after the first fleet self-update. That is the price of the
  disjoint ownership above, and it means `dpkg -l` reports the *delivered* version, not the running
  one — `opamp-fleet-client --version` and the fleet view remain the truth. Documenting it is
  mandatory; an operator who reads `dpkg -l` and concludes the update failed has been misled.
- **Negative / trade-offs:** `apt install` leaves a stopped service, which is not what a Debian user
  expects and will be reported as a bug until the release notes say why. Clause 3 accepts that in exchange
  for never manufacturing a Client pointed at `127.0.0.1`.
- **Negative / trade-offs:** the Windows install is now configurable in a way the Linux one is not.
  The asymmetry is real, bounded (both platforms have `--endpoint`), and recorded as a follow-up.
- **Negative / trade-offs:** three more build tools in the pipeline, one of them a .NET tool, and
  three more ways for a release to fail — the same cost ADR-0025 accepted for five targets. Nothing
  is signed: the `.deb`, `.rpm` and `.msi` are unsigned exactly as the archives are, so Windows shows
  an unknown-publisher prompt and `rpm` reports no signature. This is the same deferred signing
  decision, now with a second reason to make it.
- **Negative / trade-offs:** a permanent `UpgradeCode` GUID is a decision that cannot be revisited
  without stranding installed hosts.
- **Follow-ups:** signing — an Authenticode certificate for the MSI and a GPG key for the RPM, which
  is the archive-signing decision ADR-0025 deferred, now covering three more artifacts. Hosting an
  apt and a yum repository, so `apt upgrade` reaches this product at all. Preseeding the endpoint on
  Linux the way the MSI does on Windows. A macOS `.pkg`, which waits on notarization. Whether the
  Server binary deserves the same treatment — ADR-0025 asked the same question and gave the same
  answer, that it is deployed by an operator rather than by the fleet.
