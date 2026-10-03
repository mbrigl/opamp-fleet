# ADR-0023: The fleet's own agent is called `supervisor`, and a release ships it as `.tar.gz` archives and native installers that run its own install

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl
- **Applies to:** .github/workflows/release.yml, packaging/, the `[package.metadata.deb]` and `[package.metadata.generate-rpm]` tables of crates/fleet-agent/Cargo.toml, the program, Agent type and configuration-file names of the Client, `service install --endpoint`

## Context

An operator needs something to install, and a fleet needs something to hand its Server. They are
different questions. The fleet needs one artifact per platform that the Client itself can open and
install as a self-update ([ADR-0021](0021-the-client-updates-itself.md)). A host needs what its
platform expects — `apt` and `dnf` with an inventory, an upgrade path and a clean removal; on
Windows an `.msi`, the only thing Intune, Group Policy and SCCM deploy.
`opentelemetry-collector-releases` and Elastic Agent both publish native packages *beside* their
archives, for exactly that reason.

Three forces constrain both:

- **`service install` owns the install** ([ADR-0014](0014-the-client-as-an-installed-service.md)):
  the versioned layout, the `current` pointer, the registration with its single-token name, display
  name and Windows recovery actions. A native package that registered a service of its own would
  write a second registration missing all of that.
- **Self-update and a package manager must not own the same bytes.** The self-update writes new
  version directories and swings `current`. If `dpkg`, `rpm` or the MSI owned those paths, every
  fleet update would leave the host in a state `dpkg -V` reports as modified and the next
  `apt upgrade` silently reverts.
- **A Client with no configuration is a defect, not a default.** It dials the development endpoint
  forever and manages nothing ([ADR-0014](0014-the-client-as-an-installed-service.md)); a package
  installed on a thousand hosts must not manufacture that state a thousand times.

And one force on the name. The Baseline reserves `service.name` for the Agent *type* and recommends
a reverse FQDN — a recommendation this project does not enforce
([ADR-0024](0024-what-an-agent-reports-about-itself.md)), and which opamp-spec issue 131 records as
an overload of that key. Among Collectors, Foreign Agents and the process that supervises them, the
useful type for the last is the role it plays. What the fleet offers the thing is named after what
the thing is, and so is the thing itself. Artifact names are a public contract: operators script
against them.

## Decision

We will call the fleet's own agent `supervisor` at every layer where it is the program, and publish
each release as one `.tar.gz` per target named `supervisor_<version>_<os>_<arch>` — the fleet's
artifact — plus a `.deb`, `.rpm` and `.msi` that deliver the same binary and run its own
`service install`.

### The name

