# ADR-0010: The Client is an OS service the product names — clap subcommand CLI, one build-time name, a versioned install layout, one account

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

## Context

The specification commits the Client to run as first-class infrastructure: it "installs as a native
operating-system service that updates itself in place, and runs on Linux, macOS, and Windows"
(Mission; Strategy), and **Goal #11** requires exactly that — install and run as a native OS service
on all three platforms, with an in-place self-update that survives the service restart. A service
manager's working directory is `/` or `System32`, so configuration and state paths relative to the
working directory are meaningless under it. systemd stops a service with `SIGTERM`; a process that
only handles `ctrl_c()` hits the kill timeout and skips the `agent_disconnect` goodbye.

Forces beyond the platform differences themselves:

- **The three service models genuinely differ.** systemd and launchd supervise an ordinary
  foreground process; the **Windows Service Control Manager (SCM)** launches the process and expects
  status reports over the SCM protocol within ~30 seconds or kills it with error 1053 — so "run
  under the manager" is a different code path on Windows. And a process cannot reliably detect an
  SCM launch (`StartServiceCtrlDispatcher` fails with error 1063 when *not* SCM-launched), so the
  installed command line must carry a marker.
- **No fixed installation path.** The Client must be registerable from wherever its binary lives;
  the operator may choose the install root. Nothing may hard-code `/usr/bin` or `Program Files`.
- **Self-update must stay possible** (Goals #10/#11; the mechanism is
  [ADR-0017](0017-client-self-update-and-its-consent.md)'s). What the service points at is the load-bearing choice:
  registering the raw binary path would force a re-registration on every update; registering a
  stable *pointer* makes an update a pointer switch. On Windows the running `.exe` is locked, which
  rules out overwriting in place and independently motivates a versioned side-by-side layout.
- **One thing must name an installation.** It names the install path, the service, the package and
  the `PATH` symlink, so it has to exist before the installation's configuration is read. A runtime
  instance flag cannot be that thing. No delivery path reaches it: the `.deb` and `.rpm` maintainer
  scripts and the MSI register one installation, and a second one is reachable only by unpacking an
  archive and registering it by hand. Nothing enumerates instances, so an operator must remember the
  name to stop or remove what they installed. And it does nothing at runtime: `RunSpec` carries
  `config_path`, `state_dir` and `service`, and an instance name would only select a service name and
  two default paths at install time.
- **The installation's name and the program's name are different things.** The program is
  `supervisor` and its configuration `supervisor.toml`
  ([ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) clauses 9 and 10). A path such as
  `/opt/opamp-fleet/client/default/current/supervisor` spells two names for one thing, and a level
  that only ever holds the constant `default` names nothing.
- **Platforms give different reasons to keep the executable layout apart from data.** On Linux an
  enforcing SELinux policy never lets systemd start a binary under `/var/lib`; on Windows
  `Program Files` belongs to the installer and is read-only to everything else. Clause 9 decides
  both.
- **Least privilege.** A fleet client should be able to run without root or `LocalSystem`, and
  whatever account it runs as must be able to rewrite its own executable layout, because ADR-0017's
  updater is the daemon itself.
- A CLI parser, a cross-platform service-management library, and a Windows service runtime are new
  dependencies and a new public interface surface — per `AGENTS.md` §3 that requires this ADR before
  any code. ADR-0007 constrains dependencies to the rustls/ring stack (no competing TLS/crypto
  backends); ADR-0008 fixes configuration as a hand-edited TOML file the `--config` flag points at.
  "Simplicity first / YAGNI" bounds the scope: service *lifecycle* and the *layout* — not the update
  mechanism, not per-backend unit tuning.

## Decision

We will turn the Client into a **`clap` subcommand CLI** that registers, controls, and runs *itself*
as a native OS service on Linux (systemd), macOS (launchd), and Windows (SCM), with all daemon code
isolated in one `service` module. We will fix the product's name at **build time**, collapse the
install path to a single level named after it, split program from data wherever the platform gives a
reason, and make a second installation a **second build**. The service may run under an
operator-named account that owns what it rewrites.

### The program, its CLI and the service lifecycle

1. **Subcommands:** `run` (foreground daemon — the default when no subcommand is given, so
   `supervisor --config <path>` runs the daemon) and `service install | uninstall | start | stop |
   status`. Global flags: `--config` (ADR-0008; the default file name is ADR-0022 clause 10's
   `supervisor.toml`) and `--state-dir` (override). `service uninstall | start | stop | status` take
   no name: there is one service per build (clause 11). No environment-variable configuration is
   added; the Client stays file-configured, and the installed unit carries the config *path*, not
   the config.

2. **Lifecycle** is implemented over the **`service-manager`** crate (systemd, launchd, Windows SCM
   behind one API; verified 2026-07: v0.11.0 of 2026-02 is current and actively maintained, and it
   is effectively the only maintained cross-platform lifecycle crate — comparable Rust agents such
   as Vector and Mullvad hand-roll per-platform installs, so the wrapper around it stays thin enough
   to drop to platform-specific calls where a backend falls short). The default is a
   **system-level** service (systemd system unit / launchd `LaunchDaemon` / Windows `LocalSystem`,
   or the account clause 18 names) because a fleet client must run without a logged-in user and
   start at boot; `--user` is the development opt-in. The restart policy is
   `OnFailure { delay: 5 s }` — restart after a crash, never after an explicit stop, which the
   updater relies on to swap the binary without the manager racing it back up. Backend unevenness is
   handled, not papered over: the `sc.exe` backend cannot express a restart policy, and the Windows
   recovery actions that make it real are ADR-0017 point 5's; since v0.10 `install` on launchd no
   longer auto-starts (so `install` prints the follow-up `service start` step rather than
   pretending); and launchd `status` is advisory (a known upstream bug reports running services as
   stopped).

3. **Versioned install layout, laid out by `service install`** under the layout root (clauses 8–10):
   the running executable is staged into
   `<root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/supervisor` (`supervisor.exe` on Windows;
   the program name and the directory prefix are ADR-0022 clause 9's) — Elastic Agent's directory
   naming (`elastic-agent-<version>-<hash>`) with our component name. The version part is
   ADR-0009's bare `MAJOR.MINOR.PATCH` base — **never the pre-release**; `<hash>` is the commit
   short-hash from the build metadata. The release `1.2.3` and a dev build descending from it thus
   live as `versions/supervisor-1.2.3-a1b2c3d/` and `versions/supervisor-1.2.3-b4e5f6a/`,
   distinguished by their commit alone; the **full** ADR-0009 version string (pre-release and
   metadata included) is recorded in the version directory's manifest, which is where
   release-or-dev is answered. Rebuilding the same commit maps to the same directory; staging into
   an already-present version directory replaces its contents and rewrites the manifest (an
   idempotent re-install, never a silent mix of two builds). The binary's full SHA-256 lives in the
   same manifest — the content hash the self-update verifies staged packages against. A stable
   **`current` pointer** (symlink on Unix, directory junction on Windows — junctions need no symlink
   privilege, the same reason Scoop uses one for its `current` alias) points at the active version
   *directory*, and the service's program is `<root>/current/supervisor`. Pointer switches are
   atomic where the platform allows: on Unix, create a temp symlink and `rename` it over `current` —
   never unlink-then-relink; on Windows the swap happens only while the service is stopped and must
   be idempotent and retried with backoff (antivirus scanners take transient locks on fresh
   executables). On start the daemon self-heals a torn swap: it verifies `current` resolves to the
   directory it actually runs from and repairs or reports the mismatch. `<data root>/state/` is the
   default state directory when the config does not name an absolute one. All paths are absolutized
   at install time; the installed command line is `run --service --config <abs> --state-dir <abs>`.

4. **Running under the manager** is a plain foreground process on Linux and macOS — the same `run`
   loop plus graceful `SIGTERM`/`SIGINT` shutdown via `tokio::signal` (feature already enabled),
   injected into the transports so the clean-shutdown `agent_disconnect` path fires on a service stop
   too; `SIGHUP` is explicitly ignored rather than left at its default terminate disposition
   (daemon(7) reserves it for config reload — a possible later feature, never an accidental kill).
   Shutdown completes well under launchd's 20-second `ExitTimeOut` default. On **Windows only**, a
   `cfg(windows)` runtime shim built on the **`windows-service`** crate registers the SCM control
   handler, reports `StartPending` → `Running` → `StopPending` (with wait hint) → `Stopped`, and then
   runs the identical daemon body; the `SERVICE_CONTROL_SHUTDOWN` path finishes in under ~5 seconds
   (the `WaitToKillServiceTimeout` default). The hidden `--service` marker flag — and only that flag
   — routes into the SCM dispatcher.

5. **The version in all of this is [ADR-0009](0009-version-from-cargo-toml-and-git.md)'s.** The
   `versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>` directory names, the CLI `--version` output,
   and the OpAMP `service.version` attribute all call the single `version()` helper decided there —
   how the string is computed is entirely ADR-0009's contract, this ADR only consumes it (and
   renders base and commit into directory names per the naming rule in clause 3).

6. **Module shape:** everything above lives in `crates/client/src/service/` (`mod.rs` with the narrow
   `ServiceControl` seam — `start`/`stop`/`state` — plus `runtime.rs`, `layout.rs`, `manager.rs`,
   and the Windows-only `windows.rs`), so the updater depends on the seam, not on service
   internals. Errors stay `Result<_, String>` in the crate's existing style; no `anyhow`.

The update mechanism itself (staging new versions over the wire, health gate, rollback) is
ADR-0017's; this ADR only guarantees the shape it needs. Richer unit/plist tuning (systemd
`Type=notify`, launchd throttling) is not decided here.

