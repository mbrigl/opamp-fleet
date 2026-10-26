# ADR-0019: The installed service and its state

- **Status:** 🟢 accepted
- **Date:** 2026-10-23
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-agent/src/cli.rs`, `crates/fleet-agent/src/main.rs`, `crates/fleet-agent/src/service/` (the state directory a re-registration reads in `manager.rs`, the directories a system-scope install restricts in `windows_rights.rs`), `crates/fleet-agent/src/config_init.rs`, `ClientConfig::service_state_dir` in `crates/fleet-agent/src/config_file.rs`, `crates/fleet-agent/src/logging.rs`, `crates/fleet-agent/src/product.rs`, `crates/fleet-agent/build.rs`, the `[package.metadata.deb]` and `[package.metadata.generate-rpm]` tables of `crates/fleet-agent/Cargo.toml`, `packaging/linux/`, `packaging/windows/`, the `state_dir` and `supervisor_dir` comments of `config/supervisor.toml`, the program, Agent type and configuration-file names of the Client, `service install`, `service config`, and every path or name an installed Client uses and every document that tells an operator where an installed Client keeps its state

## Context

The specification commits the Client to install as a native operating-system service that updates
itself in place, on Linux, macOS and Windows (Mission, Strategy, G-11). A fleet client must run
without a logged-in user, start at boot, survive the service restart a self-update causes
([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)), and explain itself when it
fails on a host nobody watches. It puts security before convenience (Strategy "Security before
convenience", Q-1 "Secure by default"): the first configuration an installer writes must
authenticate and must not send plaintext beyond the loopback. Plaintext is accepted on the loopback
alone, a host name is never the loopback, and a Client without a client certificate is refused at
startup. The Agent plane admits a peer by its client certificate alone
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)), so there is no fleet
credential for an installer to ask for, write or name.

Forces:

- **The three service models differ.** systemd and launchd supervise an ordinary foreground
  process. The Windows Service Control Manager (SCM) launches the process and expects status
  reports within about 30 seconds or kills it with error 1053, and a process cannot reliably detect
  an SCM launch, so the registered command line must carry a marker. The SCM discards a service's
  stderr.
- **What the service points at decides how an update works.** Registering the binary's own path
  would force a re-registration on every update, and Windows locks a running `.exe`. A stable
  pointer into side-by-side version directories makes an update a pointer switch; the layout itself
  is [ADR-0020](0020-the-accounts-the-service-runs-under.md)'s.
- **The layout is rewritten at runtime by the daemon itself**, so it must stand where the platform
  lets the service execute it *and* where the service's own account may write it. On Fedora, RHEL
  and openSUSE Leap 16 / SLES 16 the SELinux targeted policy is enforcing by default, and systemd
  (`init_t`) may not execute a file labelled `var_lib_t`: a binary under `/var/lib` installs,
  registers, and dies at its first start with `status=203/EXEC` and an AVC denial the unit's
  journal never explains, while the same binary runs fine from a shell. On Windows,
  `Program Files` is `TrustedInstaller`-owned and meant to be read-only once installation finishes.
- **A name that identifies an installation has to exist before the installation is read.** It
  names the directory the configuration is read from, and `service uninstall` must find the
  service without that file. It must at once be a systemd unit name, a launchd label, an SCM
  service name and a directory name on every platform.
- **Nothing ships the configuration.** A release artifact is the bare binary, and the Client loads
  defaults when its file is absent ([ADR-0008](0008-five-crates-on-tokio-and-axum.md)), so a
  service installed without one starts, dials the development default, and manages nothing,
  silently. `install` is also the command an Ansible play, an MDM profile or an MSI invokes: a
  prompt there does not fail, it hangs. And a first configuration names a client identity, files
  that must already be on the host or that an enrolment obtains
  ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)).
- **`service install` runs as root, or as an Administrator on Windows**, and the Linux
  post-install and the MSI run it on every upgrade. `supervisor.toml` belongs to the service's
  account and is the account's to rewrite. The install cuts the state directory off from inherited
  rights on Windows and grants the account Modify on it, hands it over by a recursive ownership
  change on Linux and macOS, and writes the enrolment's files into it as root. If the state
  directory came from the file, an account the Server reaches, or anything that compromises the
  Client, could name any tree (another service's data, a user's profile, a system directory), and
  the next upgrade would hand that tree to it: a privilege escalation from the service's account to
  arbitrary file access on every platform. A Server cannot write those keys, since it delivers
  `[[supervisor]]` blocks only ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)); a compromised
  service account can. The registration (`/etc/systemd/system/<PRODUCT_NAME>.service`,
  `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`, or the SCM's entry) is what the Client wrote as
  root, and only an administrator can change it.
- **A Supervisors' root is relocatable**, on purpose: programs off a `noexec` or undersized `/var`.

An operator also needs something to install. A host needs what its platform expects: `apt` and
`dnf` with an inventory, an upgrade path and a clean removal; on Windows an `.msi`, the only thing
Intune, Group Policy and SCCM deploy. `service install` owns the install (the layout, the `current`
pointer, the registration with its single-token name, display name and Windows recovery actions),
so a native package that registered a service of its own would write a second registration missing
all of that. Self-update and a package manager must not own the same bytes: if `dpkg`, `rpm` or the
MSI owned the layout, every fleet update would leave the host in a state `dpkg -V` reports as
modified and the next `apt upgrade` silently reverts. The releases that carry these packages are
[ADR-0034](0034-releases-attestations-and-the-server-image.md)'s.

And one force on the name. The Baseline reserves `service.name` for the Agent *type* and recommends
a reverse FQDN, a recommendation this project does not enforce
([ADR-0016](0016-what-an-agent-reports-about-itself.md)), and which opamp-spec issue 131 records as
an overload of that key. Among Collectors, Foreign Agents and the process that supervises them, the
useful type for the last is the role it plays. What the fleet offers the thing is named after what
the thing is, and so is the thing itself.

## Decision

We will make the Client the program `supervisor`, a `clap` subcommand CLI that registers, controls
and runs itself as a native service on systemd, launchd and the Windows SCM under a product name
fixed at build time, from a layout kept apart from its data on Linux at system scope, that takes
an installed Client's state directory from `--state-dir`, else its registration, else
`supervisor.toml` only beneath the install's roots, writes its first configuration when asked and
never one the Client would refuse at startup, logs to a bounded file when it runs as a service,
and is delivered by `.deb`, `.rpm` and `.msi` packages that run its own `service install`.

### The program and its names

1. **One CLI, thin, over a `service` module.** The subcommands are `run` (the foreground daemon,
   and the default when no subcommand is given) and `service install | config | uninstall | start
   | stop | status`. The global flags are `--config` (default `supervisor.toml`) and `--state-dir`;
   every service verb takes `--user`. There is no environment-variable configuration: the Client is
   file-configured ([ADR-0008](0008-five-crates-on-tokio-and-axum.md)) and a registered service
   carries the configuration's *path*, never its content. clap's `version` is wired to
   `opamp::version::current()` ([ADR-0033](0033-the-products-version.md)). The daemon code lives
   in `crates/fleet-agent/src/service/` (`runtime`, `layout`, `manager`, `run_as`, and the
   Windows-only `windows`, `windows_config`, `windows_rights`), behind the narrow `ServiceControl`
   seam (`start`, `stop`, `state`) that the self-update depends on. Errors are
   `Result<_, String>`.

2. **`PRODUCT_NAME` is a build-time constant.** Default **`opamp-fleet`**, overridable with
   `OPAMP_PRODUCT_NAME` for a variant build. `crates/fleet-agent/build.rs` validates it against the
   intersection of the systemd-unit, launchd-label, SCM-name and directory-name grammars (lowercase
   `[a-z0-9-]`, 1–32 characters, no leading or trailing `-`, never a Windows reserved device name:
   `con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`) and **fails the build** on a name
   that breaks it, so an illegal name cannot reach a service manager. The display name, default
   **`OpAMP Fleet Agent`**, is the constant `PRODUCT_TITLE`, overridable with `OPAMP_PRODUCT_TITLE`
   and only required to be non-blank. It is prose, cannot be derived from the slug by any rule that
   would read correctly for the next variant, and appears only where prose belongs: the Add/Remove
   Programs entry, the SCM's display column and the installer dialog's title. It is not the Agent
   type, so the fleet view never prints one word in both columns
   ([ADR-0016](0016-what-an-agent-reports-about-itself.md)).

3. **Three names sit side by side, and none is derived from another.**

   | Name | Names | Appears in |
   |---|---|---|
   | `PRODUCT_NAME` (`opamp-fleet`) | the **product** | the install path, the service, the `.deb`/`.rpm` package, the `/usr/libexec/<PRODUCT_NAME>/` and `Program Files\<PRODUCT_NAME>` payload directories, the `PATH` symlink |
   | `supervisor` (`layout::COMPONENT`, `BINARY_FILENAME`) | the **program** | the file, its configuration `supervisor.toml`, the version directories, the archive member |
   | `supervisor` (`CLIENT_AGENT_TYPE`) | the **Agent type** | `service.name` on the wire, the Package's key |

   The program's and the Agent type's names are those of clauses 6 and 4; they share a string and
   are separate constants. Keeping the program's name off `PRODUCT_NAME` is what lets **one
   published Package update every variant build**: the archive member a self-update extracts is
   the same in all of them.

4. **The Agent type is `supervisor`**: the constant `CLIENT_AGENT_TYPE` every Client reports as
   `service.name` ([`agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs)), the same on
   every host, because every Client in a fleet is the same kind of thing.