1. **The Agent type is `supervisor`** — the constant `CLIENT_AGENT_TYPE` every Client reports as
   `service.name` ([`agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs)), the same on every
   host, because every Client in a fleet is the same kind of thing.

2. **The package that carries the Client is `supervisor` too.** `[self_update] package` defaults to
   the Agent type ([ADR-0021](0021-the-client-updates-itself.md) clause 2), and a Server offers a
   package under the Agent type it is built for ([ADR-0030](0030-packages-and-deployments.md)) — so
   the release a fleet uploads for its Clients is a package for Agent type `supervisor`, and no
   per-host setting is needed. A Configuration carrying the Client's `[[supervisor]]` blocks
   ([ADR-0022](0022-a-supervisors-directory-program-and-set.md)) is typed `supervisor` as well.

3. **The program is `supervisor`.** `supervisor.exe` on Windows: the binary Cargo builds
   (`[[bin]] name = "supervisor"` in the package `fleet-agent`), the file in every version directory
   (`supervisor-<MAJOR.MINOR.PATCH>-<hash>`), the member a package artifact carries, the payload file
   under `/usr/libexec/<PRODUCT_NAME>/`, the log file, the self-check token and the CLI's own name.
   The configuration file is **`supervisor.toml`**, and the `--config` default with it. What names
   an *installation* — the install path, the service, the dpkg/rpm package, the `PATH` symlink — is
   the product's name, not the program's
   ([ADR-0014](0014-the-client-as-an-installed-service.md) clauses 2–4). The program's name is not
   derived from the product's, so one published package serves every variant build: the member a
   self-update extracts is the same in all of them. Two names deliberately stay where they are: the
   Cargo package `fleet-agent`, a build-time identifier that never leaves the repository, and the OTLP
   instrumentation scope `opamp-fleet-client`, which names the library a signal came from —
   renaming it would move every operator's dashboards.

4. **The instance name is a separate attribute, and its default is `Supervisor Agent`.** The
   top-level `name` is the operator's name for *this* Client, reported as `service.instance.name`
   ([ADR-0024](0024-what-an-agent-reports-about-itself.md)). Its default is a display name — spaces
   and capitals — and deliberately not `supervisor`: a default equal to the type would print the
   same word in both columns of the fleet view. Nothing resolves a path or a service from this key.

5. **One name per thing, with no alias.** No artifact carries the program twice, no version
   directory holds a compatibility link, and the loader reads no configuration file but the one it
   is given. **A Client that finds no `supervisor.toml` at its configured path but a `client.toml`
   beside it refuses to start**, naming both paths and the `mv` that fixes it: coming up on defaults
   there — dialling the development endpoint and managing nothing — is the one outcome nobody would
   see. An artifact whose member is not `supervisor` is refused by the self-update and the host
   stays on the version it runs.

### The release

6. **A release is one workflow run that builds five targets from one version.** The version and its
   `version/*` tag are decided as [ADR-0013](0013-versions.md) states. A first job decides the
   number, builds the Client and asks it `--version`; every artifact is named from that one answer,
   and each target's build asserts its own binary reports the same full version (except the
   cross-compiled macOS x86_64 build, which the runner cannot execute). Every job checks out with
   full history so the baked version is the tag's. A `workflow_dispatch` dry run — the default —
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

7. **Every archive is a `.tar.gz` written by `opamp-package-sign pack --format tar.gz`.** The packer
   names the single member after the program, with its executable mode, so **a release archive is a
   valid package artifact unmodified**: the operator uploads the file they downloaded, and the
   SHA-256 the packer prints is both the published checksum and the hash an Agent verifies
   ([ADR-0019](0019-package-delivery-on-the-agent.md)). `.tar.gz` because it is the container every
   other agent's package ships as and the one that carries the executable bit and unpacks the same
   way on every platform. `.7z` remains a package container the Client opens and the packer writes,
   including encrypted ([ADR-0019](0019-package-delivery-on-the-agent.md)); a release has no use for
   encryption.

8. **Every asset is named `supervisor_<version>_<os>_<arch>.<ext>`.** Four fields separated by `_`:
   the package name, the base version `MAJOR.MINOR.PATCH` without build metadata, and the platform
   in the vocabulary an Agent reports as `os.type` and `host.arch`
   ([ADR-0020](0020-the-package-store.md)). **All artifacts of a target share the stem** and differ
   in extension alone:

   | | artifact |
   |---|---|
   | archive | `supervisor_1.2.3_linux_amd64.tar.gz` |
   | Debian | `supervisor_1.2.3_linux_amd64.deb` |
   | RPM | `supervisor_1.2.3_linux_amd64.rpm` |
   | Windows | `supervisor_1.2.3_windows_amd64.msi` |

   `_` because it is the one separator no field contains: the name grammar is `[a-z0-9-]` and SemVer
   spells a pre-release with `-`, while neither admits `_`. It is also the shape an operator already
   reads — GoReleaser's default, the Collector's own `otelcol_<version>_linux_amd64.tar.gz`,
   Debian's `<package>_<version>_<arch>.deb`. Where a platform is a *tag* rather than a field of a
   file name — the store's `<os>-<arch>.bin`, the fleet view's platform column — it stays
   `<os>-<arch>`, for the mirror-image reason. Inside the `.deb` and `.rpm` the architecture is the
   ecosystem's own (`amd64`/`arm64`, `x86_64`/`aarch64`), derived by the packaging tool; nothing
   resolves a package by its file name. **A published asset is never renamed or rewritten**: a
   checksum published against a URL stays true.

9. **A file name is read from the right, and an unknown tail fills nothing.** The fleet view's
   upload form reads an artifact's name by matching the platform tokens it knows — including the
   aliases `x86_64`, `aarch64`, `macos` — at the end of the stem, separated by `_` or `-`; the
   version starts at the first separator followed by a digit, and the rest is the name. A tail it
   does not know fills no field rather than a guess, because a wrong platform is an entry no Agent is
   ever offered. Accepting `-` serves upstream artifacts named by upstream. The release notes' upload
   loop splits on `_` and needs to be told neither name nor version.

10. **What is published.** A GitHub release named after the version holds the five archives, the
    two `.deb`, the two `.rpm`, the `.msi` and one `SHA256SUMS` covering every asset. The notes say
    which file is for which purpose, give the per-platform install commands and the procedure that
    uploads the archives as one package for Agent type `supervisor`, and state the full baked
    version string as provenance — the upload uses the release number, since versions are compared
    without build metadata ([ADR-0013](0013-versions.md)). Nothing is signed. A change to the asset
    names or containers breaks operators' scripts and is named in `CHANGELOG.md` and the release
    notes of the release that carries it.

### The installers

11. **Two classes of asset, and only one is a package artifact.** The archive is opened by the
    Client on a self-update and parsed by the fleet view; the `.deb`, `.rpm` and `.msi` are opened by
    `dpkg`, `rpm` and Windows Installer and by nothing in the fleet path. The installers are
    **additive operator artifacts**: they never replace the archive, which is the only format the
    fleet can install.

12. **The package delivers one file; `service install` installs.**

    | | delivers | after install | before removal |
    |---|---|---|---|
    | `.deb` / `.rpm` | `/usr/libexec/<PRODUCT_NAME>/supervisor` | `… service install` | `service stop`, then `service uninstall` |
    | `.msi` | `INSTALLFOLDER\supervisor.exe` | `… service install [--endpoint <ENDPOINT>] [--no-self-update]` | `service stop`, then `service uninstall` |

    **No package ships a systemd unit, a `LaunchDaemon` or an MSI `ServiceInstall` element**;
    `cargo-deb`'s `systemd-units` and WiX's `ServiceInstall`/`ServiceControl` are deliberately unused.
    There is one install path on every platform. The ownerships are disjoint by construction: the
    package owns its payload and never anything in the layout; `service install` stages a *copy* into
    the layout, which is what the service runs and what a self-update replaces, so `dpkg -V` and
    `rpm -V` stay quiet through every fleet update. The pre-removal runs only on a real removal — on
    an upgrade it falls straight through, because the successor's post-install re-runs
    `service install`, and unregistering would stop a managed host in the middle of `apt upgrade` —
    and it calls the payload path, never the symlink, and tolerates a service already unregistered.
    The dpkg/rpm package identity is `PRODUCT_NAME` and the MSI's is its display name and
    `UpgradeCode`, so an `apt`, `dnf` or MSI upgrade stays an upgrade.

13. **No installer starts the service it registers.** The Linux post-install and the MSI register the
    service and leave it stopped — knowingly departing from Debian Policy and `dh_installsystemd` —
    because a Client with no configuration would dial the development default, at fleet scale where
    nobody watches a terminal. The one exception is an upgrade of a Linux host whose service was
    running: it is restarted, so a configured host finishes the upgrade on the delivered binary. The
    post-install prints the two remaining steps:

    ```console
    sudo opamp-fleet service install --endpoint wss://fleet.example.com/v1/opamp
    sudo systemctl start opamp-fleet
    ```

    The second `service install` is a re-install, which is idempotent and writes the configuration
    that `service install` refuses to overwrite once it exists.

14. **The CLI on `PATH` is a symlink through `current`, and a package removal takes the layout.**
    `/usr/bin/<PRODUCT_NAME>` → `/opt/<PRODUCT_NAME>/current/supervisor`, so the operator's first
    diagnostic command answers for the binary the service runs, not for the one the package
    delivered. It is laid by the maintainer scripts, which both formats honour, rather than shipped
    as a packaged file:

    | hook | does |
    |---|---|
    | `postinst` / `%post` | `service install`, then `ln -sfn` the symlink |
    | `%posttrans` (rpm only) | `ln -sfn` again — rpm erases the old package's files *after* the new `%post`, which may take the link; dpkg removes them during unpack and needs nothing |
    | `postrm` / `%postun`, real removal only | remove the symlink (only if it is one), and the layout root `/opt/<PRODUCT_NAME>` with every staged version and `current`; on dpkg **purge**, also the data root `/var/lib/<PRODUCT_NAME>` |

    A removal keeps the data root — the Agent's identity and a credential the operator typed are not
    binaries, and a reinstall picks them up — while the layout goes, so a reinstall comes up on the
    package it installed rather than on a surviving `current`. Purge is dpkg's "leave nothing"; rpm
    has none. `service uninstall` itself still deletes nothing
    ([ADR-0014](0014-the-client-as-an-installed-service.md)); this cleanup is the package's. Re-staging
    **skips the write when the staged binary already holds the running bytes**, because
    `service install` invoked through the symlink runs from the very file it would overwrite and Linux
    refuses that (`ETXTBSY`). Symlink and cleanup name the default system roots only — the only roots
    a packaged install uses; an install rooted with `--root` is a manual one and nothing packaged
    touches it.

15. **The MSI asks for the installation folder, the endpoint and the self-update consent, all as
    public properties.** The UI is a copy of the stock `WixUI_InstallDir` set with one dialog inserted —
    `WelcomeDlg → InstallDirDlg → EndpointDlg → VerifyReadyDlg` — and **no licence page**: Apache-2.0
    needs no click-through, and the dialog wants a second copy of `LICENSE` as RTF. It is a copy with
    the navigation re-pointed, as WiX's customization guide prescribes, because overriding a stock
    set's rows in place depends on control-event ordering the toolset may renumber.

    | property | dialog | effect |
    |---|---|---|
    | `INSTALLFOLDER` | `InstallDirDlg`, via `WIXUI_INSTALLDIR` | where the payload goes; default `C:\Program Files\<PRODUCT_NAME>` |
    | `ENDPOINT` | `EndpointDlg`, an `Edit` control | `service install --endpoint`; empty means none |
    | `SELFUPDATE` | `EndpointDlg`, a checkbox, default `1` | the consent of [ADR-0021](0021-the-client-updates-itself.md) clause 3 |

    `INSTALLFOLDER` is **one directory the operator configures**, holding only the delivered payload.
    The layout and the state directory go to `%ProgramData%\<PRODUCT_NAME>` and no root reaches the
    command line ([ADR-0014](0014-the-client-as-an-installed-service.md)), so an uninstall empties the
    folder and the configuration survives where Windows expects data to. All names are upper-case and
    listed `Secure`: Windows Installer resets private properties between the UI and execute sequences,
    and `Secure` lets the same MSI run unattended:

    ```console
    msiexec /i supervisor_1.2.3_windows_amd64.msi /qn ^
      ENDPOINT="wss://fleet.example.com/v1/opamp" SELFUPDATE=0
    ```

    Registration is a **deferred type 18 custom action with `Impersonate="no"`**, after
    `InstallFiles` — the executable it runs is the one being installed — whose formatted command line
    receives the properties without a `CustomActionData` round trip. It is two actions under opposite
    conditions on `ENDPOINT`, because `--endpoint ""` is rejected by the loader while no flag is the
    ordinary configure-later install. Stop and unregister run only on a real uninstall
    (`REMOVE="ALL"`), best-effort, before `RemoveFiles`. **The `UpgradeCode` is minted once and never
    changed** — it is how Windows Installer recognises 1.2.4 as an upgrade of 1.2.3, and a new one
    strands every installed host — paired with `MajorUpgrade`, with the `ProductCode` regenerated per
    build. The MSI asks for no credential: a value typed into an installer is written to its log.

16. **The endpoint dialog is prefilled with the development Server, interactively only.** A
    `SetProperty` in the UI sequence, conditioned on `ENDPOINT` unset and the product not installed,
    sets `http://localhost:4320/v1/opamp`. A silent install that names no `ENDPOINT` still writes no
    configuration, so unattended deployment never acquires the development default by omission; a
    command-line value wins; clearing the field is the configure-later answer. The value names the
    host, port and path of the loader's default but with `http://`: the scheme selects the transport
    ([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)), and HTTP polling is the chosen
    transport for the click-through install.

17. **What builds the installers.** Additional steps in the release's `build` matrix job, after the
    archive is packed, consuming the binary that job built — so the bytes in a `.deb` are provably
    the bytes in the archive:

    | | tool | runs on |
    |---|---|---|
    | `.deb` | `cargo-deb`, `--no-build --no-strip` | both Linux runners, natively |
    | `.rpm` | `cargo-generate-rpm`, no `rpmbuild` needed | both Linux runners, natively |
    | `.msi` | WiX Toolset 6 as the `wix` .NET tool, with its UI extension | the Windows runner |

    Package metadata lives in `crates/fleet-agent/Cargo.toml` under `[package.metadata.deb]` and
    `[package.metadata.generate-rpm]`, reading the workspace's version, licence and description;
    `cargo-deb` derives `Depends` from what the binary links (`$auto`). The RPM `Release` is `1` and
    stays `1`, and no epoch or `~` mangling is needed, because a version is released once and is
    always `MAJOR.MINOR.PATCH` ([ADR-0013](0013-versions.md)). The maintainer scripts live in
    `packaging/linux/`, the WiX sources in `packaging/windows/`.

18. **`service install --endpoint <URL>` writes the first configuration without a terminal.** It
    writes the file `--interactive` writes, through the same renderer, with the endpoint *given*
    rather than asked: the same shape, mode `0600`, never overwriting an existing file, validated by
    the loader's own endpoint rule before anything is written. It conflicts with `--interactive`; it
    carries the self-update answer ([ADR-0021](0021-the-client-updates-itself.md) clause 3). It
    deliberately has no siblings for the credential or the CA file: a credential belongs behind a
    hidden prompt, never in a process list, shell history or installer log. The installers do not
    write TOML themselves; one flag on the command that owns the file is the smaller thing.

**Out of scope:** signing (an Authenticode certificate for the MSI, a GPG key for the RPM, the
archives); hosting apt and yum repositories; preseeding the endpoint on Linux; a macOS `.pkg`, which
needs a Developer ID and notarization; packaging the Server, which an operator deploys rather than
the fleet; publishing variant builds.

## Alternatives considered

- **Keep a product name such as `opamp-fleet-client` for the type, the package and the program.**
  The type would repeat a product name the row already carries as its instance name, and the fleet
  could not say what the Client *is*.
- **A reverse FQDN type, `io.opamp-fleet.supervisor`.** A recommendation enforced nowhere else here,
  in a table column where the short form is what gets read.
- **Rename only some layers.** A package name that is not the type splits the self-update's one
  rule into two names; a release named after the product does not fit the type a Client reports; a
  program named differently from its type is the doubled vocabulary this ends.
- **A transitional release carrying both program names.** Re-creates one file with two names in the
  packer, the layout, the loader and the documentation, and needs a second decision about when to
  withdraw the old one.
- **Derive the program's name from `PRODUCT_NAME`.** Every variant would need its own published
  package of the same bytes, because the archive member is extracted by name.
- **`.7z` releases, or a split container** (`.tar.gz` on Unix, `.zip` on Windows). The Client cannot
  open a `.zip`; `.7z` is a second container where every other agent ships `.tar.gz`.
- **Hyphen-separated file names with a parsing heuristic.** One separator cannot both occur inside a
  field and delimit it; `otelcol-2-1.0.0-linux-amd64` has no right answer.
- **`name_version_os-arch`**, the tighter grammar. A fourth shape nothing else writes, against a
  convention operators read fluently.
- **A sidecar manifest per artifact.** An operator hands one file to a Server; it must describe
  itself.
- **The package owns the whole install** — a shipped unit, `/etc` configuration, an MSI
  `ServiceInstall`. Forks the install path per platform and puts the self-update in conflict with the
  package manager over the same files; a fleet update reverted by `apt upgrade` is worse than no
  package.
- **`nfpm`** for `.deb` and `.rpm`. A good tool and a close call; a second ecosystem's binary to pin,
  restating the metadata the cargo subcommands read from `Cargo.toml`. Worth reconsidering when a
  third format is wanted.
- **`cargo-wix`.** The `.wxs` is hand-written either way for a custom dialog and action, and it
  predates WiX v6. A WiX Burn bundle is awkward in Intune, Group Policy and SCCM.
- **Per-ecosystem file names** (`…-1.2.3-1.x86_64.rpm`). Nothing resolves an RPM by its file name;
  the ecosystem vocabulary survives inside the package metadata.
- **A launcher shim at `/usr/bin`, or `service install` writing `/usr/bin` itself.** A second code
  path to do what one symlink does; and an application writing into the package manager's directory
  crosses the ownership line, while a custom-root install must not touch `/usr/bin`.
- **An MSI endpoint default in both sequences, or the loader's `ws://127.0.0.1` verbatim.** The
  first pins every silent install to localhost; the second was declined in favour of HTTP for the
  click-through install, and the two values are pinned by test rather than left to drift.
- **Rename the dpkg/rpm identity or `UpgradeCode` with the program.** Costs every host a second
  product beside the first.

## Sources / Prior art

- [`opentelemetry-collector-releases`](https://github.com/open-telemetry/opentelemetry-collector-releases)
  and [Install the Collector on Linux](https://opentelemetry.io/docs/collector/install/binary/linux/)
  — per-platform archives plus `.deb`, `.rpm`, `.msi`, named `otelcol_<version>_linux_<arch>`.
- [Elastic Agent install documentation](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent)
  — native packages beside archives, and archives as what its fleet upgrades from.
- [GoReleaser — Archives](https://goreleaser.com/customization/archive/) — the default
  `{{ .ProjectName }}_{{ .Version }}_{{ .Os }}_{{ .Arch }}`.
- [Debian FAQ, package basics](https://www.debian.org/doc/manuals/debian-faq/pkg-basics.en.html) and
  [dpkg-name(1)](https://www.man7.org/linux/man-pages/man1/dpkg-name.1.html);
  [Semantic Versioning 2.0.0](https://semver.org/) — `-` is in a version's alphabet, `_` is not.
- [The Cargo Book, environment variables](https://doc.rust-lang.org/cargo/reference/environment-variables.html)
  — `--target` builds the triple while the packer stays a host build.
- [`cargo-deb`](https://github.com/kornelski/cargo-deb) and its
  [systemd notes](https://github.com/kornelski/cargo-deb/blob/main/systemd.md);
  [`cargo-generate-rpm`](https://github.com/cat-in-136/cargo-generate-rpm);
  [`cargo-wix`](https://crates.io/crates/cargo-wix).
- [WiX Toolset](https://github.com/wixtoolset/wix), the [`wix` .NET tool](https://www.nuget.org/packages/wix),
  [WixUI dialog library](https://docs.firegiant.com/wix/tools/wixext/wixui/),
  [`WixUI_InstallDir`](https://documentation.help/WiX-Toolset/WixUI_installdir.html),
  [adding a dialog to a stock set](https://github.com/orgs/wixtoolset/discussions/8075),
  [customizing built-in dialog sets](https://docs.firegiant.com/wix3/wixui/wixui_customizations/).
- Windows Installer: [Public Properties](https://learn.microsoft.com/en-us/windows/win32/msi/public-properties),
  [Custom Action Type 18](https://learn.microsoft.com/en-us/windows/win32/msi/custom-action-type-18),
  [deferred execution custom actions](https://learn.microsoft.com/en-us/windows/win32/msi/deferred-execution-custom-actions);
  type 51 actions in `InstallUISequence` do not run under `/qn`.
- [Debian Policy, `init.d` scripts and services](https://www.debian.org/doc/debian-policy/ch-opersys.html#system-run-levels-and-init-d-scripts)
  — the enable-and-start convention clause 13 departs from.
- [rpm scriptlet ordering](https://docs.fedoraproject.org/en-US/packaging-guidelines/Scriptlets/) —
  the new `%post` runs before the old files are erased; `%posttrans` runs last.
- [FHS 3.0 `/usr/libexec`](https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch04s07.html) — binaries
  run by other programs, off `PATH`; Debian's `alternatives` — what users invoke is a link that is
  repointed.
- [OpAMP specification, `AgentDescription`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md),
  [opamp-spec issue #131](https://github.com/open-telemetry/opamp-spec/issues/131) and the
  [resource semantic conventions, `service.name`](https://opentelemetry.io/docs/specs/semconv/resource/#service).

## Consequences

- Positive: one word for one thing, from the Agent type in the fleet view to the file an operator
  edits; the Client's release is an ordinary fleet package in the container every agent uses.
- Positive: a release archive is installable by the fleet unmodified, so the hash published is the
  hash an Agent verifies; a build that lost its tags fails instead of publishing a `-dev` artifact.
- Positive: first contact is one command per platform — `apt install`, `dnf install`, a double
  click — and removal is real. The installers are wrappers around the one install path, and a
  self-update never fights the package manager.
- Positive: a file name parses back into its four fields, and the fleet view reads upstream
  artifacts and ours with one code path.
- Negative — `supervisor` names three things: the specification's Supervisor inside a Client, the
  Agent type, and the program. Documentation never uses the bare word where a file or unit is meant.
  It is also a common word: Debian's `supervisor` package installs `supervisord`/`supervisorctl`
  and does not collide, but the margin is thin.
- Negative — `dpkg -l` and `rpm -q` report the *delivered* version, which diverges from the running
  one after the first self-update; `opamp-fleet --version` and the fleet view are the truth, and the
  manual must say so. Until `service install` has run the symlink dangles, and a broken-by-hand
  layout makes the CLI fail loudly rather than answer for the wrong binary.
- Negative — `apt install` leaves a stopped service, which a Debian user does not expect; a
  click-through MSI install on a production host gets a localhost configuration rather than a
  warning, and runs HTTP polling where an unconfigured Client would choose WebSocket.
- Negative — a platform is spelled `linux_amd64` in a file name and `linux-amd64` as a tag; the
  version in a file name is the base version, so the name is not a unique build identifier.
- Negative — three more build tools, one of them .NET, five targets of which two are not what their
  runner natively is, and nothing signed: Windows shows an unknown publisher and `rpm` reports no
  signature. The `UpgradeCode` cannot be revisited without stranding installed hosts.
- Follow-ups: signing; apt and yum repositories; Linux endpoint preseeding; a macOS `.pkg`; a
  `PATH` entry through `current` on Windows, where the delivered payload and the running binary
  drift the same way; how variant builds would be published and named beside the shared archive
  stem.

## Enforcement

- [`release.yml`](../../.github/workflows/release.yml): the version job's "The binary agrees" step
  and each target's "The binary agrees with the name it will be given" (clause 6); the pack step
  (clauses 7, 8); "The RPM carries its scriptlets, not their file names", which asserts with
  `rpm -qp --scripts` that `service install`, `service uninstall`, `ln -sfn`, the layout-root and
  data-root removals are in the package, and that the RPM's architecture is the ecosystem's
  (clauses 8, 12, 14).
- [`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs):
  `the_clients_own_agent_type_is_the_one_name_this_program_has`,
  `the_clients_own_agent_reports_its_type_and_its_configured_name_separately` (clauses 1, 3, 4).
- [`config.rs`](../../crates/fleet-agent/src/config.rs):
  `the_configurations_old_name_beside_the_new_one_is_refused_rather_than_defaulted` (clause 5).
- [`service/layout.rs`](../../crates/fleet-agent/src/service/layout.rs):
  `the_directory_name_is_base_plus_hash_never_the_prerelease`,
  `restaging_identical_bytes_leaves_the_staged_binary_untouched` (clauses 3, 14).
- [`tests/msi_exe_command.rs`](../../crates/fleet-agent/tests/msi_exe_command.rs) parses the WiX source's
  command lines as the C runtime will: `register_service_with_endpoint_survives_the_crt`,
  `register_service_survives_the_crt`,
  `the_msi_names_no_root_so_no_directory_property_reaches_a_command_line`,
  `stop_and_unregister_survive_the_crt` (clauses 12, 15) and
  `endpoint_prefill_is_the_development_server_and_interactive_only` (clause 16).
- `--endpoint` (clause 18): `install_takes_an_endpoint_without_a_terminal` and
  `an_endpoint_and_interactive_are_refused_together` in [`cli.rs`](../../crates/fleet-agent/src/cli.rs);
  `an_endpoint_given_is_written_and_loads`, `a_bad_endpoint_is_refused_before_anything_is_written`,
  `an_endpoint_given_never_overwrites_an_existing_file` and
  `an_endpoint_is_validated_by_the_loaders_own_rule` in
  [`config_init.rs`](../../crates/fleet-agent/src/config_init.rs).

**Not mechanically decidable:** what `dpkg`, `rpm` and Windows Installer do with the maintainer
scripts and the MSI tables on a real host — the upgrade fall-through, the rpm `%posttrans` ordering,
the dialog sequence, the `UpgradeCode`'s permanence — is exercised only by installing a release;
no CI job installs the packages. The fleet view's prefill (clause 9) has no automated test.