### The product names the installation

7. **`PRODUCT_NAME` is a build-time constant**, default **`opamp-fleet`** — the repository's own
   name — overridable with `OPAMP_FLEET_PRODUCT_NAME` for a variant build. It must be simultaneously
   a systemd unit name, a launchd label, an SCM name and a directory name, so its grammar is the
   intersection of those four: lowercase `[a-z0-9-]`, 1–32 characters, no leading or trailing `-`,
   and never a Windows reserved device name (`con`, `prn`, `aux`, `nul`, `com1`–`com9`,
   `lpt1`–`lpt9`), which would be legal by the other rules but is an invalid directory name on
   Windows. The build **fails** on a name that breaks the grammar, so an illegal name cannot reach a
   host. The same grammar governs the `[[supervisor]]` block names.

8. **The install path is `<base>/<PRODUCT_NAME>`, on every platform.** The directory is named by
   the product, never by its display name: `/opt/opamp-fleet`, `/Library/Application
   Support/opamp-fleet`, `%ProgramData%\opamp-fleet`, and — where the MSI lays its payload down —
   `C:\Program Files\opamp-fleet`. It does not ask for prose in a directory name, and `nodejs`,
   `Git` and `PowerShell` sit under `Program Files` under their own names. `OpAMP Fleet Agent` is a
   display name and keeps the three places display names belong: the Add/Remove Programs entry, the
   SCM's display column, and the installer dialog's title. A directory named for the display name
   would also be the one place a variant build could not tell itself apart, since two variants
   differ in `PRODUCT_NAME`.