5. **The package that carries the Client is `supervisor` too.** The self-update takes only the
   package named after the Agent type
   ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md) clause 2), and a Server
   offers a package under the Agent type it is built for
   ([ADR-0027](0027-packages-signatures-and-delivery.md)), so the release a fleet uploads for its
   Clients is a Package for Agent type `supervisor`, and no per-host setting is needed. A
   Configuration carrying the Client's `[[supervisor]]` blocks
   ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)) is typed `supervisor` as well.

6. **The program is `supervisor`.** `supervisor.exe` on Windows: the binary Cargo builds
   (`[[bin]] name = "supervisor"` in the package `fleet-agent`), the file in every version
   directory (`supervisor-<MAJOR.MINOR.PATCH>-<hash>`), the member a package artifact carries, the
   payload file under `/usr/libexec/<PRODUCT_NAME>/`, the log file, the self-check token and the
   CLI's own name. The configuration file is **`supervisor.toml`**, and the `--config` default with
   it. What names an *installation* (the install path, the service, the dpkg/rpm package, the
   `PATH` symlink) is the product's name, not the program's (clauses 2 and 3, and the service name
   of [ADR-0020](0020-the-accounts-the-service-runs-under.md)). The program's name is not derived
   from the product's, so one published Package serves every variant build: the member a
   self-update extracts is the same in all of them. Two names deliberately stay where they are: the
   Cargo package `fleet-agent`, a build-time identifier that never leaves the repository, and the
   OTLP instrumentation scope `opamp-fleet-client`, which names the library a signal came from;
   renaming it would move every operator's dashboards.

7. **The instance name is a separate attribute, and its default is `Supervisor Agent`.** The
   top-level `name` is the operator's name for *this* Client, reported as `service.instance.name`
   ([ADR-0016](0016-what-an-agent-reports-about-itself.md)). Its default is a display name (spaces
   and capitals) and deliberately not `supervisor`: a default equal to the type would print the
   same word in both columns of the fleet view. Nothing resolves a path or a service from this key.

8. **One name per thing, with no alias.** No artifact carries the program twice, no version
   directory holds a compatibility link, and the loader reads no configuration file but the one it
   is given. **A Client that finds no `supervisor.toml` at its configured path but a `client.toml`
   beside it refuses to start**, naming both paths and the `mv` that fixes it: coming up on
   defaults there, dialling the development endpoint and managing nothing, is the one outcome
   nobody would see. An artifact whose member is not `supervisor` is refused by the self-update and
   the host stays on the version it runs.

9. **A second installation is a second build.** A variant has its own `PRODUCT_NAME` and therefore
   its own service name, package name, `/usr/libexec` payload directory, `PATH` symlink and MSI
   `UpgradeCode`. The `UpgradeCode` is the one identity that cannot be derived: each variant mints
   one GUID once and records it, because Windows Installer treats a shared `UpgradeCode` as an
   instruction to remove the other installation. An installation is an isolation boundary:
   separate Server, client identity, lifecycle and rollback. Scaling the number of *managed* Agents
   is the multiplexing inside one installation
   ([ADR-0015](0015-client-modes-and-the-gateway.md)); two variants on one host are told apart by
   `service.instance.name` ([ADR-0016](0016-what-an-agent-reports-about-itself.md)).

10. **There is no instance flag.** `--instance` is not a flag at all (not hidden, not accepted and
    ignored), so a command line carrying it fails to parse, and no registered service carries it.
    `service uninstall | start | stop | status` take no name: with one service per build there is
    nothing to look up. The name grammar of clause 2 is also the grammar of `[[supervisor]]` block
    names (`cli::parse_instance_name`).

### Where an installation lives

11. **The install path is `<base>/<PRODUCT_NAME>`, one level, never fixed.** It is named by the
    product, never by its display name.

    | Platform, scope | Executable layout | Data |
    |---|---|---|
    | Linux, system | `/opt/<PRODUCT_NAME>` | `/var/lib/<PRODUCT_NAME>` |
    | Linux, user | `$XDG_DATA_HOME/<PRODUCT_NAME>` (else `~/.local/share/…`) | same |
    | macOS, system | `/Library/Application Support/<PRODUCT_NAME>` | same |
    | macOS, user | `~/Library/Application Support/<PRODUCT_NAME>` | same |
    | Windows, system | `%ProgramData%\<PRODUCT_NAME>` | same |
    | Windows, user | `%LOCALAPPDATA%\<PRODUCT_NAME>` | same |

    The MSI's payload directory is `C:\Program Files\<PRODUCT_NAME>`; it is not the layout
    (clause 12).

