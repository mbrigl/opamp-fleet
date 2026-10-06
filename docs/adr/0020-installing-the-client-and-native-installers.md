# ADR-0020: The Client is installed by `service install` — the first configuration is asked once and never overwritten, and native `.deb`, `.rpm` and `.msi` packages deliver the binary and call it

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

[ADR-0010](0010-client-os-service-and-installation-layout.md) gave the Client the lifecycle an operator needs —
`service install | uninstall | start | stop | status`, one CLI over the `service-manager` crate,
writing the systemd unit (Ubuntu and RHEL are both systemd), the launchd `LaunchDaemon`, and the
Windows SCM registration; [ADR-0017](0017-client-self-update-and-its-consent.md) closed the last platform gap by
configuring the SCM recovery actions the `sc.exe` backend discards. That half of the operator's
question — *does this work the same on all four systems* — is answered. The program is `supervisor`
(`supervisor.exe` on Windows) and its configuration file `supervisor.toml`
([ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) clauses 9 and 10); the install
directories, the service, the native package and the `PATH` symlink carry the product's name,
`<PRODUCT_NAME>`, by default `opamp-fleet`
([ADR-0010](0010-client-os-service-and-installation-layout.md) clauses 8, 9, 11 and 15).

The **configuration** half is not, and ADR-0010 left it out deliberately: the Client is
file-configured ([ADR-0008](0008-toml-configuration.md)) and the installed unit carries the config
*path*, never the config. On a fresh host that path names nothing, and nothing says so.
`ClientConfig::load` returns the defaults when the file is absent, so `service install` succeeds, the
service starts, and the Client dials `ws://127.0.0.1:4320/v1/opamp` — the development default —
forever. The host is not broken and it is not managed either. That silent half-state is the defect
this decision closes; it is the same reason ADR-0017 refused to let a Windows self-update look like
it worked on two platforms out of three. A package installed on a thousand hosts must not
manufacture that state a thousand times.

Five forces shape the answer.

**There is nothing on the host to copy from.** [ADR-0019](0019-release-pipeline-and-artifact-names.md)
packs each release asset around the binary the Client looks for (`supervisor` / `supervisor.exe`),
precisely so a release asset is also a valid package artifact for ADR-0017 and
[ADR-0021](0021-one-platform-vocabulary.md). That coupling is the whole point of the archive
format, and nothing here weakens it. `config/supervisor.toml` is a document in this repository, not
a shipped file. An operator who follows the documented download path has a binary and no example.

**`install` already runs unattended.** It is the command an Ansible play, an MDM profile, or an MSI
wrapper invokes. A command that blocks on stdin there does not fail — it hangs, which is worse. Any
interactivity has to be something the operator asks for, never something they discover. An MSI
dialog is not a terminal either, so the answer it collects has to reach the configuration through a
flag.

**A first configuration contains a secret.** The endpoint is useless without the credential that goes
with it ([ADR-0013](0013-opamp-endpoint-admission.md)), and asking for a bearer token or a
password puts it on the screen unless the terminal is told otherwise. Suppressing the echo is
platform terminal control (`termios` on Unix, `SetConsoleMode` on Windows), not string handling.

**An archive is the shape the fleet needs, not the shape a host needs.** An operator on Ubuntu or
RHEL has `apt` and `dnf` — an inventory of what is installed, an upgrade path, a removal that leaves
nothing behind — and gets none of it from an archive unpacked by hand. An operator on Windows has
Intune, Group Policy, and SCCM, all of which deploy an `.msi` and none of which deploy an archive.
Without native packages, the first contact with this product is unpack, `service install
--interactive`, `service start` — three commands, typed as root, on every host. The prior art this
project already cites is unanimous: `opentelemetry-collector-releases` — ADR-0019's naming source —
publishes `.deb`, `.rpm` and `.msi` alongside its archives, and Elastic Agent does the same. Neither
replaces the archive; both add to it, because the two artifacts answer different questions.

**`service install` owns the install, and self-update and a package manager must not own the same
bytes.** `service install` lays out `<root>/versions/<version>/`, a `current` pointer, and the state
directory, registers the unit or the SCM entry, and absolutizes every path into the installed
command line (ADR-0010). It gives the registration its single-token name and its Windows display name
through a post-install call the `service-manager` backend cannot make (ADR-0010 clauses 11 and 12), and
it sets the SCM recovery actions that same backend discards (ADR-0017). A native package that
registered a service of its own would produce a registration missing both. ADR-0017's self-update
writes a new version directory under the layout root and swaps `current`. If `dpkg`, `rpm` or the
MSI owned those paths, every fleet-driven update would put the host into a state `dpkg -V` reports as
modified and the next `apt upgrade` silently reverts — the Server's decision undone by the host's.
The two ownerships have to be disjoint, and they can be: the package owns the delivered binary, the
fleet owns what `service install` staged from it.

The install layout already applies that rule to the service: nothing invokes a version binary
directly, everything goes through the `current` pointer, so a version switch never re-registers
anything. The CLI on `PATH` must follow the same rule. If the shell resolves the command to the
package-delivered file, then after a fleet update — or after an operator reinstalls an older package
by hand — `--version` answers for the delivered binary while the service runs what `current` names.
The operator's first diagnostic command is then the one place the drift is guaranteed to mislead,
and a warning in the manual does not prevent that. A removal must likewise mean what it says: if the
package goes and the layout stays, the next install comes up on the surviving `current` pointer,
running a version the operator believed uninstalled.