9. **Program and data split wherever the platform gives a reason, and the reason differs.** On
   Linux at system scope the executable layout is `/opt/opamp-fleet` and the data root
   `/var/lib/opamp-fleet`. **Linux at system scope is the only place that splits.** macOS, Windows
   and every user scope keep one directory, because no other platform gives a reason to.

   **Linux: a binary under `/var/lib` is one SELinux never lets systemd start.**

   - Fedora and RHEL run the SELinux targeted policy in **enforcing** mode by default, and
     openSUSE Leap 16 / SLES 16 have switched from AppArmor to exactly that. This is not one
     distribution family's quirk but the default posture of the entire rpm world the `.rpm` exists
     for.
   - Files created under `/var/lib` carry the type `var_lib_t`. systemd (`init_t`) may only
     `execve` types the policy marks as service entrypoints — `bin_t`, `usr_t` and friends,
     through which a third-party service transitions into `unconfined_service_t`. `var_lib_t` is
     not such a type, for anybody: data directories are deliberately not executable by the init
     domain.
   - The failure is deferred and silent at install time. The package installs, `service install`
     stages the layout and registers the unit, `systemctl enable` succeeds — and the first
     `systemctl start` dies with `status=203/EXEC` (Permission denied), an AVC denial in the audit
     log the unit's own journal never explains. The same binary runs fine from an interactive shell
     (`unconfined_t` may execute nearly anything), which makes the diagnosis actively misleading.
   - **Why `/opt` is the answer and not a label fix.** Its default file context is `usr_t`, an
     entrypoint type through which systemd transitions a third-party service into
     `unconfined_service_t` — the mechanism the targeted policy provides precisely so vendor
     software outside the distribution's packages can run enforcing. Files the self-update
     (ADR-0017) stages later inherit the directory's label, so staging keeps working with no
     SELinux tooling, no policy module and no new runtime dependency. This is load-bearing: the
     layout is rewritten at runtime by the *daemon*, so any fix that is applied once at install
     time is a fix that expires at the next staged version or the next filesystem relabel.
   - FHS 3.0 sanctions the shape rather than merely tolerating it: `/opt` is for add-on
     application software (§3.13) and an add-on package's variable data belongs under `/var`. The
     split is the standard-conformant layout, not a compromise, and it is the field-proven one —
     Elastic Agent, whose versioned-directory scheme clause 3 adopts, installs to
     `/opt/Elastic/Agent`.
   - **The blind spot that hid this is still on record.** The service smoke test excludes "hosts
     with SELinux or AppArmor in the way" from coverage (`crates/client/tests/service_smoke.rs`),
     which is why no automated check ever met an enforcing host. This decision does not close that
     gap and must not be read as having closed it.

   **Windows does not split, and `Program Files` is the MSI's payload directory — not the
   layout.** This is the same line ADR-0020 drew on Linux, applied to the platform
   that has the identical shape under different names. There, the `.deb` and `.rpm` deliver one
   file to `/usr/libexec/<PRODUCT_NAME>` — package-manager-owned, never rewritten — while the
   versioned layout under `/opt` belongs to the program. Here, the MSI delivers its payload to
   `C:\Program Files\<PRODUCT_NAME>` — `TrustedInstaller`-owned, meant to be read-only once
   installation finishes — and then runs the same `service install` the command line would, which
   builds the layout and the state directory under `%ProgramData%\<PRODUCT_NAME>`.

   **Putting the layout in `Program Files` is refused**, for the reason the `/usr/lib` and
   `/usr/libexec` layouts are refused on Linux (see Alternatives): the layout is rewritten at
   runtime by the daemon, and the installer's hierarchy is the installer's. Clause 18 makes the cost
   concrete — the self-update means the service's own account must be able to write `versions/` and
   `current`, so a layout in `Program Files` would require granting a low-privileged service account
   modify rights on a tree it also executes from. That is a privilege-escalation surface offered in
   exchange for a directory listing, and this decision declines it.

   **What this settles.** A Windows host installed by the MSI and one unpacked from the archive put
   the same things in the same places. And `service uninstall` deliberately leaves the root and the
   state directory behind — deleting a `supervisor.toml` holding a credential an operator typed
   would be the overwrite ADR-0020 refused, one step later — while the MSI's uninstaller empties
   `INSTALLFOLDER`. With `INSTALLFOLDER` holding nothing but the delivered payload, emptying it is
   exactly right, and the credential sits in `%ProgramData%` where Windows expects data to survive:
   the same remove-versus-purge shape the `.deb` already has.

   **This requires a second flag, on one platform.** `--root` names everything under the one
   directory the operator names, whose labelling and permissions are then the operator's business,
   documented in the manual — and `--data-root` names the other. It exists for the Linux
   system-scope split and for an operator who wants the two halves apart anywhere; the MSI does not
   pass it, because Windows does not split. **ADR-0020 clause 13 is therefore left intact**:
   `INSTALLFOLDER` stays "one directory for everything … the operator configures one path, not half
   of one", and that one directory holds the payload, not the layout.

   **What a package removal takes, in the split layout.** ADR-0020's remove/purge distinction is
   drawn across the two roots: `postrm` on **remove** deletes the layout root — it holds nothing but
   staged binaries — and on **purge** additionally the data root. ADR-0020's mechanism is
   untouched; only the paths it names are the ones this decision gives.

10. **`--root` still overrides, and is still never a fixed path.** Given alone it collapses layout
   and data into the one directory named.