12. **Program and data split only on Linux at system scope, and only where the platform gives a
    reason.**
    - **Linux:** the executable layout is under `/opt`, whose default file context `usr_t` is an
      entrypoint type through which systemd starts a third-party service in
      `unconfined_service_t`. Every version the self-update stages later inherits that label, so
      staging needs no SELinux tooling, no policy module and no new runtime dependency; a label
      applied once at install time would expire at the next staged version or relabel. FHS 3.0
      places add-on software under `/opt` and its variable data under `/var`, so the split is the
      standard layout. The data root stays in `/var/lib`.
    - **Windows does not split.** The MSI delivers its payload to `Program Files\<PRODUCT_NAME>`
      and then runs the same `service install` the command line would, which builds the layout and
      the data under `%ProgramData%\<PRODUCT_NAME>`. The layout is not put in `Program Files`: the
      service's own account must be able to write it
      ([ADR-0020](0020-the-accounts-the-service-runs-under.md)), and granting a low-privileged
      account modify rights on a `TrustedInstaller`-owned tree it executes from is a
      privilege-escalation surface. The line is the one the Linux packages draw between the
      package-owned `/usr/libexec/<PRODUCT_NAME>/` payload and the program-owned `/opt` layout
      ([ADR-0020](0020-the-accounts-the-service-runs-under.md)). The MSI passes no root flag, so no
      directory property reaches a command line, and an MSI host and an archive host put the same
      things in the same places.
    - **Two flags.** `--root` names the layout root; given **alone** it collapses layout and data
      into that one directory, whose labelling and permissions are then the operator's business.
      `--data-root` names the data root; alone, it moves only the data. A packaged install passes
      neither and takes the platform defaults.
    - **What a package removal takes.** On **remove** the layout root and the `PATH` symlink (they
      hold nothing but staged binaries) and on **purge** additionally the data root, with the
      configuration and the identity in it. On Windows the MSI's uninstaller empties only
      `INSTALLFOLDER`, and the data under `%ProgramData%` survives: the same remove-versus-purge
      shape. An install rooted elsewhere with `--root` is manual, and no package touches it.

13. **An upgrade on an installed host is a re-registration, never a migration.**
    - `--config` keeps pointing into the data root. `--state-dir` keeps pointing where the
      registration has it (clause 14).
    - A re-registration rewrites the program path (`ExecStart`) and the rest of the unit or plist
      from the version that runs it, the systemd unit's directives included
      ([ADR-0020](0020-the-accounts-the-service-runs-under.md)). The account stays the one the
      registration names ([ADR-0020](0020-the-accounts-the-service-runs-under.md)).
    - The instance's configuration, identity and state never move.
    - A change that would move the data root or the state directory of an installed host must bring
      its own migration and carries the full weight of this rule. An administrator who names
      another `--state-dir` moves nothing: the service then starts on an empty state directory and
      enrols anew.

14. **The state directory is taken in this order.**
    1. **`--state-dir`**, the global flag (clause 1), given to `service install` or
       `service config` and made absolute.
    2. **The registration's**, at system scope: the value after `--state-dir` in the command line
       of the registration the Client wrote.
       - Under systemd it is `ExecStart=` of `/etc/systemd/system/<PRODUCT_NAME>.service`.
       - Under launchd it is `ProgramArguments` of `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`.
       - Under the SCM it is the service's binary path name, read with `QueryServiceConfig`.
       - The install reads what the Client wrote, never a drop-in and never the manager's loaded
         state, as the account a registration names is read
         ([ADR-0020](0020-the-accounts-the-service-runs-under.md)).
       - A registration that names no `--state-dir`, and every user-scope one, offers none. A user
         scope has no administrator between the file and the service.
    3. **The file's**: the absolute `state_dir` that `supervisor.toml` names, and only beneath the
       data root or the layout root (clause 15). A relative one names none.
    4. **`<data root>/state`.**

    `service config` finds the state directory by the same order, as clause 23 asks of it.

15. **Beneath a root means beneath it as spelled, through directories only.**
    - A directory is beneath a root when its path starts with the root's path, compared component
      by component as spelled. Nothing is resolved, so another spelling of the root is not beneath
      it.
    - Below the root, no component may be `.` or `..`.
    - Every component below the root that exists must be a directory and not a symbolic link or
      junction. What does not exist yet ends the check.
    - The roots themselves are named by `--root` and `--data-root` or by the platform default, and
      this check does not inspect them. Whether a root may itself be a link is a question of its
      own, which this clause leaves open.
    - This holds for a state directory from any source that lies beneath one of the roots: the
      service's account owns what is under them.
    - A state directory from the file that fails it is refused, naming the directory, why, and
      `--state-dir`. The refusal comes before the install writes anything.

16. **A Supervisors' root outside the roots is the operator's.** A `supervisor_dir` outside the
    data root, the layout root and the state directory stays as relocatable as the Supervisors'
    directories are made ([ADR-0020](0020-the-accounts-the-service-runs-under.md)). A system-scope
    install on Windows never creates, restricts or hands over such a root. It leaves it as it is
    and names it in a warning. On Linux and macOS an install never touches a Supervisors' root.

### The service

17. **The lifecycle runs over `service-manager`, system scope by default.** System scope is a
    systemd system unit, a launchd `LaunchDaemon` or an SCM service, because a fleet client runs
    without a logged-in user and starts at boot; `--user` is a development opt-in, refused on
    Windows, which has no user-scope services.
    - **Restart on failure, never after an explicit stop**, after 5 seconds and with no retry
      limit: what lets the self-update stop the service and switch `current` without the manager
      racing it. systemd and launchd carry the policy natively (`Restart=on-failure`,
      `KeepAlive{SuccessfulExit:false}`); the crate's `sc.exe` backend discards it, so
      `windows_config` sets SCM recovery actions with the same delay and enables them for
      non-crash failures, and sets the display name (`sc config`) and the Description
      (`sc description`).
    - **A system-scope install that lacks the rights fails before anything is written**: on
      Windows by probing the SCM for `CREATE_SERVICE` first, with a message saying so.
    - **`install` does not start the service** on launchd and says so, printing the follow-up
      `service start`; launchd `status` is advisory.
    - **`uninstall` deregisters only.** It never deletes the layout, the configuration or the
      state.

18. **Under the manager the daemon is the same `run` loop.** On Linux and macOS it is a plain
    foreground process: `SIGTERM` and `SIGINT` shut it down gracefully, through the transports, so
    the OpAMP `agent_disconnect` goodbye is sent on a service stop too, well within launchd's
    20-second `ExitTimeOut`; `SIGHUP` is explicitly ignored rather than left to terminate it. On
    Windows a `cfg(windows)` shim on the `windows-service` crate registers the control handler,
    reports `StartPending` → `Running` → `StopPending` → `Stopped`, and runs the identical body. The
    hidden `run --service` marker routes into the SCM dispatcher on Windows and, on every platform,
    says no terminal is watching (clause 19).

19. **A Client running as a service writes its own log to a rotating file, on every platform.**
    `run --service` writes it; a foreground run writes stderr alone, because somebody is reading
    it. The file is written on Linux and macOS too, although the journal already holds stderr
    there, so "where are the logs" has one answer everywhere, including a container. It lives in
    `<state dir>/logs/` as `supervisor.<date>.log`: the state directory survives self-updates and
    `uninstall`, so a log explaining a failed start is still there afterwards.

20. **The log is bounded, and `[logging]` is the machine's.** It rotates daily and keeps a fixed
    number of days, 7 by default. `[logging]` in `supervisor.toml` takes `dir` (moves the file),
    `keep` (days kept) and `enabled = false` (off, for an operator whose platform collects stderr).
    `keep = 0` is refused at load rather than read as "forever", and an unknown key fails startup.
    The bound is on age and file count, not bytes: the Client is quiet and backs off when it cannot
    reach the Server. The section never arrives over the wire: a Server able to redirect or silence
    a Client's log could hide its own effects
    ([ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)).

21. **The file never costs the Client its run.** `tracing` takes one subscriber per process,
    installed before the command line is parsed, so the file layer is installed from the start
    with a writer that discards until the run opens the file; events before that still reach
    stderr. `tracing-appender` (from `tokio-rs/tracing`, beside `tracing-subscriber`) does the
    rolling, non-blocking. A directory that cannot be written or a full disk is reported once and
    the Client runs without the file.

### The first configuration

22. **Without `--config`, the configuration is `<data root>/supervisor.toml`.** One rule for every
    platform, and the absolute path registered is the one just written. An explicit `--config`
    wins; because the flag has a default value, the two cases are told apart by clap's value
    source, not by comparing with the default string.