None of this overturns ADR-0008. What gets written is a *starting* file the operator keeps editing by
hand; the Client gains no second configuration mechanism, and no environment fallback.

There is no upstream answer to adopt wholesale. The OpenTelemetry `opampsupervisor` has a
hand-written configuration file and no scaffolding command at all. Elastic Agent is the closest prior
art and is instructive in both directions: `elastic-agent install` prompts for confirmation and for
Fleet enrollment by default, and offers `--non-interactive` for automation — but it also *overwrites*
`elastic-agent.yml`, and `--force` exists to skip the confirmation for that. Overwriting is exactly
the behaviour we must not copy on a file that holds a credential an operator typed.

## Decision

We will let `supervisor service install` write the Client's **first** configuration file — asked
interactively only when the operator asks for it, never overwriting a file that exists, and validated
before anything is registered. We will publish, **in addition to** the release archives and without
changing them, a **`.deb` and an `.rpm` for each Linux architecture** and **one `.msi` for Windows**,
in which the package delivers the binary and **`service install` still performs the install**. The
CLI on `PATH` resolves through the layout's `current` pointer, a package removal takes every staged
version with it, and the MSI collects the **Server endpoint** — prefilled with the development
default in its dialog only — and passes it to `service install`.

### The first configuration

1. **Interactivity is an opt-in flag on `install`, not a new command.** `service install
   --interactive` runs the questionnaire; without the flag `install` does not ask. The default stays
   non-interactive because that is what every scripted invocation relies on. `--interactive` on a
   stdin that is not a terminal is an **error**, not a silent fallback: `std::io::IsTerminal` has
   been in `std` since Rust 1.70 and the workspace MSRV is 1.97, so this costs no dependency and
   turns "hangs a deploy forever" into a message.

2. **A file that exists is never overwritten.** If the target already exists, the questionnaire is
   skipped, `install` proceeds with that file, and prints which file it kept. Re-installing stays
   idempotent, as ADR-0010 requires of the version layout, and a second `--interactive` install can
   never eat the credential typed into the first.

3. **When the operator names no path, the file goes to `supervisor.toml` in the data root** that
   `service install` already derives per scope — `/var/lib/<PRODUCT_NAME>` for a Linux system-scope
   install, the one install root everywhere else (ADR-0010 clause 9; the file name is ADR-0022
   clause 10). That is one rule for four platforms instead of a new `/etc` vs `/Library` vs
   `%ProgramData%` policy, and the absolute path baked into the unit is the one just written. An
   explicit `--config` wins; since that flag has a default value, the two cases are distinguished by
   clap's value source, not by comparing against the default string.

4. **The questionnaire asks only what has no useful default on a fresh host:** the Server `endpoint`,
   the Agent `name`, whether authentication is used and under which scheme (ADR-0013), and — only
   when the endpoint scheme is `wss://` or `https://` — the CA file for a private CA
   ([ADR-0007](0007-dual-transport-and-tls.md)). `[self_update]` (ADR-0017) is offered last; its
   default is set by [ADR-0017](0017-client-self-update-and-its-consent.md).
   Everything else is written as commented defaults, in the shape of `config/supervisor.toml`, so the
   file remains a starting point for hand-editing.

5. **The written file is treated as holding a secret:** mode `0600` on Unix; on Windows it inherits
   the install root's ACL, which for a system-scope install is already administrator-owned.

6. **It is validated before the service is registered.** The order is write → load through
   `ClientConfig::load` → lay out the versioned install → register. This preserves the
   "fail on a broken configuration now, not at the service's first start" property. A file that
   fails to load is left on disk and named in the error, never silently deleted — a typo is corrected
   by editing, not by answering five questions again.

7. **A non-interactive install with no configuration file warns.** Not an error — automation must not
   break — but a printed line naming the path that will be baked into the unit and saying the Client
   will run on defaults until that file exists. The silence is the bug.

8. **Hidden input comes from `dialoguer`.** It is the one part worth a dependency: the platform
   terminal control behind a password prompt is not something to hand-roll for three operating
   systems. Version 0.12.0 (2025-08-23) is current and maintained, it is MIT-licensed, and it brings
   no TLS or crypto backend, so ADR-0007's constraint on the rustls/ring stack is untouched.

### The native installers