11. **The service is `PRODUCT_NAME`, with no suffix.** It is a single-token `ServiceLabel`, so
   systemd, launchd and the SCM render the same string. The service is not the program's name with
   an instance suffixed: there is no instance to suffix (clause 13), and with one installation per
   build, the name that identifies an installation is the product's, not the program's. The binary,
   the version directories, the archive member, the `/usr/libexec` payload, the log file, the
   self-check token and the CLI's own name all remain `supervisor` (ADR-0022 clause 9). Only the
   `PATH` symlink carries the product name, because two variants would otherwise collide on one
   `/usr/bin` entry.

12. **The display name is `OpAMP Fleet Agent`**, set from a second build-time variable
   (`OPAMP_FLEET_PRODUCT_DISPLAY_NAME`): it is prose, not a slug, and cannot be derived from
   `opamp-fleet` by any rule that would still read correctly for the next variant. The default is
   not `opamp-fleet`, for the reasoning of ADR-0022 clause 2: a default equal to the Agent type
   would print the same word in both columns of the fleet view, which is the collapse
   [ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) ended.

13. **There is no `--instance`.** Not hidden, not accepted-and-ignored: there is no instance flag, no
   per-instance root, no per-instance state directory, and no instance argument in a registered
   unit. `service uninstall|start|stop|status` have no lookup key — with one service name per build
   there is nothing to look up. The instance-name grammar itself **survives** as clause 7's
   grammar, because `[[supervisor]]` block names still need it.

14. **An upgrade on a managed host is a re-registration, not a migration.** `--config` and
   `--state-dir` keep pointing into `/var/lib/<PRODUCT_NAME>`, only `ExecStart` changes, and
   identity, credential and state never move. The state directory holds the instance UID and the
   configuration; moving it makes every host a *new* Agent in the fleet view and loses the
   credential an operator typed. A later decision that would move the roots inherits the full weight
   of this rule: it cannot rest on there being nothing to migrate.

15. **The program and the Agent type keep their own names.** The binary is `supervisor`
   (`supervisor.exe` on Windows) and its configuration `supervisor.toml`, exactly as ADR-0022
   clauses 9 and 10 decided, and neither is derived from `PRODUCT_NAME`. Three names sit side by
   side, each naming a different thing:

   | name | names | appears in |
   |---|---|---|
   | `opamp-fleet` | the **product** | the path, the service, the package, the `PATH` symlink |
   | `supervisor` | the **program** | the file, its configuration, the archive member |
   | `supervisor` | the **Agent type** | `service.name` on the wire, the package Set's key |

   The last two share a string and are separate constants. Keeping the program's name off
   `PRODUCT_NAME` is what lets **one published package Set serve every variant**: the archive member
   a self-update extracts is the same in all of them, so ADR-0022 clause 7's default — the Set is
   `supervisor` @ version @ `supervisor` — needs no per-variant exception.

   The Agent type constant is `CLIENT_AGENT_TYPE`. It never names a service, and with the service
   carrying the product's name, a name like `CLIENT_SERVICE_NAME` would read as the one thing it is
   not.

16. **A second instance is a second build**, with its own `PRODUCT_NAME` and therefore its own
   service name, `.deb`/`.rpm` package name, `/usr/libexec` payload directory, `PATH` symlink and
   MSI `UpgradeCode`. A runtime flag no delivery path could reach would be a capability nobody
   receives; a build variant is one every delivery path carries.

   This answers the question **ADR-0020 clause 13** left open when it installed "the `default`
   instance only", noting that enumerating instances "would need a product code per instance". The
   `UpgradeCode` is the one identity that cannot be derived: each variant needs a GUID minted once
   and recorded, because Windows Installer treats a shared `UpgradeCode` as grounds to remove the
   other installation. ADR-0020's own "minted once and never changed" discipline holds per variant.

   It also redraws the line **ADR-0020 clause 21** drew, which read `--root` or `--instance` as
   the mark of a manual install "where no package writes to `/usr/bin` or deletes a layout at all".
   Half that premise is gone: a non-default variant is precisely a *packaged* install that does
   both. The `--root` half stands.

17. **An installation is an isolation boundary** — separate Server, credentials, lifecycle,
   rollback — and the mechanism that makes a second one is a build, not a flag. Scaling the number
   of *managed* Agents happens *inside* one installation via the multiplexing of
   [ADR-0003](0003-client-modes-and-connection-multiplexing.md) — most hosts run exactly one
   installation. With the service not suffixed, two variants on one host are told apart by
   `service.instance.name` — which is what ADR-0022 point 2, restated by ADR-0022 clause 2,
   prescribes.