23. **Interactivity is an opt-in flag on `install`, and `service config` is the questionnaire
    alone.** `service install --interactive` runs the questionnaire; without it `install` never
    asks anything, because it is the scripted command. `service config` runs the same
    questionnaire against an installed Client and registers nothing: it finds the file as
    `install` does (`--config`, else `<data root>/supervisor.toml` from `--user`, `--root` and
    `--data-root`) and its state directory the way `install` derives it (clause 14). The file must
    load before the questionnaire asks: one that does not is corrected by hand. Either with a stdin
    that is not a terminal is an **error**, not a fallback: a provisioning run must fail with a
    message rather than hang.

24. **A configuration file that exists is never overwritten, and the questionnaire edits it in
    place.** No installer-supplied answer and no `install` without `--interactive` touches it:
    `install` proceeds with that file and names it. The questionnaire (`--interactive` and
    `service config` alike) offers the value the file holds as the default of every question and
    changes only the keys it asks for; every other key, every comment, the `[[supervisor]]` blocks
    and the self-update consent stay as written. A re-install can never eat the answers typed into
    the first, or the edits made to the file since.

25. **The written file is treated as holding a secret:** mode `0600` on Unix; on Windows it
    inherits the data root's ACL, administrator-owned at system scope. It holds no credential. It
    may name where a client identity's private key lies, and it may hold `[packages] archive_key`,
    the fleet's archive decryption secret, which an operator adds by editing the file the installer
    created.

26. **The configuration is validated before the service is registered, by the rules the Client
    applies at startup.** The order is write → load through `ClientConfig::load` → enrol, when the
    questionnaire took a bootstrap identity or one lies beside the program
    ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)) → stage the layout → register, so a
    broken configuration fails at install and not at the service's first start. A `ws://` or
    `http://` endpoint off the loopback fails the install, naming the setting. A missing client
    identity is what the Client refuses at startup; the install names it in a warning and registers
    the service all the same, and the service refuses to run until the file is complete, so nothing
    ever runs unauthenticated. A file that already exists is validated the same way. A written file
    that fails to load is left on disk and named in the error. An interrupted enrolment leaves the
    written file and registers nothing.

27. **A non-interactive install with no configuration file warns.** Not an error (automation must
    not break) but a printed line naming the path registered and saying the Client refuses to start
    until that file exists with a client identity.

28. **The prompts come from `dialoguer`.** It validates an answer and asks again, offers a default
    and a yes/no confirmation, the same way on three operating systems; `dialoguer` is
    MIT-licensed and brings no TLS or crypto backend. The questionnaire asks for no secret, so no
    prompt hides its input. The terminal check itself is `std::io::IsTerminal`, no dependency.

### The installers

29. **Three classes of asset, and only one is a package artifact.** The Client's archive is opened
    by the Client on a self-update and parsed by the fleet view; the operator tool's archives
    ([ADR-0032](0032-the-operator-program-opamp-fleetctl.md)) are downloaded to an operator's
    machine and are no package of the fleet; the `.deb`, `.rpm` and `.msi` are opened by `dpkg`,
    `rpm` and Windows Installer and by nothing in the fleet path. The installers are **additive
    operator artifacts**: they never replace the archive, which is the only format the fleet can
    install.

30. **The endpoint dialog is prefilled with the development Server, interactively only.** A
    `SetProperty` in the UI sequence, conditioned on `ENDPOINT` unset and the product not
    installed, sets `wss://127.0.0.1:4320/v1/opamp`. A silent install that names no `ENDPOINT`
    still writes no configuration, so unattended deployment never acquires the development default
    by omission; a command-line value wins; clearing the field is the configure-later answer. The
    value is the loader's default verbatim. `http://localhost` does not qualify on either count:
    the Agent plane always serves TLS, so a development Server answers no plaintext request, and
    `localhost` is a host name, which is never the loopback (only the IP literals `127.0.0.1` and
    `::1` are), so the Client would refuse the value at startup. The scheme selects the transport
    ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)), so the click-through install runs
    WebSocket, as an unconfigured Client would. Like every value an installer writes, the prefill
    passes the rule the Client applies at startup
    ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md), on what an installer writes).

31. **What builds the installers.** Additional steps in the release's `build` matrix job, after the
    archive is packed, consuming the binary that job built, so the bytes in a `.deb` are provably
    the bytes in the archive:

    | | tool | runs on |
    |---|---|---|
    | `.deb` | `cargo-deb`, `--no-build --no-strip` | both Linux runners, natively |
    | `.rpm` | `cargo-generate-rpm`, no `rpmbuild` needed | both Linux runners, natively |
    | `.msi` | WiX Toolset 6 as the `wix` .NET tool, with its UI extension | the Windows runner |

    Package metadata lives in `crates/fleet-agent/Cargo.toml` under `[package.metadata.deb]` and
    `[package.metadata.generate-rpm]`, reading the workspace's version, licence and description;
    `cargo-deb` derives `Depends` from what the binary links (`$auto`). The RPM `Release` is `1`
    and stays `1`, and no epoch or `~` mangling is needed, because a version is released once and
    is always `MAJOR.MINOR.PATCH` ([ADR-0033](0033-the-products-version.md)). The maintainer
    scripts live in `packaging/linux/`, the WiX sources in `packaging/windows/`.

**Out of scope:** the versioned layout and its `current` pointer, the service's name, the account
it runs under, what the Linux packages deliver and remove, and the MSI's dialogs and actions
([ADR-0020](0020-the-accounts-the-service-runs-under.md)); staging, verifying, rolling back and
pruning versions, and the self-update consent
([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)); the questions of the first
configuration, `service install --endpoint` and what lies beside the program
([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)); that no installer starts the service
it registers ([ADR-0020](0020-the-accounts-the-service-runs-under.md)); whether
variant builds are ever published, and where their `UpgradeCode`s are recorded; how an operator
discovers what is installed on a host; a confined SELinux policy module; configuration reload on
`SIGHUP`; `Type=notify` integration; the startup rules themselves and the Server's enrolment window
and approval, which the installer only applies and names
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)); hosting apt and yum
repositories; preseeding the endpoint on Linux; a macOS `.pkg`, which needs a Developer ID and
notarization; the files an enrolment writes as root into the state directory, which no
privileged step writes through a link ([ADR-0020](0020-the-accounts-the-service-runs-under.md)
clause 11); the `[tls]` files an enrolment reads as root; `[logging] dir`, which only the
service's own run uses; the releases and the Server's image
([ADR-0034](0034-releases-attestations-and-the-server-image.md)).

## Alternatives considered

- **Separate installers or OS packages owning the service** (shipping a unit file, wrapping
  `sc.exe`): spreads the logic across artifacts and gives two registrations that disagree; the
  packages ship the binary and call `service install`.
- **Hand-write the three backends**: three code paths to own; `service-manager` abstracts exactly
  install/start/stop/status/uninstall, and the wrapper stays thin enough to finish what it omits
  (`windows_config`). **A daemonizing crate or double-fork**: Unix-only, and init systems want to
  supervise a foreground process.
- **Register the binary's own path instead of `current`**: every update would re-register the
  service with administrator rights, and Windows locks the running `.exe`.
- **Environment-variable configuration in the unit**: a second, diverging source of truth.
  **`anyhow`**: the crate uses `Result<_, String>` throughout.
- **A runtime instance flag**: no package can pass it, nothing enumerates instances, the running
  process ignores it, and it keeps an extra level in every path. A build variant is something every
  delivery path carries.