9. **Two classes of release asset, and only one of them is a package artifact.**

   | asset | opened by | named by | parsed by |
   |---|---|---|---|
   | `.tar.gz` | the Client, on a self-update | ADR-0019 | the fleet view's prefill, the upload loop |
   | `.deb` / `.rpm` / `.msi` | `dpkg` / `rpm` / Windows Installer | ADR-0019 (see clause 12) | nothing |

   The archive set is untouched by the installers: same targets, same packer, same names, same role
   as the artifact a Server holds and offers (ADR-0019 clause 3, ADR-0021; the container is
   ADR-0022 clause 8's). The Client cannot open a `.deb` and no Server will ever be handed one — the
   installers are **operator** artifacts, and nothing in the fleet path reads them. The release notes
   must therefore say which file is for which purpose, because a release that offers seven Linux
   files without saying so is worse than one that offers two.

10. **The package delivers; `service install` installs.** Each native package places exactly one
    file — the release binary, the same bytes the sibling archive carries — and then invokes the
    Client's own install:

    | | delivered to | post-install runs | pre-removal runs |
    |---|---|---|---|
    | `.deb` / `.rpm` | `/usr/libexec/<PRODUCT_NAME>/supervisor` (clause 16) | `/usr/libexec/<PRODUCT_NAME>/supervisor service install`, then lays the `PATH` symlink (clause 16) | `service stop`, then `service uninstall` |
    | `.msi` | `INSTALLFOLDER\supervisor.exe` | `… service install --endpoint <ENDPOINT>` | `service stop`, then `service uninstall` |

    No package ships a systemd unit, a `LaunchDaemon`, or an MSI `ServiceInstall` element.
    `cargo-deb`'s `systemd-units` integration and WiX's `ServiceInstall`/`ServiceControl` elements
    are **deliberately unused**: each would write a second registration that knows nothing of
    ADR-0010 clause 11's service name, ADR-0017's recovery actions, or ADR-0010's `current` pointer.
    There is one install path on four operating systems, and it stays the one that already exists.

    The two ownerships stay disjoint by construction. `dpkg` owns the delivered payload and never
    anything under the layout or data root; `service install` stages a *copy* into
    `<layout root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/` (ADR-0022 clause 9), which is what
    the service actually runs and what a self-update replaces. A fleet update therefore never touches
    a package-owned file, and `dpkg -V` stays quiet. The delivered binary and the running binary drift
    apart after a self-update — that is correct, and the Consequences record it as the cost it is.

11. **The Linux packages register the service and do not start it.** `postinst` / `%post` runs
    `service install` and stops. It does not `systemctl enable --now`.

    This breaks with Debian Policy's rule on init scripts and services, and with what
    `dh_installsystemd` does by default, and the reason is clause 7's: `service install` on a host
    with no configuration succeeds, and the service it would start dials the development default.
    Starting it would mean every `apt install` of this package produces the exact silent half-state
    this decision exists to eliminate — and produces it at fleet scale, where nobody is watching a
    terminal. A registered, stopped service is honest: the host has the Client, and it is not yet
    managed.

    Clause 7 already makes `service install` warn, naming the path that will be baked into the unit,
    so the operator gets the right message without the package printing one of its own. The two
    remaining steps are the same on every distribution:

    ```console
    sudo apt install ./supervisor_1.2.3_linux_amd64.deb
    sudo opamp-fleet service install --endpoint wss://fleet.example.com/v1/opamp
    sudo systemctl start opamp-fleet
    ```

    The second command is a re-install, which ADR-0010 already requires to be idempotent, and it
    writes the configuration clause 2 refuses to overwrite once it exists. An operator who prefers to
    be asked runs `service install --interactive` instead.

12. **One naming rule for the whole release.** Every asset is named `<name>_<version>_<os>_<arch>.<ext>`
    — ADR-0019's four `_`-separated fields, with ADR-0021's platform vocabulary, extended over the
    native extensions rather than beside them. All four artifacts of a target share the name and
    differ in extension alone; the name is `supervisor` (ADR-0022 clause 8):

    | | artifact |
    |---|---|
    | archive | `supervisor_1.2.3_linux_amd64.tar.gz` |
    | Debian | `supervisor_1.2.3_linux_amd64.deb` |
    | RPM | `supervisor_1.2.3_linux_amd64.rpm` |
    | Windows | `supervisor_1.2.3_windows_amd64.msi` |

    …and the same four for `arm64`, minus the MSI (ADR-0019 keeps Windows on arm64 out; it is one row
    when a deployment asks for it).

    **The file name is not what a package manager reads.** `rpm` and `dpkg` resolve architecture from
    the metadata *inside* the package, and there each ecosystem keeps its own vocabulary — the `.rpm`
    records `x86_64` / `aarch64`, the `.deb` records `amd64` / `arm64`, both derived by the packaging
    tool from the Rust target triple. Only the name is uniform, and the name is the thing an operator
    globs and a release page sorts. `otelcol_0.158.0_linux_amd64.deb` is exactly this shape, from the
    project ADR-0019 took its naming from.

    The `<version>` is the base version, as everywhere else (ADR-0019 clause 4,
    [ADR-0009](0009-version-from-cargo-toml-and-git.md)). It needs no epoch
    and no `~` pre-release mangling, because [ADR-0009](0009-version-from-cargo-toml-and-git.md)'s pipeline
    refuses to release anything that is not `MAJOR.MINOR.PATCH`. The RPM `Release` field is `1` and
    stays `1`: a version is released once (ADR-0009), so there is never a second build of one.

13. **The MSI asks for the install folder and the endpoint, and both are ordinary public
    properties.** The UI is the stock **`WixUI_InstallDir`** dialog set with one dialog inserted
    before the directory page:

    | property | dialog | used as |
    |---|---|---|
    | `INSTALLFOLDER` | `InstallDirDlg` (stock), selected by setting `WIXUI_INSTALLDIR` | the directory the payload is delivered to |
    | `ENDPOINT` | one inserted dialog with an `Edit` control | `service install --endpoint` |

    `INSTALLFOLDER` is **one directory**, and the operator configures one path, not half of one. It
    holds the delivered payload only; the layout and the state directory are built by `service
    install` under `%ProgramData%\<PRODUCT_NAME>`, and the MSI passes no root flag (ADR-0010
    clause 9). The endpoint dialog also carries the self-update consent as a checkbox and the public
    `SELFUPDATE` property (ADR-0017).

    Both names are **all uppercase**, which is not cosmetic: Windows Installer resets private
    properties when execution crosses from the UI sequence to the execute sequence, so a value typed
    in a dialog reaches a custom action only if the property is public. Both are additionally listed
    in `SecureCustomProperties`, which makes the identical MSI an unattended one — the same answers on
    the command line, which is how Intune and SCCM will actually deploy it:

    ```console
    msiexec /i supervisor_1.2.3_windows_amd64.msi /qn ^
      INSTALLFOLDER="C:\Program Files\opamp-fleet" ^
      ENDPOINT="wss://fleet.example.com/v1/opamp"
    ```

    The registration runs as a **deferred custom action with `Impersonate="no"`**, because it writes
    under `Program Files` and registers a service, and neither is something the invoking user is
    guaranteed to be allowed to do. It is a type 18 action — an executable installed by this package
    — whose command line "commonly contains properties that are designated dynamically": the `Target`
    field is a formatted string resolved when the installation script is written, so the endpoint
    reaches it without a `CustomActionData` round trip. It is sequenced after `InstallFiles`, which
    that action type requires, since the executable it runs is the one being installed.

    An `ENDPOINT` left empty is allowed and means "no `--endpoint`": the install then behaves exactly
    as a Linux one, warning and leaving the file to be written later. That is two custom actions under
    opposite conditions rather than one with an empty flag, because `--endpoint ""` would be
    *rejected* by the loader's endpoint rule while no flag at all is the ordinary
    deferred-configuration install.

    The sequence is `WelcomeDlg → InstallDirDlg → EndpointDlg → VerifyReadyDlg`: **no licence page.**
    Apache-2.0 requires no click-through, and `LicenseAgreementDlg` wants the licence as RTF — a
    second copy of `LICENSE` in a format nothing else here uses and that would have to be kept in
    sync. Dropping it is the worked example in WiX's own customization guide. The set is a *copy* of
    the toolset's `WixUI_InstallDir` fragment with the navigation re-pointed, which is what that guide
    prescribes; overriding a stock set's rows in place would depend on control-event ordering the
    toolset is free to renumber, and a wrong guess there is a wizard that silently skips a page.

    A second installation on one host is a second build, with its own `UpgradeCode` (ADR-0010
    clause 16).

    **The `UpgradeCode` is a GUID minted once and never changed again.** It is the identity by which
    Windows Installer recognises version 1.2.4 as an upgrade of 1.2.3 rather than a second product
    installed beside it; changing it later strands every host that already has the old one. It is
    therefore a constant in the WiX source with a comment saying exactly that, paired with
    `MajorUpgrade` so a new version removes the old. The `ProductCode` is regenerated per version,
    which is what `MajorUpgrade` requires.

14. **What builds them, and where.**

    | | tool | version | licence | runs on |
    |---|---|---|---|---|
    | `.deb` | `cargo-deb` | 3.7.0 (2026-05-02) | MIT | the two Linux runners |
    | `.rpm` | `cargo-generate-rpm` | 0.21.0 (2026-05-04) | MIT | the two Linux runners |
    | `.msi` | WiX Toolset, as the `wix` .NET tool | 6.x | MS-RL | the Windows runner |

    All three are **additional steps in the existing `build` matrix job**, after the release binary is
    built and the archive is packed, and all three consume the binary that job already produced —
    `cargo deb --no-build`, `cargo generate-rpm` pointed at the same target directory, and a WiX
    source that harvests one file. Nothing is compiled twice, so the bytes in the `.deb` are provably
    the bytes in the archive. The arm64 Linux runner packages arm64 natively; no cross-packaging is
    needed.

    `cargo-generate-rpm` builds the RPM through the `rpm` crate and needs no `rpmbuild` on the
    runner, which is why the RPM can be produced on an Ubuntu runner at all. `cargo-deb` derives
    `Depends` from the binary's actual shared-library needs (`$auto`), so the glibc floor in the
    package is the one the build really has rather than one somebody typed.

    Package metadata lives in `crates/client/Cargo.toml` under `[package.metadata.deb]` and
    `[package.metadata.generate-rpm]` — read from the workspace's own `version`, `license` and
    `description` rather than restated. The WiX sources live in `packaging/windows/`.

    The `SHA256SUMS` file covers the native assets too, and a `workflow_dispatch` dry run builds and
    packs all of them and publishes nothing (ADR-0019 clause 1).

15. **`service install` takes `--endpoint`.** A non-interactive flag on `service install`: it writes
    the same first configuration `--interactive` writes — through the same renderer and its
    `write_new`, so the file's shape, its `0600` mode, and the never-overwrite rule are unchanged —
    with the endpoint **given** rather than asked. It is validated by the loader's own rule, the same
    one the questionnaire uses. It is mutually exclusive with `--interactive`; with neither flag,
    `install` writes no configuration and warns (clause 7).

    The MSI does not write TOML. The questionnaire is already a second place that has to follow the
    configuration schema; a WiX custom action emitting TOML would be a third, written in a language
    nobody here tests. One flag on the command that already owns the file is the smaller thing.

    `--endpoint` deliberately does **not** grow siblings for the credential or the CA file. Clause 8
    puts the credential behind a hidden prompt precisely so it never lands in a process list or a
    shell history, and an MSI property is written to `%WINDIR%\Installer` logs. The endpoint alone
    gets a fresh host to the point where it is visibly aimed at the right Server, which is the half a
    packaged install can honestly automate. The self-update flags beside `--endpoint` are
    ADR-0017's.

### The packaged CLI and package removal

16. **The CLI on `PATH` is a symlink through `current`.** The `.deb` and `.rpm` deliver the payload
    to **`/usr/libexec/<PRODUCT_NAME>/supervisor`** (off `PATH`; FHS 3.0 sanctions `/usr/libexec` for
    internal binaries), and **`/usr/bin/<PRODUCT_NAME>` is a symlink** to the default system layout's
    `current` binary — `/opt/<PRODUCT_NAME>/current/supervisor` (ADR-0010 clauses 9 and 11) — never to
    a delivered version binary. The maintainer scriptlets maintain it:

    | hook | runs | does |
    |---|---|---|
    | `postinst` / `%post` | after files land | `/usr/libexec/… service install`, then `ln -sfn` the symlink |
    | `%posttrans` (rpm only) | end of the transaction | `ln -sfn` again — see clause 17 |
    | `postrm` / `%postun` | after a real removal (never an upgrade) | remove the symlink (only if it is one) and the staged versions with `current`; on dpkg **purge**, the data root too — see clause 20 |

17. **rpm re-lays the symlink in `%posttrans`.** rpm's upgrade ordering erases the *old* package's
    files (a regular file an earlier release delivered at the `/usr/bin` path) **after** the new
    package's `%post` has run — deleting the symlink `%post` just created. The transaction scriptlet
    runs after that erasure, so the link it lays is the one that survives. On dpkg the obsolete file
    is removed during unpack, before `postinst configure`, so no equivalent is needed.

18. **Staging is rewrite-free when the bytes are identical.** `service install` invoked through the
    symlink *runs from* `<layout root>/versions/…/supervisor`; re-staging would write over the very
    file it executes, which Linux refuses (`ETXTBSY`). `stage_current_exe` compares hashes and skips
    the write when the staged binary already holds the running bytes — an idempotent re-install stays
    idempotent, and the documented post-install step (`opamp-fleet service install --endpoint …`)
    keeps working when it arrives through the link.

19. **The maintainer scripts call the payload at its `/usr/libexec` path, never the symlink:** it is
    the file the package guarantees to exist at that moment.

20. **A real package removal also uninstalls every staged version.** `service uninstall` itself keeps
    deleting nothing — ADR-0010's rule protects the manual flows, where the layout is the operator's.
    A removal that only *looks* complete leaves the layout behind, and the next install comes up on
    the surviving `current` pointer running a version the operator believed uninstalled. So the
    package's `postrm` finishes what a removal means on a package-managed host, across the two roots
    of ADR-0010 clause 9:

    - **remove** deletes the layout root `/opt/<PRODUCT_NAME>` — every staged version and the
      `current` pointer; it holds nothing else. The data root `/var/lib/<PRODUCT_NAME>`, with its
      state and `supervisor.toml`, stays — an instance identity and a credential the operator typed
      are not binaries, and a reinstall picks them back up (a stale `installed-package.json` is
      discarded at startup when it does not name the running release).
    - **purge** (dpkg only; rpm has no equivalent) additionally deletes the data root whole — state,
      logs and configuration included. Purge is dpkg's word for "leave nothing".

21. **The packaged symlink and the removal both target the default system roots** — the only roots
    a packaged install ever uses, because `postinst` calls plain `service install`. An operator who
    chooses `--root` is doing a manual install (archive), where no package writes to `/usr/bin` or
    deletes a layout at all. A variant build is a packaged install under its own product name, with
    its own payload directory, symlink and roots (ADR-0010 clause 16).

### The MSI's endpoint prefill

22. **The MSI prefills `ENDPOINT` with the development Server in its HTTP form,
    `http://localhost:4320/v1/opamp`, in the UI sequence only** (a `SetProperty` with
    `Sequence="ui"`, conditioned on the property being unset and the product not yet installed). The
    thousand-hosts case never sees the dialog: fleet deployments run `msiexec /qn` through Intune,
    Group Policy or SCCM, where the endpoint arrives as `ENDPOINT=` on the command line. The person the
    dialog *does* face is evaluating or developing against a local Server, and the one value they
    would type is the development default the Client already knows.

23. **A silent install that names no `ENDPOINT` writes no configuration**, warns and defers —
    unattended fleet deployment cannot acquire the development default by omission.

24. **A value given on the `msiexec` command line wins over the prefill**; clearing the field remains
    the "configure later" answer.

25. **The prefilled value names the same host, port and path as the loader's `default_endpoint()`,
    but with the `http://` scheme** — the scheme selects the transport (ADR-0008), and HTTP polling
    is the operator's explicit choice for the click-through install.
    `crates/client/tests/msi_exe_command.rs` holds the value to the loader's endpoint rule and to
    this string.

## Alternatives considered

- **A separate `config init` command** — a second entry point for work the operator is already
  in the middle of when they run `install`, and a second place where the config path must be
  resolved. Rejected for the flag; the cost is recorded under Consequences.
- **Interactive by default, with `--non-interactive` to opt out** (Elastic Agent's shape) — rejected
  because our `install` is *already* the scripted command. Elastic can afford that default because
  their install is the documented first contact with the product; flipping ours would hang every
  automated invocation.
- **Overwrite an existing file, with `--force` to confirm** (what Elastic does to
  `elastic-agent.yml`) — rejected: a re-install that discards a credential the operator typed is a
  worse failure than one that refuses to write.
- **Plain `stdin().read_line()`, no dependency at all** — the simplest thing, and it echoes the
  bearer token onto the screen and into the scrollback. Rejected on that point alone.
- **`inquire` 0.9.4 (2026-02-24)** — actively maintained and richer (editor prompts, derive macros
  for enum menus), which is more than four questions need, at roughly a third of `dialoguer`'s
  adoption. **`cliclack`** — a styled multi-step experience we do not need. Both remain viable if
  `dialoguer` ever stops being maintained; nothing outside the questionnaire module would change.
- **Ship `config/supervisor.toml` inside the release artifact** — rejected: ADR-0019 keeps the asset
  installable as a package artifact, and an example copied onto the host still has to be edited
  before the service does anything, which is the actual problem.
- **Environment variables for the first configuration** — already refused by ADR-0008 and ADR-0010;
  nothing here reopens it.
- **The package owns the whole install** — the binary in `/usr/bin`, a shipped systemd unit under
  `/lib/systemd/system/`, a configuration under `/etc`, `%ProgramFiles%` plus an MSI
  `ServiceInstall`. The conventional distro shape, and the one most operators would predict.
  Rejected on two counts, either sufficient: it forks the install path per platform, which ADR-0010
  exists to avoid and which would leave `service install` as a fourth, differently-behaving variant;
  and it puts ADR-0017's self-update in direct conflict with the package manager over the same
  files. A packaged product whose fleet updates are reverted by `apt upgrade` is worse than one that
  ships no package.
- **`nfpm`** — one tool and one YAML file for `.deb` and `.rpm` (and `.apk`), and what
  `opentelemetry-collector-releases` uses via goreleaser. A genuinely good tool and a close call. Not
  chosen because it is a second ecosystem's binary to install and pin in CI, and because its config
  restates the version, licence and description that the two cargo subcommands read straight out of
  `Cargo.toml` — a duplicate of exactly the kind ADR-0009 removed. Worth reconsidering the moment a
  third format (`.apk`, Arch) is wanted, where one config beats three.
- **`cargo-wix`** — the in-ecosystem choice, and it does drive WiX from `Cargo.toml`. Rejected: its
  value is generating a `.wxs` and shelling out to the toolset, and this MSI needs a custom dialog, a
  custom action and public properties, so the `.wxs` is hand-written either way. Its release 0.3.9
  (2025-03-13) also predates WiX v6 and defaults to the legacy v3 toolset. Calling `wix build` on our
  own source is one layer fewer.
- **A WiX Burn bundle (`.exe`) instead of a plain `.msi`** — richer UI, can chain prerequisites.
  Rejected: the ask was an MSI, and MSI is what Intune, Group Policy and SCCM ingest; a Burn bundle
  is awkward in all three.
- **Traditional per-ecosystem file names** — `<name>-1.2.3-1.x86_64.rpm`, the shape a RHEL
  administrator expects. Rejected for one naming rule across the release: nothing resolves an RPM by
  its file name, ADR-0019 gave this project a rule and a reason, and the prior art ADR-0019 cites
  publishes `_linux_amd64.rpm`. The ecosystem vocabulary survives where it is load-bearing — inside
  the package metadata.
- **Preseed the endpoint on Linux too** (debconf on Debian, an RPM macro or a config file read by
  `%post` on RHEL) — symmetry with the MSI, and rejected for now: two more preseed mechanisms to
  write, document and test, when `service install --endpoint` already works unattended on both and
  is one line in the Ansible play that installed the package. Recorded as a follow-up, not as a gap.
- **A macOS `.pkg`** — the same argument would justify it, and it is genuinely out of scope here: it
  needs a Developer ID, notarization, and a stapling step, which is the signing decision ADR-0019
  deferred for the archives. Naming it as a follow-up is honest; bolting an unsigned `.pkg` onto
  this would not be.
- **Replace the archives with the installers** — refused. The archive is the only format the Client
  can open, so dropping it removes the fleet's ability to self-update (ADR-0017, ADR-0019 clause 3).
  The installers are additive.
- **Publish to an apt/yum repository instead of attaching files to a release** — the better long-term
  answer for `apt upgrade` to mean anything, and a hosting and signing decision of its own (which
  key, which host, which retention). The files have to exist before a repository can carry them; this
  is that step.
- **Ship the symlink as a packaged file instead of creating it in scriptlets.** `cargo-deb` can
  (asset tables with `preserve-symlinks`); `cargo-generate-rpm` does not document symlink assets at
  all. Two tools, two mechanisms, one of them undocumented — the scriptlets are one mechanism that
  both formats honour, and the removal guard (`only if it is a symlink`) keeps them polite.
- **A launcher shim at `/usr/bin` that execs `current`.** A second code path with its own failure
  modes (root discovery, exec error reporting), permanently delivered, to do what one symlink does.
- **Teach `service install` to write `/usr/bin` itself.** It knows the root, but a user-scope or
  custom-root install must not touch `/usr/bin`, and writing into the package manager's directory
  from the application crosses exactly the ownership boundary clause 10 draws. The scriptlets are
  the package's side of the line; the layout stays the Client's.
- **Documentation only.** A warning in the manual does not stop an operator from trusting the first
  diagnostic command. That command has to answer for the running service, not for a footnote.
- **Keep the MSI's endpoint field empty** — safest, but the interactive install then asks a question
  whose most common answer for the audience actually facing the dialog is a string the product
  already knows; explicitly requested away by the operator.
- **A static `Property` default for `ENDPOINT` (both sequences)** — one line, but it changes
  silent-install semantics: `/qn` without `ENDPOINT=` would pin every unattended host to localhost,
  precisely the state this decision refuses to manufacture.
- **Prefill the loader's `default_endpoint()` (`ws://127.0.0.1:4320/v1/opamp`) verbatim** — one
  value defined once, and the transport an unconfigured Client picks anyway. Rejected by the
  operator in favour of the `http://` form; the divergence (two development defaults, a scheme
  apart) is accepted and pinned by test rather than left to drift.

## Sources / Prior art

- [Elastic Agent command reference](https://www.elastic.co/docs/reference/fleet/agent-command-reference)
  — `install` prompts for confirmation and enrollment, `--non-interactive` for automation, `--force`
  to overwrite `elastic-agent.yml` without prompting. The closest prior art, and the source of the
  two behaviours deliberately inverted here (default and overwrite).
- [Elastic Agent install documentation](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent)
  — ships `.deb`, `.rpm` and `.msi` as well as archives, and states that the archive distributions are
  the ones its fleet can upgrade from: the same split between an operator artifact and a fleet
  artifact clause 9 makes.
- [`dialoguer` on crates.io](https://crates.io/crates/dialoguer) — 0.12.0, published 2025-08-23, MIT.
- [`inquire` on crates.io](https://crates.io/crates/inquire) — 0.9.4, published 2026-02-24.
- [Comparison of Rust CLI prompts: cliclack, dialoguer, promptly, inquire](https://fadeevab.com/comparison-of-rust-cli-prompts/)
  — the field, side by side.
- [`std::io::IsTerminal`](https://doc.rust-lang.org/stable/std/io/trait.IsTerminal.html) — in `std`
  since 1.70; no `atty`/`is-terminal` dependency is needed for the TTY guard.
- [Command Line Applications in Rust — Communicating with machines](https://rust-cli.github.io/book/in-depth/machine-communication.html)
  and [Improving CLIs with isatty](https://blog.jez.io/cli-tty/) — the convention that a program
  behaves differently, and predictably, when it is not talking to a person.
- The OpenTelemetry `opampsupervisor`, whose configuration is hand-written and which offers no
  scaffolding command — the same absence of an upstream answer ADR-0017 found for self-update.
- [`opentelemetry-collector-releases`](https://github.com/open-telemetry/opentelemetry-collector-releases)
  — publishes `.deb`, `.rpm` and `.msi` beside its archives, and names them
  `otelcol_<version>_linux_<arch>.deb`. ADR-0019 took its archive naming from here; clause 12 takes
  the extension of that rule to native packages from the same place.
- [Install the Collector on Linux](https://opentelemetry.io/docs/collector/install/binary/linux/) —
  the operator-facing shape of that release: one command per distribution family.
- [`cargo-deb`](https://crates.io/crates/cargo-deb) — 3.7.0, 2026-05-02, MIT; its
  [systemd integration notes](https://github.com/kornelski/cargo-deb/blob/main/systemd.md), which
  document the `systemd-units` + `maintainer-scripts` + `#DEBHELPER#` mechanism clause 10
  deliberately declines; and its [repository](https://github.com/kornelski/cargo-deb) —
  maintainer-scripts pickup (`preinst`/`postinst`/`prerm`/`postrm`), symlink asset support.
- [`cargo-generate-rpm`](https://crates.io/crates/cargo-generate-rpm) — 0.21.0, 2026-05-04, MIT;
  builds through the `rpm` crate, so no `rpmbuild` is needed on the runner. Its
  [repository](https://github.com/cat-in-136/cargo-generate-rpm) documents the scriptlet options
  (`post_install_script`, `post_uninstall_script`, `post_trans_script`); symlink assets are
  undocumented.
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
  letters. The reason both property names in clause 13 are uppercase.
- [Custom Action Type 18](https://learn.microsoft.com/en-us/windows/win32/msi/custom-action-type-18)
  — an executable installed by the package, whose command line "commonly contains properties that
  are designated dynamically", and which "must be sequenced after the InstallFiles action" when the
  executable is the one being installed. Both are why clause 13's action looks the way it does.
- [Deferred execution custom actions](https://learn.microsoft.com/en-us/windows/win32/msi/deferred-execution-custom-actions)
  — why the registration is deferred rather than immediate: deferred actions are "the only types of
  actions that can run outside the users security context", which is what registering a service
  under `Program Files` needs.
- [Customizing built-in WixUI dialog sets](https://docs.firegiant.com/wix3/wixui/wixui_customizations/)
  — copy the dialog set's fragment and re-point its `Publish` navigation, rather than overriding a
  stock set's rows in place; also the source of the worked example that removes the licence page.
- Windows Installer: type 51 (set-property) custom actions scheduled in `InstallUISequence` do not
  run under `/qn`; `Secure` public properties cross the UAC elevation boundary — the standard
  mechanism for UI-only defaults (clause 22).
- [Debian Policy, system run levels and `init.d` scripts](https://www.debian.org/doc/debian-policy/ch-opersys.html#system-run-levels-and-init-d-scripts)
  — the enable-and-start convention clause 11 knowingly departs from, and the reason it is stated
  rather than quietly skipped.
- [rpm scriptlet ordering](https://docs.fedoraproject.org/en-US/packaging-guidelines/Scriptlets/) —
  new `%post` runs before the old package's files are erased; `%posttrans` runs last.
- [FHS 3.0 `/usr/libexec`](https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch04s07.html) —
  binaries run by other programs rather than by users, off `PATH`.
- The `alternatives`-style indirection every Debian-managed toolchain uses: what users invoke on
  `PATH` is a link that is repointed, never the binary that moves.
- **This project's ADR-0008, ADR-0010, ADR-0017, ADR-0019, ADR-0021, ADR-0019, ADR-0022 and
  ADR-0010** — scheme selects transport, the install layout, the self-update this must not collide
  with, the artifact set this extends, the platform vocabulary, the naming grammar, the program's and
  the artifacts' names, and the product name with its paths and service.

## Consequences

- **Positive:** a fresh host goes from a downloaded binary to a working, registered service with one
  command, identically on Ubuntu, RHEL, macOS, and Windows. The default-endpoint service that looks
  installed and manages nothing stops being reachable by accident. The credential is typed into a
  hidden prompt instead of a command-line flag, so it never lands in shell history or a process list.
- **Positive:** the documented first contact becomes one command per platform — `apt install`,
  `dnf install`, double-click — instead of unpack, install, start. On Windows the operator is asked
  what a fresh host cannot guess, in a dialog, and the same MSI deploys unattended through Intune
  with the same answers on the command line. The local-evaluation install is a click-through, and the
  interactive and unattended paths keep their distinct semantics.
- **Positive:** removal becomes real. `apt remove` and Add/Remove Programs stop the service,
  unregister it, and take the binary; on Linux the removal takes every staged version too, so a
  package removal followed by an install of an older release comes up on that older release — no
  surviving `current` pointer outranking the operator's decision.
- **Positive:** there is one install path on four operating systems and one place where a bug in it
  can be fixed. The packages are wrappers, not a second implementation.
- **Positive:** a self-update never fights the package manager. The package owns a payload off
  `PATH` and a constant link; the layout owns everything the link resolves to; `dpkg -V` and
  `rpm -V` stay quiet through every fleet update.
- **Positive:** `opamp-fleet --version` — and every other CLI invocation on `PATH` — answers for the
  binary the service actually runs on Linux, in both drift directions (fleet updated the host, or an
  operator hand-reinstalled an older package).
- **Negative / trade-offs:** a host whose configuration lives anywhere but the default path must name
  it with `--config`. One new dependency (`dialoguer`) and the terminal handling it brings.
  A configuration cannot be generated without installing — an operator who wants only the file must
  write it by hand or install and then uninstall. The questionnaire is a second place that has to
  follow the configuration schema, bounded to the handful of keys it asks about. `--interactive` is
  unavailable where it would be most tempting and least appropriate — container builds, image
  bakery, unattended provisioning — which is the point, but it means those paths still ship a config
  file by their own means.
- **Negative / trade-offs:** the delivered binary and the running binary diverge after the first fleet
  self-update. That is the price of the disjoint ownership, and it means `dpkg -l` reports the
  *delivered* version, not the running one — inherent to a package manager. `opamp-fleet --version`
  and the fleet view remain the truth, and the manual must say so; an operator who reads `dpkg -l`
  and concludes the update failed has been misled.
- **Negative / trade-offs:** until `service install` has run once (which `postinst` does), the symlink
  dangles; a broken-by-hand layout makes the CLI fail loudly rather than answer for the wrong binary,
  which is the better failure. The symlink assumes the default system roots; a packaged install
  combined with a hand-moved root leaves a dangling link, a combination that is unsupported.
- **Negative / trade-offs:** `apt remove && apt install` does not preserve the running version across
  the gap — the reinstalled package's own binary is what comes up, staged fresh. That is the point.
- **Negative / trade-offs:** `apt install` leaves a stopped service, which is not what a Debian user
  expects and will be reported as a bug until the release notes say why. Clause 11 accepts that in
  exchange for never manufacturing a Client pointed at `127.0.0.1`.
- **Negative / trade-offs:** the Windows install is configurable in a way the Linux one is not. The
  asymmetry is real, bounded (both platforms have `--endpoint`), and recorded as a follow-up.
- **Negative / trade-offs:** an operator interactively installing the MSI on a production host and
  clicking through without reading gets a `supervisor.toml` pinned to localhost rather than a warning
  — the narrow slice of the thousand-hosts concern clause 22 consciously accepts. And a
  click-through install runs HTTP polling where an unconfigured Client would have chosen WebSocket —
  two development defaults, a scheme apart.
- **Negative / trade-offs:** three more build tools in the pipeline, one of them a .NET tool, and
  three more ways for a release to fail — the same cost ADR-0019 accepted for its targets. Nothing is
  signed: the `.deb`, `.rpm` and `.msi` are unsigned exactly as the archives are, so Windows shows an
  unknown-publisher prompt and `rpm` reports no signature. This is the same deferred signing
  decision, with a second reason to make it.
- **Negative / trade-offs:** a permanent `UpgradeCode` GUID is a decision that cannot be revisited
  without stranding installed hosts.
- **Follow-ups:** whether `uninstall` should offer to remove a configuration file it wrote (it
  deletes nothing, deliberately, and that asymmetry is visible); whether the same questionnaire
  should back a later `config validate` / `config show`. Signing — an Authenticode certificate for
  the MSI and a GPG key for the RPM, which is the archive-signing decision ADR-0019 deferred, covering
  three more artifacts. Hosting an apt and a yum repository, so `apt upgrade` reaches this product at
  all. Preseeding the endpoint on Linux the way the MSI does on Windows. A macOS `.pkg`, which waits
  on notarization. Whether the Server binary deserves the same treatment — ADR-0019 asked the same
  question and gave the same answer, that it is deployed by an operator rather than by the fleet.
  Windows has the same CLI drift (`INSTALLFOLDER\supervisor.exe` is the delivered binary and nothing
  on `PATH` goes through `current`); whether the MSI should lay a junction-based equivalent is its own
  decision. macOS remains a manual install.