18. **The system service may run under an operator-named account, and both roots belong to it.**
   Its load-bearing sentence — *whatever account the service runs as must be able to write the
   executable layout, because ADR-0017's updater is the daemon itself* — is a statement about the
   layout, which is why it stands beside clause 9.

   `service install` takes **`--run-as <account>`**, system scope only (it conflicts with `--user`),
   and without it the service registers as root under systemd and launchd, `LocalSystem` under the
   SCM.

   - **The service runs as the account.** Linux: systemd `User=<account>`; macOS: launchd
     `UserName`; both through the `username` field `service-manager` already carries. Windows: an
     `sc config obj=` step in `windows_config` — the same "finish what the crate omits" seam the
     recovery actions already use — sets the logon account. No *Log on as a service* grant is
     performed: the default security policy grants that right to `NT SERVICE\ALL SERVICES`, which
     covers the virtual account; the built-in accounts carry it inherently; and a gMSA receives it
     from its domain's group policy. A host hardened to remove the default grant must restore it
     for this service's account, and the manual says so.
   - **Windows accepts only passwordless account forms**: the service's own virtual account
     (`NT SERVICE\<service name>`, the recommended form — and per clause 11 that name is
     `PRODUCT_NAME`), a gMSA (`name$`), or `NT AUTHORITY\LocalService`/`NetworkService`. A password
     parameter does not exist, for ADR-0020's reason: it would stand in the process list and the
     installer log. An account form that needs one is refused with a message naming the passwordless
     forms.
   - **The account must already exist** on Linux and macOS, and the install refuses early — before
     anything is written, because an install that cannot succeed must fail with a clear message —
     with a message showing the one-line `useradd --system` that creates it. Creating accounts is
     packaging's business, not this binary's. On Windows the virtual account exists implicitly with
     the service.
   - **`uninstall` still deletes nothing**, and re-running `install` with a different `--run-as`
     re-owns the same directories.

   **The hand-over names two roots.** After laying out and registering, the install hands ownership
   of **everything under the data root** — `supervisor.toml` and `state/`, because the service must
   read and write them — **and of `versions/` and `current` under the layout root**, because the
   updater is the service itself. `chown` on Unix, an ACL modify-grant on Windows. Where the two
   roots coincide, this is one operation; where they split, it is two, and an install that can
   perform only one of them has not succeeded.

   **This clause is why clause 9 keeps the Windows layout out of `Program Files`.** The hand-over
   has to reach `versions/` and `current`, so wherever the layout stands, the service's account
   must be able to write it. Under `Program Files` that would mean granting a low-privileged
   account modify rights on a `TrustedInstaller`-owned tree the service also executes from — a
   privilege-escalation surface, and the Windows form of exactly what is refused for a
   self-rewritten layout under `/usr/lib` or `/usr/libexec`. With the layout under
   `%ProgramData%\<PRODUCT_NAME>` the grant lands where per-machine mutable data belongs, and
   `--run-as` works on a Windows host installed by the MSI exactly as on one unpacked by hand.

## Alternatives considered

- **A separate installer or OS packages registering the service** (`.deb`/`.pkg`/MSI wrapping
  `sc.exe`) — spreads the logic across artifacts, fixes the installation path, and cannot express
  "register this binary from wherever it is". Subcommands keep one self-managing deployable;
  packages ship the binary and call `service install` without owning the service.
- **Hand-write the three backends** (emit systemd units and launchd plists, call
  `CreateService`/`DeleteService`) — maximal control, three code paths to own and test;
  `service-manager` abstracts exactly install/start/stop/status/uninstall across them.
- **A daemonizing crate (`daemonize`) or double-fork** — Unix-only, gives nothing on Windows, and
  modern init systems want to supervise a foreground process, not a self-detaching one.
- **Register the binary's own path instead of the `current` pointer** — simpler, but every
  self-update would re-register the service (needing admin rights on every update), and Windows
  locks the running `.exe` against replacement. The pointer costs one indirection at install time
  and makes updates a pointer switch.
- **Environment-variable configuration baked into the unit** (as the supervisor lineage on `main`
  does) — rejected: this Client is file-configured by ADR-0008; duplicating settings into the unit
  would create a second, diverging source of truth. The unit carries the config path only.
- **`anyhow` for error handling** (as the `main` lineage uses) — rejected; the crate uniformly uses
  `Result<_, String>` and the new module maps library errors to strings at its boundary.
- **Keep a three-level path, `<base>/opamp-fleet/client/<instance>`.** It trades a permanent
  three-level path whose last two levels are constants (`client`, `default`) and a dead flag against
  no saving at all.
- **Collapse the path but keep `--instance`.** The smallest change, and it fixes the naming
  contradiction. But it keeps a flag that no package sets, nothing enumerates, and the runtime
  ignores, while requiring the instance to stay in the path to keep two installations apart. It
  preserves the cost of the capability without making the capability reachable.
- **Derive the installation's identity from the config path** (hash/slug) — no extra flag, but
  unreadable service names and an identity that silently changes when the config file moves.
- **Keep the name a runtime value read from configuration.** Then it cannot name the directory the
  configuration is read from, and `service uninstall` needs the file to find the service it must
  remove. A name that identifies an installation has to exist before the installation is read.
- **Derive the program's name from `PRODUCT_NAME` too.** Tempting for consistency: a variant's
  binary would be `opamp-fleet-b`, visible as such in `ps`. It breaks the self-update. The archive
  member is extracted by name, so every variant would need its own published Set of the same
  bytes — the fleet would carry N products where it has one. Rejected in favour of clause 15.
- **Rename the program to `agent`.** `agent` is this system's word for *every* managed thing; the
  program that supervises the others cannot also be the generic term for them without
  reintroducing the collapse ADR-0022 ended. `supervisor` already says which Agent it is.

Each of the following is a way to keep executing from `/var/lib` on Linux, and each is rejected:

- **A persistent SELinux file context from the scriptlets** (`semanage fcontext -a -t bin_t …` +
  `restorecon -R`) — needs `policycoreutils-python-utils`, which the `.rpm` would have to require
  or guard; a guarded fallback fails silently, which is this bug with extra steps. It is label
  management in shell, re-run after every self-update and every relabel, to keep executing from a
  directory the policy says should not be executed from.
- **Ship an SELinux policy module in the `.rpm`** — the most packaging-orthodox answer and the
  heaviest: a policy to author, build and verify across Fedora, RHEL and the newly-enforcing SUSE
  family, for a Client whose need is fully met by standing in the right directory. It remains the
  natural follow-up if a confined domain is ever wanted (CIS Server Level 2 flags
  `unconfined_service_t` daemons); nothing here forecloses it.