- **Keep the product's name a runtime value read from configuration**: it cannot name the
  directory the configuration is read from, and `uninstall` would need the file to find the
  service.
- **Derive the program's name from `PRODUCT_NAME`**: each variant would need its own published
  Package of the same bytes, because the archive member is extracted by name. **Name the program
  `agent`**: the system's word for every managed thing.
- **Keep the Linux layout under `/var/lib` and fix the label**: a persistent file context
  (`semanage fcontext` + `restorecon`) needs `policycoreutils-python-utils`, and a guarded fallback
  fails silently; `chcon` does not survive an autorelabel and puts SELinux tooling in the runtime
  path on every distribution; a shipped SELinux policy module is the heaviest answer (it stays
  possible if a confined domain is wanted; CIS Server Level 2 flags `unconfined_service_t`).
- **The layout under `/usr/lib` or `/usr/libexec`, or in `Program Files`**: the package manager's
  and the installer's hierarchies are theirs; a layout rewritten at runtime does not belong there,
  and on Windows it would need a privilege-escalating grant.
- **Everything, state included, under `/opt`**: parks variable data in `/opt` against FHS and
  gives back the `Program Files` problem on the other platform.
- **A separate `config init` command**: a second entry point and a second place resolving the
  path. **Interactive by default with `--non-interactive`** (Elastic Agent's shape): would hang
  every scripted install. **Overwrite with `--force`**: discards answers typed once and the edits
  made since. **`read_line` without a dependency**: re-implements validation, defaults and asking
  again for every question. **`inquire`, `cliclack`**: more than the questions need. **Ship an
  example configuration in the artifact**: it still has to be edited before the service does
  anything.
- **An installer that may write an unauthenticated or plaintext configuration**: the Client
  refuses that file at startup, so the installer would register a service that cannot start, and
  an answer without a client identity or a `ws://` default teaches the insecure setup as the normal
  one. **Treat `localhost` as loopback**: a name resolves through whatever the host's resolver
  says, so only the IP literal guarantees the traffic stays on the host.
- **Copy the bootstrap identity beside the configuration and let the service enrol on first
  start.** The install would not wait, but the identity would stay on the host after it has
  served, and the file would name a certificate that stops working within days. The enrolment is
  the install's (clause 26).
- **Take the state directory from the file, as long as it names an absolute one, else
  `<data root>/state`.** That is the escalation clause 14 closes: a file the service's account can
  rewrite would steer what an administrator's install acts on.
- **Take the file's value only beneath the roots, and `--state-dir` anywhere.** It closes the
  escalation, but a packaged upgrade passes no flag. A relocated state directory then either fails
  `apt upgrade` or is silently moved to `<data root>/state`, against clause 13.
- **Have the packages pass `--state-dir`.** The post-install and the MSI do not know it. They would
  have to read it from somewhere an administrator wrote, and the registration already is that
  place.
- **Keep the state directory in a root-owned file of its own.** That is a second record of what the
  registration already says. The two can disagree, and every platform needs a place for it.
- **Resolve both paths (`canonicalize`) and compare.** That follows the links the account can plant
  beneath the roots, which is exactly what must not decide.
- **Refuse a `supervisor_dir` outside the roots.** That contradicts the relocatable Supervisors'
  root, and the documented reason for the key is to place it outside. On Unix the install never
  acts on it anyway.
- **Ask the service manager for the loaded unit** (`systemctl show`, `launchctl print`). A drop-in
  or a hand edit would then steer the install, which is rejected for the account for the same
  reason.
- **The Windows Event Log**: an event source registered at install time, a message resource, and a
  code path only one platform exercises. **A file only on Windows**: makes "where are the logs"
  platform-dependent and leaves containers without one. **Rely on the OTLP own-logs bridge**
  ([ADR-0016](0016-what-an-agent-reports-about-itself.md)): needs a reachable Server, so it cannot
  explain the failures that prevent one. **Size-based rotation**: `tracing-appender` does not
  offer it. **`keep = 0` as unlimited**: the setting that fills a disk. **Remotely configurable
  logging**: the Client's own configuration stays on the machine. **The log in the version
  directory**: a self-update would scatter it and a rollback take it away.
- **Keep a product name such as `opamp-fleet-client` for the type, the package and the program.**
  The type would repeat a product name the row already carries as its instance name, and the fleet
  could not say what the Client *is*.
- **A reverse FQDN type, `io.opamp-fleet.supervisor`.** A recommendation enforced nowhere else
  here, in a table column where the short form is what gets read.
- **Rename only some layers.** A package name that is not the type splits the self-update's one
  rule into two names; a release named after the product does not fit the type a Client reports; a
  program named differently from its type is the doubled vocabulary this avoids.
- **A transitional release carrying both program names.** Re-creates one file with two names in the
  packer, the layout, the loader and the documentation, and needs a second decision about when to
  withdraw the old one.
- **The package owns the whole install** (a shipped unit, `/etc` configuration, an MSI
  `ServiceInstall`). Forks the install path per platform and puts the self-update in conflict with
  the package manager over the same files; a fleet update reverted by `apt upgrade` is worse than
  no package.
- **`nfpm`** for `.deb` and `.rpm`. A good tool and a close call; a second ecosystem's binary to
  pin, restating the metadata the cargo subcommands read from `Cargo.toml`. Worth reconsidering
  when a third format is wanted.
- **`cargo-wix`.** The `.wxs` is hand-written either way for a custom dialog and action, and it
  predates WiX v6. A WiX Burn bundle is awkward in Intune, Group Policy and SCCM.
- **An MSI endpoint default in both sequences.** Pins every silent install to the loopback.
- **Keep `http://localhost:4320/v1/opamp`, or prefill `http://127.0.0.1` for HTTP polling.** The
  first is plaintext to a host name, which the Client refuses at startup; the second is plaintext
  to a Server whose Agent plane answers only TLS. `https://127.0.0.1` would work, but a second
  value beside the loader's default is one more thing to keep from drifting, and the two are
  pinned by test either way.
- **Rename the dpkg/rpm identity or `UpgradeCode` with the program.** Costs every host a second
  product beside the first.
- **Name the build variables for this project (`OPAMP_FLEET_PRODUCT_NAME`, …)**: a build of
  `acme-agent` would set a variable that names a product the build is not.
- **No prefix (`PRODUCT_NAME`, `PRODUCT_TITLE`)**: common in other toolchains; Xcode exports
  `PRODUCT_NAME` to every build phase, and a stray slug would pass the grammar of clause 2 and
  silently rename the service and move the install path. `OPAMP_` names the protocol, not the
  product, so it holds in every variant.

## Sources / Prior art

- Specification Mission, Strategy and G-11 ([`docs/SPECIFICATION.md`](../SPECIFICATION.md));
  Strategy "Security before convenience" and Q-1 "Secure by default".
- Loopback names:
  [RFC 6761 section 6.3](https://www.rfc-editor.org/rfc/rfc6761#section-6.3) (`localhost`
  resolution left to the resolver) and
  [draft-ietf-dnsop-let-localhost-be-localhost](https://datatracker.ietf.org/doc/draft-ietf-dnsop-let-localhost-be-localhost/).
- [`service-manager`](https://docs.rs/service-manager/) and its
  [changelog](https://docs.rs/crate/service-manager/latest/source/CHANGELOG.md) (`RestartPolicy`,
  launchd `install` not auto-starting, `ServiceInstallCtx.username` honoured for systemd and
  launchd only); the launchd status bug
  <https://github.com/chipsenkbeil/service-manager-rs/issues/41>.
- [`windows-service`](https://docs.rs/windows-service/), used by Vector
  (<https://github.com/vectordotdev/vector/pull/2896>) and Mullvad; Windows error 1053
  (<https://learn.microsoft.com/en-us/answers/questions/1389851/>);
  [`SERVICE_STATUS`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_status);
  service names
  ([`GetServiceKeyName`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-getservicekeynamea)).
- Versioned directories and a pointer: Elastic Agent's `<component>-<version>-<hash>` directories
  and upgrade design (<https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md>);
  Scoop's `current` junction
  (<https://github.com/ScoopInstaller/Scoop/wiki/The-'Current'-Version-Alias>); the Chromium
  updater (<https://chromium.googlesource.com/chromium/src/+/main/docs/updater/design_doc.md>);
  atomic symlink replacement
  (<https://blog.moertel.com/posts/2005-08-22-how-to-change-symlinks-atomically.html>).
- systemd [unit names](https://man7.org/linux/man-pages/man5/systemd.unit.5.html),
  [kill](https://man7.org/linux/man-pages/man5/systemd.kill.5.html),
  [daemon(7)](https://man7.org/linux/man-pages/man7/daemon.7.html) on `SIGHUP`,
  [`Restart=on-failure`](https://man7.org/linux/man-pages/man5/systemd.service.5.html); launchd
  (<https://www.launchd.info/>, launchd.plist(5)); Telegraf's Windows service
  (<https://github.com/influxdata/telegraf/blob/master/docs/WINDOWS_SERVICE.md>); the
  `WaitToKillServiceTimeout` budget
  (<https://kb.firedaemon.com/support/solutions/articles/4000086193-increasing-service-shutdown-time>);
  [clap derive](https://docs.rs/clap/latest/clap/_derive/index.html).
- `systemd.service(5)`, "Command lines": quoting, `%%` and `$$` in `ExecStart=`. The Client writes
  each word in double quotes; the reader of the state directory is held to that writer by tests
  rather than to the whole grammar. `launchd.plist(5)`: `ProgramArguments` is an array of strings.
  Microsoft, "Parsing C command-line arguments" and `CommandLineToArgvW`: how the SCM's binary
  path name splits into arguments; the `service-manager` crate quotes it that way, and
  `QueryServiceConfigW` returns it. These were consulted from the documentation as recalled, not
  re-fetched; the readers are pinned by tests against what the Client itself writes.
- CWE-59 (link following) and CWE-269 (improper privilege management): the classes clauses 14 and
  15 close.
- SELinux: `status=203/EXEC` (<https://thomaspowell.com/2026/04/03/the-selinux-203-exec-systemd/>,
  <https://github.com/actions/runner/issues/1606>); `unconfined_service_t`
  (<https://danwalsh.livejournal.com/70577.html>,
  <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/selinux_users_and_administrators_guide/sect-security-enhanced_linux-targeted_policy-unconfined_processes>,
  CIS: <https://access.redhat.com/solutions/6714611>); openSUSE Leap 16.0 enforcing by default
  (<https://doc.opensuse.org/release-notes/x86_64/openSUSE/Leap/16.0/html/release-notes-leap-160/index.html>);
  FHS 3.0 `/opt` (<https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch03s13.html>); Elastic Agent
  in `/opt/Elastic/Agent` (<https://www.elastic.co/docs/reference/fleet/installation-layout>).
- Microsoft's `Program Files` / `ProgramData` guidance; Windows Installer `UpgradeCode` semantics
  (`MajorUpgrade`, `FindRelatedProducts`, `RemoveExistingProducts`); `cargo-deb` variants and
  `cargo-generate-rpm --set-metadata` for differently named packages from one tree; Elastic Agent
  and Telegraf ship one product per package, multiple instances being multiple installations.
- Service accounts: [Service User
  Accounts](https://learn.microsoft.com/en-us/windows/win32/services/service-user-accounts),
  [virtual accounts](https://docs.delinea.com/online-help/privilege-manager/install/upgrades/virtual-accounts.htm),
  [`sc config`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/sc-config),
  [`ChangeServiceConfig`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfiga);
  [Elastic Agent unprivileged mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged).
- First configuration: [Elastic Agent command
  reference](https://www.elastic.co/docs/reference/fleet/agent-command-reference)
  (`install` prompting, `--non-interactive`, `--force` overwriting, the two behaviours inverted
  here); [`dialoguer`](https://crates.io/crates/dialoguer);
  [`inquire`](https://crates.io/crates/inquire);
  [Rust CLI prompts compared](https://fadeevab.com/comparison-of-rust-cli-prompts/);
  [`std::io::IsTerminal`](https://doc.rust-lang.org/stable/std/io/trait.IsTerminal.html);
  [machine communication](https://rust-cli.github.io/book/in-depth/machine-communication.html),
  [isatty](https://blog.jez.io/cli-tty/); the OpenTelemetry `opampsupervisor`
  (<https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>),
  which has no scaffolding command.
- Kubernetes `kubeadm join`: a joining node takes the CA and a bootstrap token, and waits for its
  first certificate before it serves.
  <https://kubernetes.io/docs/reference/setup-tools/kubeadm/kubeadm-join/>
- Logging: [Elastic Agent
  logging](https://www.elastic.co/docs/reference/fleet/elastic-agent-standalone-logging-config)
  (logs under its data directory, size rotation);
  [OpenTelemetry Collector #5300](https://github.com/open-telemetry/opentelemetry-collector/issues/5300)
  (file logging for a Windows service, the Event Log output reported unreliable);
  [`tracing-appender`](https://crates.io/crates/tracing-appender).
- [Elastic Agent install
  documentation](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent)
  — native packages beside archives, and archives as what its fleet upgrades from.
- [`cargo-deb`](https://github.com/kornelski/cargo-deb) and its
  [systemd notes](https://github.com/kornelski/cargo-deb/blob/main/systemd.md);
  [`cargo-generate-rpm`](https://github.com/cat-in-136/cargo-generate-rpm);
  [`cargo-wix`](https://crates.io/crates/cargo-wix).
- [WiX Toolset](https://github.com/wixtoolset/wix), the
  [`wix` .NET tool](https://www.nuget.org/packages/wix),
  [WixUI dialog library](https://docs.firegiant.com/wix/tools/wixext/wixui/),
  [`WixUI_InstallDir`](https://documentation.help/WiX-Toolset/WixUI_installdir.html),
  [adding a dialog to a stock set](https://github.com/orgs/wixtoolset/discussions/8075),
  [customizing built-in dialog sets](https://docs.firegiant.com/wix3/wixui/wixui_customizations/).
- Windows Installer: [Public
  Properties](https://learn.microsoft.com/en-us/windows/win32/msi/public-properties),
  [Custom Action Type 18](https://learn.microsoft.com/en-us/windows/win32/msi/custom-action-type-18),
  [deferred execution custom actions](https://learn.microsoft.com/en-us/windows/win32/msi/deferred-execution-custom-actions);
  type 51 actions in `InstallUISequence` do not run under `/qn`.
- [Debian Policy, `init.d` scripts and
  services](https://www.debian.org/doc/debian-policy/ch-opersys.html#system-run-levels-and-init-d-scripts)
  — the enable-and-start convention the installers depart from.
- [rpm scriptlet ordering](https://docs.fedoraproject.org/en-US/packaging-guidelines/Scriptlets/)
  — the new `%post` runs before the old files are erased; `%posttrans` runs last.
- [FHS 3.0 `/usr/libexec`](https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch04s07.html) —
  binaries run by other programs, off `PATH`; Debian's `alternatives`: what users invoke is a link
  that is repointed.
- [OpAMP specification,
  `AgentDescription`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md),
  [opamp-spec issue #131](https://github.com/open-telemetry/opamp-spec/issues/131) and the
  [resource semantic conventions, `service.name`](https://opentelemetry.io/docs/specs/semconv/resource/#service).
- Xcode Build Settings Reference, `PRODUCT_NAME`: exported to the environment of every Run Script
  build phase. Recalled, not re-verified.
- `std::env::current_exe`: the path of the running executable as the operating system reports it;
  on Linux it resolves symbolic links. <https://doc.rust-lang.org/std/env/fn.current_exe.html>

## Consequences

- Positive: one binary installs, controls and deregisters itself as a native service on all three
  platforms, from any location, and still runs in the foreground. A service stop sends the OpAMP
  goodbye. A self-update stages a version and switches `current` without re-registering.
- Positive: the layout says what it is (`/opt/opamp-fleet/current/supervisor`, the product, then
  the program). An illegal name fails the build, not the host. One Package updates every variant
  build. An MSI host and an archive host put the same things in the same places.
- Positive: the `.rpm` produces a service that starts on Fedora, RHEL and SUSE 16 with SELinux
  enforcing, with no labelling step to keep alive.
- Positive: a fresh host goes from a binary to a working, registered service in one command, and no
  secret is typed into the installer at all. A Windows service failure becomes readable, and the
  logs cover failures own-telemetry structurally cannot.
- Positive: the first configuration a fresh host gets authenticates and encrypts beyond the
  loopback; an install never registers a service the Client would then refuse to start.
- Positive: no file the service's account can write moves what an administrator's install acts on
  (Q-1). An upgrade keeps the state directory the registration holds, including a relocated one,
  and an operator relocates it once, with `--state-dir`.
- Positive: one word for one thing, from the Agent type in the fleet view to the file an operator
  edits.
- Positive: first contact is one command per platform (`apt install`, `dnf install`, a double
  click) and removal is real. The installers are wrappers around the one install path, and a
  self-update never fights the package manager.
- Negative / trade-offs: a Windows-only runtime path Unix never exercises; system scope needs root
  or Administrator; launchd `status` is advisory and its `install` does not start; real
  registration runs only in the ignored smoke test and a manual checklist.
- Negative / trade-offs: the Linux system install spans two directories, and `--data-root` is the
  flag that must be right where getting it wrong is silent: `--root` alone on an enforcing host
  roots the binary wherever the operator chose. A host mounting `/opt` `noexec` breaks, loudly.
- Negative / trade-offs: a second installation needs a variant build, a minted `UpgradeCode` and a
  package-name entry; a forgotten `UpgradeCode` fails on a Windows host by removing the other
  installation. Two constants hold `supervisor` for different reasons, and conflating them would
  split the fleet's Packages.
- Negative / trade-offs: a configuration cannot be generated without installing; the questionnaire
  is a second place following the configuration schema, for the keys it asks. The log duplicates
  the journal on Linux and macOS, is bounded by days and not bytes, and keeps a week of whatever
  the Client logs on disk, so nothing secret may be logged.
- Negative / trade-offs: on a host not yet enrolled, an install that enrols needs the Server
  reachable, its enrolment window open and an operator approving the request while the command
  waits. A host that cannot reach the Server at install time is completed by editing the file.
- Negative / trade-offs: a host whose file was written with a credential keeps that `[auth]`
  section, secret included, until an operator deletes it: neither `install` nor the Client
  rewrites the file.
- Negative / trade-offs: a host whose file names a state directory outside the roots while its
  registration names another keeps the registration's, silently apart from the printed state
  directory. A first installation whose file names such a directory is refused until the operator
  names it with `--state-dir`. On Windows a Supervisors' root outside the roots must be created by
  the operator. A state directory beneath the layout root goes with it when a Linux package is
  removed, the identity included; the data root keeps it. A registration is kept wherever it
  points: one that an install wrote from a file the service's account had rewritten stays until an
  administrator names another with `--state-dir`, and the install warns of a registered state
  directory outside the roots on every re-registration. The install reads three command-line
  formats.
- Negative: `supervisor` names three things: the specification's Supervisor inside a Client, the
  Agent type, and the program. Documentation never uses the bare word where a file or unit is
  meant. It is also a common word: Debian's `supervisor` package installs
  `supervisord`/`supervisorctl` and does not collide, but the margin is thin.
- Negative: `dpkg -l` and `rpm -q` report the *delivered* version, which diverges from the running
  one after the first self-update; `opamp-fleet --version` and the fleet view are the truth, and
  the manual must say so.
- Negative: `apt install` leaves a stopped service, which a Debian user does not expect; a
  click-through MSI install on a production host gets a loopback configuration rather than a
  warning, and the prefill reaches a development Server only when that Server presents a
  certificate the Client trusts for `127.0.0.1`.
- Negative: three more build tools, one of them .NET, and no code signature on the installers:
  Windows shows an unknown publisher and `rpm` reports no signature. The `UpgradeCode` cannot be
  revisited without stranding installed hosts.
- Follow-ups: the service smoke test excludes hosts with SELinux or AppArmor in the way, so nothing
  automated exercises clause 12's reason, and the manual checklist covers it; whether `uninstall`
  should offer to remove the configuration and log it created; a size cap beside the day count;
  remotely raising a Client's log level, as a decision of its own; apt and yum repositories; Linux
  endpoint preseeding; a macOS `.pkg`; a `PATH` entry through `current` on Windows, where the
  delivered payload and the running binary drift the same way; how variant builds would be
  published and named beside the shared archive stem.

## Enforcement

- `crates/fleet-agent/build.rs` fails the build on a `PRODUCT_NAME` outside the grammar or a blank
  display name, and `product.rs` reads `OPAMP_PRODUCT_NAME` and `OPAMP_PRODUCT_TITLE` through
  `env!`; `crates/fleet-agent/src/product.rs` `product_name_satisfies_the_grammar`,
  `display_name_is_prose` (clause 2).
- `crates/fleet-agent/src/service/manager.rs`: `every_backend_renders_the_same_name`,
  `the_service_is_named_after_the_product_not_the_program`, `the_service_name_carries_no_suffix`,
  `the_names_a_human_reads`, `the_installed_command_line_is_the_marker_plus_absolute_paths`,
  `both_platforms_restart_after_the_same_delay`,
  `the_default_root_is_one_level_named_after_the_product`,
  `the_linux_system_layout_executes_from_opt`, `no_other_platform_splits`.
- `crates/fleet-agent/src/cli.rs`: `instance_is_not_a_flag_any_more`, `both_roots_can_be_named`,
  `service_verbs_parse_with_scope_and_root`, `the_installed_command_line_parses`,
  `install_is_not_interactive_unless_asked`, `bare_invocation_has_no_subcommand`,
  `config_takes_the_install_roots` (clause 23).
- `crates/fleet-agent/src/service/layout.rs`:
  `the_directory_name_is_base_plus_hash_never_the_prerelease`,
  `set_current_points_and_repoints`, `stage_writes_binary_manifest_and_pointer`,
  `restaging_identical_bytes_leaves_the_staged_binary_untouched`,
  `restaging_replaces_a_staged_binary_with_different_bytes`,
  `a_torn_pointer_is_healed_a_correct_one_left_alone`,
  `the_layout_is_found_from_the_pointer_the_service_was_registered_against` (clause 6, and the
  layout of [ADR-0020](0020-the-accounts-the-service-runs-under.md)).
- `crates/fleet-agent/src/service/run_as.rs`: `windows_forms_are_the_passwordless_ones`,
  `a_missing_account_is_refused_with_the_way_out`,
  `the_handover_walks_the_tree_and_skips_what_is_missing` (the account of
  [ADR-0020](0020-the-accounts-the-service-runs-under.md)).
- `crates/fleet-agent/src/config_init.rs`: `an_existing_file_is_never_overwritten`,
  `an_endpoint_given_never_overwrites_an_existing_file`,
  `an_edit_changes_the_asked_keys_and_keeps_the_rest`, `the_defaults_are_what_the_file_holds`,
  `interactive_without_a_terminal_fails_instead_of_blocking`,
  `the_file_is_not_readable_by_the_rest_of_the_machine`,
  `the_rendered_file_loads_as_what_was_answered`,
  `a_bad_endpoint_is_refused_before_anything_is_written` (clauses 23–26);
  `a_complete_answer_writes_a_file_the_client_starts_with`, which renders the answers without a
  client identity and the verification key as answered, and passes
  `ClientConfig::check_admission` once the state directory holds an issued certificate
  (clause 26); `installer_answers_alone_fail_validation_and_stay_on_disk` (clause 26).
- `crates/fleet-agent/src/config_file.rs`: `admission_needs_a_client_identity_and_nothing_else`,
  which asserts that `ClientConfig::check_admission` passes a file with a client identity and no
  `[auth]` and refuses one without an identity, naming `[tls] cert_file` and `key_file`
  (clauses 26, 27).
- The tests of the first configuration's questions, `--endpoint`, the post-install's printed steps
  and the MSI's dialogs and actions, which hold what
  [ADR-0022](0022-bootstrap-certificates-and-enrolment.md) and
  [ADR-0020](0020-the-accounts-the-service-runs-under.md) carry:
  `the_bootstrap_pair_is_asked_only_before_enrolment`,
  `a_private_ca_is_only_asked_about_where_tls_applies`,
  `the_suggested_endpoint_is_tls_on_the_loopback`,
  `a_plaintext_endpoint_is_accepted_only_on_a_loopback_literal`,
  `the_answered_ca_is_copied_beside_the_configuration`, `a_rendered_file_has_no_auth_section`,
  `an_endpoint_given_is_written_and_loads`, `an_endpoint_is_validated_by_the_loaders_own_rule` in
  [`config_init.rs`](../../crates/fleet-agent/src/config_init.rs);
  `install_takes_an_endpoint_without_a_terminal` and
  `an_endpoint_and_interactive_are_refused_together` in
  [`cli.rs`](../../crates/fleet-agent/src/cli.rs);
  `a_leftover_auth_section_is_ignored_with_a_notice` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs), shared with
  [ADR-0021](0021-admission-by-client-certificate-and-revocation.md);
  `the_questionnaires_enrolment_stores_the_issued_certificate_and_copies_nothing` in
  [`tests/enrolment_e2e.rs`](../../crates/fleet-agent/tests/enrolment_e2e.rs);
  `the_post_install_prints_no_endpoint_the_client_refuses_at_startup` and
  `the_post_install_steps_ask_for_the_client_identity` in
  [`tests/linux_post_install.rs`](../../crates/fleet-agent/tests/linux_post_install.rs);
  `register_service_with_endpoint_survives_the_crt`, `register_service_survives_the_crt`,
  `stop_and_unregister_survive_the_crt` and
  `the_endpoint_dialog_names_the_identity_and_no_credential` in
  [`tests/msi_exe_command.rs`](../../crates/fleet-agent/tests/msi_exe_command.rs).
- Logging: `crates/fleet-agent/tests/logging.rs` (`a_service_run_writes_a_log_file`,
  `a_foreground_run_writes_no_log_file`); `crates/fleet-agent/src/config.rs`
  `the_log_file_is_on_by_default_and_its_retention_is_not_optional`;
  `crates/fleet-agent/src/logging.rs` `the_writer_discards_until_a_file_is_opened`,
  `the_log_directory_hangs_off_the_state_directory` (clauses 19–21).
- `crates/fleet-agent/tests/shutdown.rs` `sigterm_shuts_the_client_down_cleanly`;
  `crates/fleet-agent/tests/service_smoke.rs`
  `the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop`, run with
  `--ignored` by the `service-smoke` workflow on an ephemeral runner (clauses 17, 18).
- [`tests/msi_exe_command.rs`](../../crates/fleet-agent/tests/msi_exe_command.rs)
  `the_msi_names_no_root_so_no_directory_property_reaches_a_command_line` (clause 12);
  `endpoint_prefill_is_the_development_server_and_interactive_only`, which asserts
  `wss://127.0.0.1:4320/v1/opamp` and passes it through `ClientConfig::transport`, the endpoint
  rule the Client applies at startup (clause 30). The MSI is not built locally; this test reads
  `packaging/windows/supervisor.wxs` as text on every platform, so the prefill is checked by
  `cargo test` wherever it runs, not by building or installing the MSI.
- The `.rpm` scriptlet assertions in
  [`.github/workflows/release.yml`](../../.github/workflows/release.yml) require the removal to
  take the layout root and the purge the data root (clause 12); "The RPM carries its scriptlets,
  not their file names" asserts with `rpm -qp --scripts` that `service install`,
  `service uninstall`, `ln -sfn`, the layout-root and data-root removals are in the package, and
  that the RPM's architecture is the ecosystem's (clauses 12, 31).
- [`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs):
  `the_clients_own_agent_type_is_the_one_name_this_program_has`,
  `the_clients_own_agent_reports_its_type_and_its_configured_name_separately` (clauses 4, 6, 7).
- [`config.rs`](../../crates/fleet-agent/src/config.rs):
  `the_configurations_old_name_beside_the_new_one_is_refused_rather_than_defaulted` (clause 8).
- The install's enrolment: a non-interactive install with a bootstrap identity beside the program
  enrols and stores the issued certificate; an enrolled Client does not enrol again (clause 26).
- The state directory, by the tests that cite this ADR: a re-registration without `--state-dir`
  keeps the registered state directory whatever the file says; a file value outside the roots is
  refused when nothing is registered, and so is one reached through `..` or a link; `--state-dir`
  is taken anywhere; the systemd, launchd and SCM command lines the Client writes are read back to
  the state directory they carry; a Supervisors' root outside the roots is left untouched by the
  Windows plan; `service install` through the real command line refuses before it has created
  anything (clauses 13–16).

**Not mechanically decidable:** that the layout starts under an enforcing SELinux policy: no CI
runner enforces it, so the manual checklist in [`README.md`](../../README.md) carries that step.
`service install` (clauses 26, 27) validates only on the way to registering a service, so the
warnings it prints and its load of an existing file are not run by any test; the rules it applies
are `ClientConfig::load` and `ClientConfig::check_admission`, exercised above. The questionnaire's
prompts read a terminal, which `cargo test` does not have: that no answer skips the bootstrap
identity on a Client not yet enrolled, that no question asks for a credential, that a CA,
certificate or key path naming no readable file and a refused endpoint are asked for again, and
that an empty verification key is named, are review questions on `ask` in `config_init.rs`.

**Not mechanically decidable:** what `dpkg`, `rpm` and Windows Installer do with the maintainer
scripts and the MSI tables on a real host (the upgrade fall-through, the rpm `%posttrans`
ordering, the dialog sequence, the `UpgradeCode`'s permanence) is exercised only by installing a
release; no CI job installs the packages. That the installer packages put their program where no
bootstrap identity lies is held by review.