- **`chcon` at staging time** — not persistent across an autorelabel (`/.autorelabel`,
  `restorecon`), and it puts SELinux-specific tooling into the Client's own runtime path on every
  distribution, enforcing or not.
- **The executable layout under `/usr/lib` or `/usr/libexec`** — the labels would work, but the
  layout is application-owned and rewritten at runtime by the self-update, and ADR-0020/0048 drew
  the ownership line exactly there: the package manager's hierarchy is the package manager's.
  `/opt` is the FHS home for software a distribution's package manager does not own.
- **Move the whole root, state included, to `/opt`** — it parks variable data in `/opt` against
  FHS, and it would give back the `Program Files` problem clause 9 solves on the other platform.

For the service account (clause 18):

- **Status quo (root / `LocalSystem` only)** — refused; least privilege is the requirement, and
  every comparable fleet agent has grown this knob.
- **systemd `DynamicUser=` / `StateDirectory=`** — Linux-only with no launchd or SCM analogue, and
  its ephemeral UIDs fight an installation whose identity, credential and state must persist across
  restarts. Clause 3 bakes absolute paths for exactly that reason, and clause 14 keeps them.
- **Arbitrary Windows accounts with a password** — the password stands in the process list and the
  installer log; ADR-0020 refused precisely this, and the passwordless forms cover the fleet cases.
  Elastic went the other way, a created local user with a managed password, at the cost of password
  machinery the virtual account gets from the OS for free.
- **A layout owned by root with self-update disabled under `--run-as`** — keeps "the service cannot
  replace its own binary" as a boundary, but breaks the self-update for exactly the installs the
  flag exists for. The specification wins.
- **A privileged updater helper** — a small root service that swings `current` on request. A second
  service, an IPC surface and a privilege boundary to defend, for one flag. Rejected as a present
  need; it remains conceivable as a future hardening ADR, and it is also the other possible answer
  to the `Program Files` tension clause 18 records.
- **Creating the account inside `service install`** — platform-specific user management in this
  binary (three APIs, three idempotency stories) that packaging does in one `postinst` line.
  Deferred to packaging.

## Sources / Prior art

- **This repository's `main` lineage** — a working single-binary implementation of the same problem
  for the supervisor host: `main:crates/supervisor/src/service/` (clap subcommands,
  `service-manager` lifecycle, `windows-service` SCM shim, `ServiceControl` seam) and
  `main:crates/supervisor/src/update/layout.rs` (versioned `versions/<sha256>/` + `current`
  pointer), with its design records `main:docs/adr/0006-supervisor-host-os-service-and-cli.md` and
  `main:docs/adr/0007-in-place-self-update-with-rollback.md`. This ADR ports that design with
  operator-chosen roots; those records also flag the launchd `KeepAlive` restart-on-stop caveat
  adopted below.
- `service-manager` crate (systemd/launchd/Windows SCM; `ServiceLevel`, `RestartPolicy`):
  <https://docs.rs/service-manager/> — v0.11.0 (2026-02-18) verified current and maintained
  (checked 2026-07-23), and verified against `main`'s lockfile to pull no TLS/crypto backends, so
  ADR-0007's rustls/ring-only constraint holds. Backend caveats from its changelog and tracker:
  `RestartPolicy` rework in 0.9–0.11, launchd `install` no longer auto-starting since 0.10, limited
  `sc.exe` restart support, and the open launchd status bug
  <https://github.com/chipsenkbeil/service-manager-rs/issues/41>. Its label rendering is why the
  grammar in clause 7 is the intersection of four naming rules rather than any one of them. Its
  [changelog](https://docs.rs/crate/service-manager/latest/source/CHANGELOG.md) documents
  `ServiceInstallCtx.username`, honoured for systemd and launchd only; Windows is explicitly left
  open, which is why clause 18 names a second SCM step.
- `windows-service` crate (`define_windows_service!`, control handler, `SetServiceStatus`):
  <https://docs.rs/windows-service/> — 0.8.1 (2026-05) current; used in production by Vector
  (its hand-rolled `vector service install` on Windows,
  <https://github.com/vectordotdev/vector/pull/2896>) and maintained by Mullvad for their own
  daemon. Microsoft's younger Windows-only `windows-services` runtime crate
  (<https://crates.io/crates/windows-services>) is a watched possible successor for the shim role.
  Windows error 1053 (service must report status to the SCM):
  <https://learn.microsoft.com/en-us/answers/questions/1389851/>; `SERVICE_STATUS` wait-hint and
  checkpoint semantics:
  <https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_status>.
- Versioned-dir + pointer self-update prior art: Elastic Agent's `data/elastic-agent-<version>-<hash>/`
  dirs, top-level symlink, upgrade marker and watcher —
  <https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md> (both its
  `<component>-<version>-<hash>` directory naming — introduced in 8.13.0 for operator readability —
  and the torn-swap lessons of elastic-agent#2264 and beats#27342 are adopted here); Scoop's
  `current` junction alias
  (<https://github.com/ScoopInstaller/Scoop/wiki/The-'Current'-Version-Alias>); the Chromium/Omaha
  updater's side-by-side versioned installs with a crash-recoverable swap bit
  (<https://chromium.googlesource.com/chromium/src/+/main/docs/updater/design_doc.md>);
  Squirrel.Windows `app-<semver>/` dirs. The OTel `opampsupervisor` specifies overwrite-with-backup
  and has not shipped package updates — the versioned layout here is deliberately the stronger
  pattern. Atomic symlink replacement (temp + `rename`):
  <https://blog.moertel.com/posts/2005-08-22-how-to-change-symlinks-atomically.html>.
- systemd: unit-name grammar and 255-char limit
  (<https://man7.org/linux/man-pages/man5/systemd.unit.5.html>), `SIGTERM`-then-`SIGKILL` stop with
  90 s default timeout (<https://man7.org/linux/man-pages/man5/systemd.kill.5.html>), `SIGHUP`
  reserved for reload (<https://man7.org/linux/man-pages/man7/daemon.7.html>), `Restart=on-failure`
  as the recommended choice for long-running services
  (<https://man7.org/linux/man-pages/man5/systemd.service.5.html>).
- launchd: `LaunchDaemon` vs `LaunchAgent`, `RunAtLoad`, `KeepAlive`, `SIGTERM` with 20 s
  `ExitTimeOut`: <https://www.launchd.info/>; label convention and `<Label>.plist` file naming:
  launchd.plist(5).
- Windows service naming: ≤ 256 chars, no slashes
  (<https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-getservicekeynamea>);
  shutdown budget `WaitToKillServiceTimeout` ≈ 5 s
  (<https://kb.firedaemon.com/support/solutions/articles/4000086193-increasing-service-shutdown-time>).
- `clap` derive: <https://docs.rs/clap/latest/clap/_derive/index.html> — 4.x current (4.6.4,
  2026-07); no clap 5 exists.
- Comparable agents' service UX — Telegraf `service install`; Elastic Agent `install`/`enroll`; the
  OTel `opampsupervisor`:
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>.
- **Elastic Agent** and **Telegraf** ship one product name per package and register a service named
  after it; multiple instances are multiple installations, not a flag. Telegraf's `--service-name`
  (<https://github.com/influxdata/telegraf/blob/master/docs/WINDOWS_SERVICE.md>) exists precisely
  because its packaging cannot express a second one.
- **Windows Installer's `UpgradeCode` semantics** (`MajorUpgrade`, `FindRelatedProducts`,
  `RemoveExistingProducts`) fix the rule in clause 16: coexistence requires distinct identity, and a
  shared `UpgradeCode` is an instruction to replace, not to install beside.
- **`cargo-deb` variants** (`[package.metadata.deb.variants.<name>]`) and `cargo-generate-rpm`'s
  `--set-metadata` are the mechanisms that let one source tree emit differently-named packages
  without templating the manifest.
- systemd `status=203/EXEC` under SELinux — the failure signature, and why the same binary runs
  interactively (`unconfined_t`) but not as a service:
  <https://thomaspowell.com/2026/04/03/the-selinux-203-exec-systemd/>; a real-world case of a
  service binary in a data directory failing exactly this way (GitHub Actions runner):
  <https://github.com/actions/runner/issues/1606>.
- `unconfined_service_t` — the targeted policy's mechanism for third-party services started by init
  from entrypoint-typed files (Dan Walsh, its author):
  <https://danwalsh.livejournal.com/70577.html>; Red Hat's documentation of unconfined process
  domains:
  <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/selinux_users_and_administrators_guide/sect-security-enhanced_linux-targeted_policy-unconfined_processes>;
  CIS Server Level 2 flagging unconfined daemons (why a policy module stays a possible follow-up):
  <https://access.redhat.com/solutions/6714611>.
- openSUSE Leap 16.0 / SLES 16 release notes — SELinux targeted policy, enforcing by default,
  replacing AppArmor:
  <https://doc.opensuse.org/release-notes/x86_64/openSUSE/Leap/16.0/html/release-notes-leap-160/index.html>.
- FHS 3.0 — `/opt`: add-on application software packages (§3.13), with variable data placed under
  `/var`: <https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch03s13.html>.
- Elastic Agent installs to `/opt/Elastic/Agent` on Linux (`--base-path` to override) — the same
  layout lineage clause 3 cites:
  <https://www.elastic.co/docs/reference/fleet/installation-layout>.
- **Microsoft's `Program Files` / `ProgramData` guidance** is the Windows counterpart and the
  reason the second split has a different cause: per-machine application data that changes at
  runtime belongs under `%ProgramData%`, and `Program Files` is `TrustedInstaller`-owned and
  read-only to everything else after installation.
- [Microsoft: Service User Accounts](https://learn.microsoft.com/en-us/windows/win32/services/service-user-accounts)
  and [virtual accounts](https://docs.delinea.com/online-help/privilege-manager/install/upgrades/virtual-accounts.htm)
  — `NT SERVICE\<name>` accounts are provisioned per service with OS-managed passwords.
- [Microsoft: `sc.exe config`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/sc-config)
  / [`ChangeServiceConfig`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfiga)
  — setting the logon account does not itself grant the *Log on as a service* right; the default
  security policy's grant to `NT SERVICE\ALL SERVICES` is what covers virtual accounts.
- [Elastic Agent: unprivileged mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged)
  — the same layout lineage again, installed by root but *running* as a dedicated
  `elastic-agent-user` that owns the agent's files, upgrades included.
- [OpenTelemetry Collector Linux packages](https://opentelemetry.io/docs/collector/install/binary/linux/)
  — the `.deb`/`.rpm` create a dedicated `otelcol` system user and run the unit with `User=`.
- Specification Mission, Strategy, and Goals #10/#11
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); [ADR-0003](0003-client-modes-and-connection-multiplexing.md)
  (one Client binary), [ADR-0007](0007-dual-transport-and-tls.md) (rustls/ring dependency
  constraint), [ADR-0008](0008-toml-configuration.md) (`--config`-pointed TOML file),
  [ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) (the program, configuration and
  version-directory names).

## Consequences

- Positive: one self-contained binary installs, controls, and deregisters itself as a native
  service on all three platforms, from any location — and still runs standalone in the foreground.
  Graceful shutdown covers `SIGTERM`, so a service stop sends the OpAMP `agent_disconnect` goodbye
  instead of being killed. The self-update needs no re-registration: it stages a version and
  switches `current`, using the narrow `ServiceControl` seam.
- Positive: the layout says what it is. `/opt/opamp-fleet/current/supervisor` — the product, then
  the program; no level exists solely to hold a constant.
- Positive: an illegal name fails the build, not the host. The grammar is a compile-time check, so
  the class of failure it guards against cannot reach a service manager at all.
- Positive: one Set updates every variant, a direct consequence of clause 15, and the reason the
  Agent type must stay off `PRODUCT_NAME`.
- Positive: one product, one layout, however it was installed. A Windows host set up by the MSI and
  one set up from the archive put the same things in the same kinds of place, and a credential left
  behind by an uninstall sits where Windows expects data to survive rather than in a directory that
  is supposed to be gone.
- Positive: the `.rpm` produces a service that starts on Fedora, RHEL and openSUSE Leap 16 / SLES 16
  with SELinux enforcing — no new dependency, no policy module, no labelling step to keep alive,
  and self-update staging inherits the working label by construction.
- Positive: the Client and everything its Supervisors spawn can drop root and `LocalSystem` on
  operator demand, with both roots belonging to the account that uses them, and with no password
  anywhere in the Windows story.
- Negative / trade-offs: three dependencies (`clap`, `service-manager`, target-gated
  `windows-service`) plus `sha2` for content addressing; a Windows-only runtime path Unix never
  exercises; system-scope installs need root/Administrator and must fail with a clear message. The
  managers differ in ways the code handles, not papers over — the SCM marker argument, launchd's
  `KeepAlive` restart-on-stop semantics (must hold the service down after an explicit stop; verify
  on real hardware), launchd `status` being advisory until the upstream bug is fixed, `install` not
  auto-starting on launchd, and the `sc.exe` backend not expressing the restart policy (closed by
  ADR-0017 point 5's recovery actions). Antivirus scanners can transiently lock freshly staged
  executables on Windows; the pointer swap retries with backoff. Real service registration cannot
  run in CI: CI has a Windows/macOS compile-lint-test job for the client, while runtime behaviour
  needs a documented manual smoke checklist. Two installations pointed at the *same* explicit
  `--root` would fight over `current`; roots must be per installation (the defaults are, being named
  by `PRODUCT_NAME`). `uninstall` deregisters only and never deletes the layout or state. A
  retention policy for version directories is not decided here. Service-mode logging, which the SCM
  would otherwise lose with the service's stderr, is ADR-0026's.
- Negative: `service install` has a second root flag. `--data-root` is one more thing to get wrong,
  and it is the flag that has to be right on exactly the platform where getting it wrong is silent:
  a Linux system-scope install that names only `--root` collapses both halves into a directory the
  operator then owns the labelling of. The MSI is spared — it passes neither root and needs no
  second property — so `tests/msi_exe_command.rs` keeps parsing one path, not two.
- Negative: the default install spans two directories on Linux at system scope, so the manual and
  every path a support engineer greps for must name both. A `--root` install on an enforcing host
  still fails if the operator roots it somewhere unexecutable — a documented property of choosing a
  root, not a default anybody gets. A host mounting `/opt` `noexec` breaks, which is rarer than
  enforcing SELinux by orders of magnitude, and loud rather than silent.
- Negative: multi-instance is a build-time decision. An operator who wants a second installation
  cannot get one by passing a flag; someone must produce a variant build. This is a real reduction
  in what a single artifact can do, accepted because a flag would be unreachable from every
  artifact we actually ship — the capability stands where it can be delivered, but it is less
  immediate.
- Negative: each variant costs a minted `UpgradeCode` and a package-name entry, and both are
  manual and permanent. A forgotten `UpgradeCode` does not fail loudly at build time; it fails on a
  Windows host by removing the other installation.
- Negative: two constants hold the string `supervisor` for different reasons. The name
  `CLIENT_AGENT_TYPE` and the doc comments are what keep them apart; a future reader who conflates
  them would couple the Agent type to the product name and split the fleet's package Sets without
  noticing.
- Negative: the account is a trust boundary. Whoever holds it can replace the binary in the layout,
  and the `PATH` symlink through `current` means an administrator invoking the CLI executes
  account-owned code — the manual must say so plainly. Managed Processes inherit the account, so
  anything needing ports below 1024 or root-only telemetry sources fails under it. That is the
  operator's informed choice, not this decision's.
- Open: the coverage gap that hid the SELinux failure is still open.
  `crates/client/tests/service_smoke.rs` excludes hosts with SELinux or AppArmor in the way, so
  nothing automated exercises the reason clause 9 exists.
- Follow-ups: possibly config reload on `SIGHUP`; possibly `Type=notify`/watchdog integration once
  an update health gate wants it. The `.deb`/`.rpm` packaging grows a `postinst` account creation
  and a `--run-as` wiring; the MSI can offer the virtual account as a checkbox; the manual's
  `service install` section documents the flag, the ownership hand-over across both roots, and the
  trust boundary. Whether variant builds are ever actually published, and if so what the release
  pipeline's matrix looks like, is deliberately left open — this decision makes them possible
  without committing to shipping any. If they are, the `UpgradeCode` register needs a home, and the
  tension between artifacts named after the Set (ADR-0022 clause 8) and packages named after the
  product will need stating where ADR-0020 clause 12 requires all four artifacts of a target to share
  a name. How an operator discovers what is installed on a host is not answered here and would need
  its own decision.
