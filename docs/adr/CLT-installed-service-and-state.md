# CLT-installed-service-and-state: The installed service and its state

- **Status:** 🟢 accepted
- **Date:** 2026-10-23
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-agent/src/cli.rs`, `crates/fleet-agent/src/main.rs`, `crates/fleet-agent/src/service/` (the state directory a re-registration reads in `manager.rs`, the directories a system-scope install restricts in `windows_rights.rs`), `crates/fleet-agent/src/config_init.rs`, `ClientConfig::service_state_dir` in `crates/fleet-agent/src/config_file.rs`, `crates/fleet-agent/src/logging.rs`, `crates/fleet-agent/src/product.rs`, `crates/fleet-agent/build.rs`, the `[package.metadata.deb]` and `[package.metadata.generate-rpm]` tables of `crates/fleet-agent/Cargo.toml`, `packaging/linux/`, `packaging/windows/`, the `state_dir` and `supervisor_dir` comments of `config/supervisor.toml`, the program, Agent type and configuration-file names of the Client, `service install`, `service config`, and every path or name an installed Client uses and every document that tells an operator where an installed Client keeps its state

## Context

The specification commits the Client to install as a native operating-system service that updates
itself in place, on Linux, macOS and Windows (Mission, Strategy, G-11). A fleet client must run
without a logged-in user and start at boot. It must survive the service restart a self-update
causes ([CLT-self-update-signed-package §8](CLT-self-update-signed-package.md)). It must explain
itself when it fails on a host nobody watches.

The specification also puts security before convenience (Strategy *Security before convenience*,
Q-1 *Secure by default*). The first configuration an installer writes must authenticate and must
not send plaintext beyond the loopback. Plaintext is accepted on the loopback alone, a host name is
never the loopback, and the Client refuses at startup to run without a client certificate. The
Agent plane admits a peer by its client certificate alone
([SEC-admission-by-client-certificate §1](SEC-admission-by-client-certificate.md)). There is
therefore no fleet credential for an installer to ask for, write or name.

Forces:

- **The three service models differ.** systemd and launchd supervise an ordinary foreground
  process. The Windows Service Control Manager (SCM) launches the process and expects status
  reports within about 30 seconds, or kills it with error 1053. A process cannot reliably detect an
  SCM launch, so the registered command line must carry a marker. The SCM discards a service's
  stderr.
- **What the service points at decides how an update works.** Registering the binary's own path
  would force a re-registration on every update, and Windows locks a running `.exe`. A stable
  pointer into side-by-side version directories makes an update a pointer switch. The layout itself
  is [CLT-accounts-service-runs-under §6](CLT-accounts-service-runs-under.md)'s.
- **The daemon itself rewrites the layout at run time.** The layout must therefore stand where the
  platform lets the service execute it *and* where the service's own account may write it.
  - On Fedora, RHEL and openSUSE Leap 16 / SLES 16, the SELinux targeted policy is enforcing by
    default. systemd (`init_t`) may not execute a file labelled `var_lib_t`. A binary under
    `/var/lib` installs, registers, and dies at its first start with `status=203/EXEC`. The AVC
    denial behind it never appears in the unit's journal, and the same binary runs fine from a
    shell.
  - On Windows, `TrustedInstaller` owns `Program Files`, which is meant to be read-only once
    installation finishes.
- **A name that identifies an installation has to exist before the installation is read.** It
  names the directory the configuration is read from. `service uninstall` must find the service
  without that file. The name must at once be a systemd unit name, a launchd label, an SCM service
  name and a directory name on every platform.
- **Nothing ships the configuration.** A release artifact is the bare binary. The Client loads
  defaults when its file is absent
  ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)). A service installed without a
  file therefore starts, dials the development default, and manages nothing, silently. `install`
  is also the command an Ansible play, an MDM profile or an MSI invokes: a prompt there does not
  fail, it hangs. A first configuration names a client identity: files that must already be on the
  host, or that an enrolment obtains
  ([SEC-bootstrap-certificates-and-enrolment §18](SEC-bootstrap-certificates-and-enrolment.md)).
- **`service install` runs as root, or as an Administrator on Windows.** The Linux post-install
  and the MSI run it on every upgrade. `supervisor.toml` belongs to the service's account, which
  may rewrite it. The install acts on the state directory with full rights:
  - on Windows it cuts the directory off from inherited rights and grants the account Modify on it;
  - on Linux and macOS it hands the directory over by a recursive change of ownership;
  - it writes the enrolment's files into it as root.

  If the state directory came from the file, anything that can rewrite the file can name any tree:
  another service's data, a user's profile, a system directory. That includes an account the Server
  reaches and anything that compromises the Client. The next upgrade would hand that tree to the
  service's account: a privilege escalation from that account to arbitrary file access, on every
  platform. A Server cannot write those keys, since it delivers `[[supervisor]]` blocks only
  ([CLT-server-manages-the-supervisor-set §1](CLT-server-manages-the-supervisor-set.md)). A
  compromised service account can. The registration (`/etc/systemd/system/<PRODUCT_NAME>.service`,
  `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`, or the SCM's entry) is what the Client wrote as
  root, and only an administrator can change it.
- **A Supervisors' root is relocatable**, on purpose: it keeps programs off a `noexec` or
  undersized `/var`.

An operator also needs something to install. A host needs what its platform expects: for `apt` and
`dnf`, a package with an inventory, an upgrade path and a clean removal; on Windows, an `.msi`, the
only thing Intune, Group Policy and SCCM distribute. `service install` carries the install: the
layout, the `current` pointer, and the registration with its single-token name, display name and
Windows recovery actions. A native package that registered a service of its own would write a
second registration missing all of that. Self-update and a package manager must not both hold the
same bytes. If `dpkg`, `rpm` or the MSI held the layout, every fleet update would leave the host in
a state `dpkg -V` reports as modified, and the next `apt upgrade` would silently revert it. The
releases that carry these packages are
[OPS-releases-attestations-server-image](OPS-releases-attestations-server-image.md)'s.

One force bears on the name. The Baseline reserves `service.name` for the Agent *type* and
recommends a reverse FQDN. This project does not enforce that recommendation
([CLT-what-an-agent-reports §1](CLT-what-an-agent-reports.md)), and opamp-spec issue 131 records
it as an overload of that key. Among Collectors, Foreign Agents and the process that supervises
them, the useful type for the last is the role it plays. What the fleet offers the thing is named
after what the thing is, and so is the thing itself.

## Decision

We will make the Client the program `supervisor`, a `clap` subcommand CLI that registers, controls
and runs itself as a native service on systemd, launchd and the Windows SCM under a product name
fixed at build time, from a layout kept apart from its data on Linux at system scope, that takes
an installed Client's state directory from `--state-dir`, else its registration, else
`supervisor.toml` only beneath the install's roots, writes its first configuration when asked and
never one the Client would refuse at startup, logs to a bounded file when it runs as a service,
and is delivered by `.deb`, `.rpm` and `.msi` packages that run its own `service install`.

### The program and its names

1. **One CLI, thin, over a `service` module.**
   - The subcommands are `run` and `service install | config | uninstall | start | stop |
     status`. `run` is the foreground daemon, and the default when no subcommand is given.
   - The global flags are `--config` (default `supervisor.toml`) and `--state-dir`. Every service
     verb takes `--user`.
   - There is no configuration by environment variable. The Client is file-configured
     ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)). A registered service carries
     the configuration's *path*, never its content.
   - clap's `version` is wired to `fleet_core::version::current()`
     ([OPS-cargo-decides-product-version §5](OPS-cargo-decides-product-version.md)).
   - The daemon code lives in `crates/fleet-agent/src/service/`: `runtime`, `layout`, `manager`,
     `run_as`, `enrol`, and the Windows-only `windows`, `windows_config`, `windows_rights` and
     `withdrawal`.
   - The code sits behind the narrow `ServiceControl` seam (`start`, `stop`, `state`) that the
     self-update depends on.
   - Errors are `Result<_, String>`.

2. **`PRODUCT_NAME` is a build-time constant.**
   - Its default is **`opamp-fleet`**. A variant build may override it with `OPAMP_PRODUCT_NAME`.
   - `crates/fleet-agent/build.rs` validates it against the intersection of the systemd-unit,
     launchd-label, SCM-name and directory-name grammars:
     - lowercase `[a-z0-9-]`, 1–32 characters;
     - no leading or trailing `-`;
     - never a Windows reserved device name: `con`, `prn`, `aux`, `nul`, `com1`–`com9`,
       `lpt1`–`lpt9`.
   - The build **fails** on a name that breaks the grammar, so an illegal name cannot reach a
     service manager.
   - The display name is the constant `PRODUCT_TITLE`, default **`OpAMP Fleet Agent`**. A variant
     build may override it with `OPAMP_PRODUCT_TITLE`. It must only be non-blank.
   - The display name is prose. No rule derives it from the slug so that it reads correctly for the
     next variant.
   - It appears only where prose belongs: the Add/Remove Programs entry, the SCM's display column
     and the installer dialog's title.
   - It is not the Agent type, so the fleet view never prints one word in both columns
     ([CLT-what-an-agent-reports §11](CLT-what-an-agent-reports.md)).

3. **Three names sit side by side, and none is derived from another.**

   | Name | Names | Appears in |
   |---|---|---|
   | `PRODUCT_NAME` (`opamp-fleet`) | the **product** | the install path, the service, the `.deb`/`.rpm` package, the `/usr/libexec/<PRODUCT_NAME>/` and `Program Files\<PRODUCT_NAME>` payload directories, the `PATH` symlink |
   | `supervisor` (`layout::COMPONENT`, `BINARY_FILENAME`) | the **program** | the file, its configuration `supervisor.toml`, the version directories, the archive member |
   | `supervisor` (`CLIENT_AGENT_TYPE`) | the **Agent type** | `service.name` on the wire, the Package's key |

   The program's and the Agent type's names are those of clauses 6 and 4. They share a string and
   are separate constants. The program's name stays off `PRODUCT_NAME`. That lets **one published
   Package update every variant build**: the archive member a self-update extracts is the same in
   all of them.

4. **The Agent type is `supervisor`.** It is the constant `CLIENT_AGENT_TYPE` that every Client
   reports as `service.name` ([`agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs)). It
   is the same on every host, because every Client in a fleet is the same kind of thing.

5. **The package that carries the Client is `supervisor` too.**
   - The self-update takes only the package named after the Agent type
     ([CLT-self-update-signed-package §2](CLT-self-update-signed-package.md)).
   - A Server offers a package under the Agent type it is built for
     ([PKG-signed-packages-and-delivery §9](PKG-signed-packages-and-delivery.md)).
   - The release a fleet uploads for its Clients is therefore a Package for Agent type
     `supervisor`. No per-host setting is needed.
   - A Configuration carrying the Client's `[[supervisor]]` blocks is typed `supervisor` as well
     ([CLT-server-manages-the-supervisor-set §1](CLT-server-manages-the-supervisor-set.md)).

6. **The program is `supervisor`**, `supervisor.exe` on Windows. The name covers:
   - the binary Cargo builds (`[[bin]] name = "supervisor"` in the package `fleet-agent`);
   - the file in every version directory (`supervisor-<MAJOR.MINOR.PATCH>-<hash>`);
   - the member a package artifact carries;
   - the payload file under `/usr/libexec/<PRODUCT_NAME>/`;
   - the log file, the self-check token and the CLI's own name.

   The configuration file is **`supervisor.toml`**, and so is the `--config` default.

   The product's name, not the program's, names what identifies an *installation*:
   - the install path;
   - the service ([CLT-accounts-service-runs-under §17](CLT-accounts-service-runs-under.md));
   - the dpkg/rpm package;
   - the `PATH` symlink (clauses 2 and 3).

   The program's name is not derived from the product's. One published Package therefore serves
   every variant build: the member a self-update extracts is the same in all of them.

   Two names deliberately stay where they are:
   - the Cargo package `fleet-agent`, a build-time identifier that never leaves the repository;
   - the OTLP instrumentation scope `opamp-fleet-client`, which names the library a signal came
     from. Renaming it would move every operator's dashboards.

7. **The instance name is a separate attribute, and its default is `Supervisor Agent`.**
   - The top-level `name` is the operator's name for *this* Client. The Client reports it as
     `service.instance.name` ([CLT-what-an-agent-reports §2](CLT-what-an-agent-reports.md)).
   - Its default is a display name, with spaces and capitals. It is deliberately not `supervisor`:
     a default equal to the type would print the same word in both columns of the fleet view.
   - Nothing resolves a path or a service from this key.

8. **One name per thing, with no alias.**
   - No artifact carries the program twice.
   - No version directory holds a compatibility link.
   - The loader reads no configuration file but the one it is given.
   - **A Client that finds no `supervisor.toml` at its configured path, but a `client.toml` beside
     it, refuses to start.** It names both paths and the `mv` that fixes it. Coming up on defaults
     there, dialling the development endpoint and managing nothing, is the one outcome nobody would
     see.
   - The self-update refuses an artifact whose member is not `supervisor`. The host stays on the
     version it runs.

9. **A second installation is a second build.**
   - A variant has its own `PRODUCT_NAME`. It therefore has its own service name, package name,
     `/usr/libexec` payload directory, `PATH` symlink and MSI `UpgradeCode`.
   - The `UpgradeCode` is the one identity that cannot be derived. Each variant mints one GUID once
     and records it. Windows Installer treats a shared `UpgradeCode` as an instruction to remove the
     other installation.
   - An installation is an isolation boundary: separate Server, client identity, lifecycle and
     rollback.
   - Scaling the number of *managed* Agents is the multiplexing inside one installation
     ([CLT-modes-and-gateway](CLT-modes-and-gateway.md)).
   - `service.instance.name` tells two variants on one host apart
     ([CLT-what-an-agent-reports §2](CLT-what-an-agent-reports.md)).

10. **There is no instance flag.**
    - `--instance` is not a flag at all: not hidden, and not accepted and ignored. A command line
      carrying it fails to parse.
    - No registered service carries it.
    - `service uninstall | start | stop | status` take no name. With one service per build there is
      nothing to look up.
    - The name grammar of clause 2 is also the grammar of `[[supervisor]]` block names
      (`config::parse_instance_name`).

### Where an installation lives

11. **The install path is `<base>/<PRODUCT_NAME>`, one level, never fixed.** The product names it,
    never its display name.

    | Platform, scope | Executable layout | Data |
    |---|---|---|
    | Linux, system | `/opt/<PRODUCT_NAME>` | `/var/lib/<PRODUCT_NAME>` |
    | Linux, user | `$XDG_DATA_HOME/<PRODUCT_NAME>` (else `~/.local/share/…`) | same |
    | macOS, system | `/Library/Application Support/<PRODUCT_NAME>` | same |
    | macOS, user | `~/Library/Application Support/<PRODUCT_NAME>` | same |
    | Windows, system | `%ProgramData%\<PRODUCT_NAME>` | same |
    | Windows, user | `%LOCALAPPDATA%\<PRODUCT_NAME>` | same |

    The MSI's payload directory is `C:\Program Files\<PRODUCT_NAME>`. It is not the layout
    (clause 12).

12. **Program and data split only on Linux at system scope, and only where the platform gives a
    reason.**
    - **Linux:** the executable layout is under `/opt`.
      - The default file context of `/opt` is `usr_t`. It is an entrypoint type through which
        systemd starts a third-party service in `unconfined_service_t`.
      - Every version the self-update stages later inherits that label. Staging therefore needs no
        SELinux tooling, no policy module and no new runtime dependency.
      - A label applied once at install time would expire at the next staged version or relabel.
      - FHS 3.0 places add-on software under `/opt` and its variable data under `/var`, so the
        split is the standard layout.
      - The data root stays in `/var/lib`.
    - **Windows does not split.**
      - The MSI delivers its payload to `Program Files\<PRODUCT_NAME>`. It then runs the same
        `service install` the command line would. That builds the layout and the data under
        `%ProgramData%\<PRODUCT_NAME>`.
      - The layout must not be put in `Program Files`. The service's own account must be able to
        write it ([CLT-accounts-service-runs-under §1](CLT-accounts-service-runs-under.md)).
        Granting a low-privileged account modify rights on a tree `TrustedInstaller` holds, and
        that the account executes from, is a surface for privilege escalation.
      - The line is the one the Linux packages draw between the `/usr/libexec/<PRODUCT_NAME>/`
        payload the package holds and the `/opt` layout the program holds
        ([CLT-accounts-service-runs-under §4](CLT-accounts-service-runs-under.md)).
      - The MSI passes no root flag, so no directory property reaches a command line. An MSI host
        and an archive host put the same things in the same places.
    - **Two flags.**
      - `--root` names the layout root. Given **alone**, it collapses layout and data into that one
        directory. Its labelling and permissions are then the operator's business.
      - `--data-root` names the data root. Given alone, it moves only the data.
      - A packaged install passes neither and takes the platform defaults.
    - **What a package removal takes.**
      - On **remove**: the layout root and the `PATH` symlink. They hold nothing but staged
        binaries.
      - On **purge**: additionally the data root, with the configuration and the identity in it.
      - On Windows the MSI's uninstaller empties only `INSTALLFOLDER`, and the data under
        `%ProgramData%` survives. That is the same shape of remove versus purge.
      - An install rooted elsewhere with `--root` is manual, and no package touches it.

13. **An upgrade on an installed host is a re-registration, never a migration.**
    - `--config` keeps pointing into the data root. `--state-dir` keeps pointing where the
      registration has it (clause 14).
    - A re-registration rewrites the program path (`ExecStart`) and the rest of the unit or plist
      from the version that runs it. That includes the systemd unit's directives
      ([CLT-accounts-service-runs-under §7](CLT-accounts-service-runs-under.md)).
    - The account stays the one the registration names
      ([CLT-accounts-service-runs-under §1](CLT-accounts-service-runs-under.md)).
    - The instance's configuration, identity and state never move.
    - A change that would move the data root or the state directory of an installed host must bring
      its own migration. It carries the full weight of this rule.
    - An administrator who names another `--state-dir` moves nothing. The service then starts on an
      empty state directory and enrols anew.

14. **The state directory is taken in this order.**
    1. **`--state-dir`**, the global flag (clause 1), given to `service install` or
       `service config` and made absolute.
    2. **The registration's**, at system scope: the value after `--state-dir` in the command line
       of the registration the Client wrote.
       - Under systemd it is `ExecStart=` of `/etc/systemd/system/<PRODUCT_NAME>.service`.
       - Under launchd it is `ProgramArguments` of `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`.
       - Under the SCM it is the service's binary path name, read with `QueryServiceConfig`.
       - The install reads what the Client wrote. It never reads a drop-in or the manager's loaded
         state. The account a registration names is read the same way
         ([CLT-accounts-service-runs-under §1](CLT-accounts-service-runs-under.md)).
       - A registration that names no `--state-dir` offers none, and so does every user-scope
         registration. A user scope has no administrator between the file and the service.
    3. **The file's**: the absolute `state_dir` that `supervisor.toml` names, and only beneath the
       data root or the layout root (clause 15). A relative one names none.
    4. **`<data root>/state`.**

    `service config` finds the state directory by the same order, as clause 23 asks of it.

15. **Beneath a root means beneath it as spelled, through directories only.**
    - A directory is beneath a root when its path starts with the root's path, compared component
      by component as spelled. Nothing is resolved, so another spelling of the root is not beneath
      it.
    - Below the root, no component may be `.` or `..`.
    - Every component below the root that exists must be a directory, and not a symbolic link or
      junction. What does not exist yet ends the check.
    - `--root` and `--data-root`, or the platform default, name the roots themselves. This check
      does not inspect them. Whether a root may itself be a link is a question of its own, which
      this clause leaves open.
    - The check holds for a state directory from any source that lies beneath one of the roots. The
      service's account owns what is under them.
    - The install refuses a state directory from the file that fails the check. The refusal names
      the directory, why, and `--state-dir`. It comes before the install writes anything.

16. **A Supervisors' root outside the roots is the operator's.**
    - A `supervisor_dir` outside the data root, the layout root and the state directory stays as
      relocatable as the Supervisors' directories are made
      ([CLT-accounts-service-runs-under §24](CLT-accounts-service-runs-under.md)).
    - A system-scope install on Windows never creates, restricts or hands over such a root. It
      leaves the root as it is and names it in a warning.
    - On Linux and macOS an install never touches a Supervisors' root.

### The service

17. **The lifecycle runs over `service-manager`, system scope by default.**
    - System scope is a systemd system unit, a launchd `LaunchDaemon` or an SCM service. A fleet
      client runs without a logged-in user and starts at boot.
    - `--user` is a development opt-in. The Client refuses it on Windows, which has no user-scope
      services.
    - **The manager restarts the service on failure, never after an explicit stop.** It restarts
      after 5 seconds, with no retry limit. This lets the self-update stop the service and switch
      `current` without the manager racing it.
      - systemd and launchd carry the policy natively (`Restart=on-failure`,
        `KeepAlive{SuccessfulExit:false}`).
      - The crate's `sc.exe` backend discards the policy. `windows_config` therefore sets SCM
        recovery actions with the same delay and enables them for non-crash failures.
      - `windows_config` also sets the display name (`sc config`) and the Description
        (`sc description`).
    - **A system-scope install that lacks the rights fails before it writes anything.** On Windows
      it probes the SCM for `CREATE_SERVICE` first, and its message says so.
    - **On launchd, `install` does not start the service** and says so, printing the follow-up
      `service start`. launchd `status` is advisory.
    - **`uninstall` deregisters only.** It never deletes the layout, the configuration or the
      state.

18. **Under the manager the daemon is the same `run` loop.**
    - On Linux and macOS it is a plain foreground process.
    - `SIGTERM` and `SIGINT` shut it down gracefully, through the transports. The OpAMP
      `agent_disconnect` goodbye is thus sent on a service stop too, well within launchd's
      20-second `ExitTimeOut`.
    - The Client explicitly ignores `SIGHUP` rather than let it terminate the process.
    - On Windows a `cfg(windows)` shim on the `windows-service` crate registers the control
      handler. It reports `StartPending` → `Running` → `StopPending` → `Stopped`, and runs the
      identical body.
    - The hidden `run --service` marker routes into the SCM dispatcher on Windows. On every
      platform it says that no terminal is watching (clause 19).

19. **A Client running as a service writes its own log to a rotating file, on every platform.**
    - `run --service` writes the file. A foreground run writes stderr alone, because somebody is
      reading it.
    - The Client writes the file on Linux and macOS too, although the journal already holds stderr
      there. "Where are the logs" thus has one answer everywhere, a container included.
    - The file lives in `<state dir>/logs/` as `supervisor.<date>.log`. The state directory
      survives self-updates and `uninstall`, so a log explaining a failed start is still there
      afterwards.

20. **The log is bounded, and `[logging]` is the machine's.**
    - The log rotates daily and keeps a fixed number of days, 7 by default.
    - `[logging]` in `supervisor.toml` takes `dir` (moves the file), `keep` (days kept) and
      `enabled = false` (off, for an operator whose platform collects stderr).
    - The Client refuses `keep = 0` at load rather than read it as "forever". An unknown key fails
      startup.
    - The bound is on age and file count, not bytes. The Client is quiet, and it backs off when it
      cannot reach the Server.
    - The section never arrives over the wire. A Server able to redirect or silence a Client's log
      would be able to hide its own effects
      ([SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md)).

21. **The file never costs the Client its run.**
    - `tracing` takes one subscriber per process, installed before the command line is parsed.
      The file layer is therefore installed from the start, with a writer that discards until the
      run opens the file. Events before that still reach stderr.
    - `tracing-appender` (from `tokio-rs/tracing`, beside `tracing-subscriber`) does the rolling,
      non-blocking.
    - The Client reports a directory it cannot write, or a full disk, once. It then runs without
      the file.

### The first configuration

22. **Without `--config`, the configuration is `<data root>/supervisor.toml`.**
    - One rule holds for every platform.
    - The absolute path registered is the one just written.
    - An explicit `--config` wins. The flag has a default value, so clap's value source tells the
      two cases apart, not a comparison with the default string.

23. **Interactivity is an opt-in flag on `install`, and `service config` is the questionnaire
    alone.**
    - `service install --interactive` runs the questionnaire.
    - Without the flag, `install` never asks anything, because it is the scripted command.
    - `service config` runs the same questionnaire against an installed Client and registers
      nothing.
    - `service config` finds the file as `install` does: `--config`, else
      `<data root>/supervisor.toml` from `--user`, `--root` and `--data-root`. It finds the state
      directory the way `install` derives it (clause 14).
    - The file must load before the questionnaire asks. An operator corrects by hand a file that
      does not load.
    - Either command with a stdin that is not a terminal is an **error**, not a fallback. A
      provisioning run must fail with a message rather than hang.

24. **A configuration file that exists is never overwritten, and the questionnaire edits it in
    place.**
    - No answer an installer supplies, and no `install` without `--interactive`, touches the file.
      `install` proceeds with that file and names it.
    - The questionnaire (`--interactive` and `service config` alike) offers the value the file
      holds as the default of every question.
    - The questionnaire changes only the keys it asks for. Every other key, every comment, the
      `[[supervisor]]` blocks and the self-update consent stay as written.
    - A re-install can thus never eat the answers typed into the first install, or the edits made
      to the file since.

25. **The written file is treated as holding a secret.**
    - On Unix its mode is `0600`.
    - On Windows it inherits the data root's ACL; at system scope, the administrators own it.
    - It holds no credential.
    - It may name where a client identity's private key lies.
    - It may hold `[packages] archive_key`, the fleet's archive decryption secret. An operator adds
      that key by editing the file the installer created.

26. **The configuration is validated before the service is registered, by the rules the Client
    applies at startup.**
    - The order is: write → load through `ClientConfig::load` → enrol → stage the layout →
      register. A broken configuration thus fails at install, not at the service's first start.
    - The install enrols when the questionnaire took a bootstrap identity or one lies beside the
      program
      ([SEC-bootstrap-certificates-and-enrolment §18](SEC-bootstrap-certificates-and-enrolment.md)).
    - A `ws://` or `http://` endpoint off the loopback fails the install, naming the setting.
    - A missing client identity is what the Client refuses at startup. The install names it in a
      warning and registers the service all the same. The service refuses to run until the file
      is complete, so nothing ever runs unauthenticated.
    - The install validates a file that already exists the same way.
    - The install leaves on disk a written file that fails to load, and names it in the error.
    - An interrupted enrolment leaves the written file and registers nothing.

27. **A non-interactive install with no configuration file warns.** It is not an error, because
    automation must not break. The install prints a line naming the path registered. The line says
    that the Client refuses to start until that file exists with a client identity.

28. **The prompts come from `dialoguer`.**
    - It validates an answer and asks again, and offers a default and a yes/no confirmation, the
      same way on three operating systems.
    - `dialoguer` is MIT-licensed and brings no TLS or crypto backend.
    - The questionnaire asks for no secret, so no prompt hides its input.
    - The terminal check itself is `std::io::IsTerminal`, with no dependency.

### The installers

29. **Three classes of release file, and only one is a package artifact.**
    - The Client opens the Client's archive on a self-update, and the fleet view parses it.
    - The operator tool's archives
      ([OPS-operator-program-fleetctl](OPS-operator-program-fleetctl.md)) are downloaded to an
      operator's machine and are no package of the fleet.
    - `dpkg`, `rpm` and Windows Installer open the `.deb`, `.rpm` and `.msi`. Nothing in the fleet
      path opens them.

    The installers are **additive operator artifacts**. They never replace the archive, which is
    the only format the fleet can install.

30. **The endpoint dialog is prefilled with the development Server, interactively only.**
    - A `SetProperty` in the UI sequence sets `wss://127.0.0.1:4320/v1/opamp`. It is conditioned on
      `ENDPOINT` unset and the product not installed.
    - A silent install that names no `ENDPOINT` still writes no configuration. An unattended
      install thus never acquires the development default by omission.
    - A value given on the command line wins.
    - Clearing the field is the answer "configure later".
    - The value is the loader's default verbatim.
    - `http://localhost` does not qualify, on two counts:
      - the Agent plane always serves TLS, so a development Server answers no plaintext request;
      - `localhost` is a host name, which is never the loopback. Only the IP literals `127.0.0.1`
        and `::1` are, so the Client would refuse the value at startup.
    - The scheme selects the transport
      ([SEC-tls13-two-planes-bounded-input §2](SEC-tls13-two-planes-bounded-input.md)). The
      click-through install therefore runs WebSocket, as an unconfigured Client would.
    - Like every value an installer writes, the prefill passes the rule the Client applies at
      startup
      ([SEC-bootstrap-certificates-and-enrolment §17](SEC-bootstrap-certificates-and-enrolment.md)).

31. **What builds the installers.** Additional steps in the release's `build` matrix job build
    them. The steps run after the archive is packed and consume the binary that job built. The
    bytes in a `.deb` are thus provably the bytes in the archive.

    | | tool | runs on |
    |---|---|---|
    | `.deb` | `cargo-deb`, `--no-build --no-strip` | both Linux runners, natively |
    | `.rpm` | `cargo-generate-rpm`, no `rpmbuild` needed | both Linux runners, natively |
    | `.msi` | WiX Toolset 6 as the `wix` .NET tool, with its UI extension | the Windows runner |

    - Package metadata lives in `crates/fleet-agent/Cargo.toml` under `[package.metadata.deb]` and
      `[package.metadata.generate-rpm]`. It reads the workspace's version, licence and description.
    - `cargo-deb` derives `Depends` from what the binary links (`$auto`).
    - The RPM `Release` is `1` and stays `1`. No epoch and no `~` mangling are needed, because a
      version is released once and is always `MAJOR.MINOR.PATCH`
      ([OPS-cargo-decides-product-version](OPS-cargo-decides-product-version.md)).
    - The maintainer scripts live in `packaging/linux/`, the WiX sources in `packaging/windows/`.

**Out of scope:**

- what [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md) decides:
  - the versioned layout and its `current` pointer;
  - the service's name and the account it runs under;
  - what the Linux packages deliver and remove;
  - the MSI's dialogs and actions;
  - that no installer starts the service it registers;
- what [CLT-self-update-signed-package](CLT-self-update-signed-package.md) decides: staging,
  verifying, rolling back and pruning versions, and the self-update consent;
- what [SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md)
  decides: the questions of the first configuration, `service install --endpoint` and what lies
  beside the program;
- whether variant builds are ever published, and where their `UpgradeCode`s are recorded;
- how an operator discovers what is installed on a host;
- a confined SELinux policy module;
- configuration reload on `SIGHUP`;
- `Type=notify` integration;
- the startup rules themselves, and the Server's enrolment window and approval, which the installer
  only applies and names
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md));
- hosting apt and yum repositories;
- preseeding the endpoint on Linux;
- a macOS `.pkg`, which needs a Developer ID and notarization;
- the files an enrolment writes as root into the state directory, which no privileged step writes
  through a link ([CLT-accounts-service-runs-under §11](CLT-accounts-service-runs-under.md));
- the `[tls]` files an enrolment reads as root;
- `[logging] dir`, which only the service's own run uses;
- the releases and the Server's image
  ([OPS-releases-attestations-server-image](OPS-releases-attestations-server-image.md)).

## Alternatives considered

- **Separate installers or OS packages that hold the service** (shipping a unit file, wrapping
  `sc.exe`). It spreads the logic across artifacts and gives two registrations that disagree. The
  packages ship the binary and call `service install`.
- **Hand-write the three backends.** Three code paths to maintain. `service-manager` abstracts
  exactly install, start, stop, status and uninstall, and the wrapper stays thin enough to finish
  what it omits (`windows_config`).
- **A daemonizing crate or double-fork.** Unix-only, and init systems want to supervise a
  foreground process.
- **Register the binary's own path instead of `current`.** Every update would re-register the
  service with administrator rights, and Windows locks the running `.exe`.
- **Configuration by environment variable in the unit.** A second, diverging source of truth.
- **`anyhow`.** The crate uses `Result<_, String>` throughout.
- **A runtime instance flag.** No package can pass it, nothing enumerates instances, the running
  process ignores it, and it keeps an extra level in every path. A build variant is something every
  delivery path carries.
- **Keep the product's name a runtime value read from configuration.** It cannot name the
  directory the configuration is read from, and `uninstall` would need the file to find the
  service.
- **Derive the program's name from `PRODUCT_NAME`.** Each variant would need its own published
  Package of the same bytes, because the archive member is extracted by name.
- **Name the program `agent`.** It is the system's word for every managed thing.
- **Keep the Linux layout under `/var/lib` and fix the label.**
  - A persistent file context (`semanage fcontext` + `restorecon`) needs
    `policycoreutils-python-utils`, and a guarded fallback fails silently.
  - `chcon` does not survive an autorelabel, and puts SELinux tooling in the runtime path on every
    distribution.
  - A shipped SELinux policy module is the heaviest answer. It stays possible if a confined domain
    is wanted; CIS Server Level 2 flags `unconfined_service_t`.
- **The layout under `/usr/lib` or `/usr/libexec`, or in `Program Files`.** Those hierarchies are
  the package manager's and the installer's. A layout rewritten at run time does not belong there,
  and on Windows it would need a grant that escalates privilege.
- **Everything, state included, under `/opt`.** It parks variable data in `/opt` against FHS and
  gives back the `Program Files` problem on the other platform.
- **A separate `config init` command.** A second entry point and a second place resolving the
  path.
- **Interactive by default with `--non-interactive`** (Elastic Agent's shape). It would hang every
  scripted install.
- **Overwrite with `--force`.** It discards answers typed once and the edits made since.
- **`read_line` without a dependency.** It re-implements validation, defaults and asking again for
  every question.
- **`inquire`, `cliclack`.** More than the questions need.
- **Ship an example configuration in the artifact.** It still has to be edited before the service
  does anything.
- **An installer that may write an unauthenticated or plaintext configuration.** The Client refuses
  that file at startup, so the installer would register a service that cannot start. An answer
  without a client identity, or a `ws://` default, teaches the insecure setup as the normal one.
- **Treat `localhost` as loopback.** A name resolves through whatever the host's resolver says.
  Only the IP literal guarantees that the traffic stays on the host.
- **Copy the bootstrap identity beside the configuration and let the service enrol on first
  start.** The install would not wait. But the identity would stay on the host after it has served,
  and the file would name a certificate that stops working within days. The enrolment is the
  install's (clause 26).
- **Take the state directory from the file, as long as it names an absolute one, else
  `<data root>/state`.** That is the escalation clause 14 closes: a file the service's account can
  rewrite would steer what an administrator's install acts on.
- **Take the file's value only beneath the roots, and `--state-dir` anywhere.** It closes the
  escalation, but a packaged upgrade passes no flag. A relocated state directory would then either
  fail `apt upgrade` or be silently moved to `<data root>/state`, against clause 13.
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
  or a hand edit would then steer the install. The account is not read that way, for the same
  reason.
- **The Windows Event Log.** An event source registered at install time, a message resource, and a
  code path only one platform exercises.
- **A log file only on Windows.** It makes "where are the logs" depend on the platform and leaves
  containers without one.
- **Rely on the OTLP bridge of the Client's own logs**
  ([CLT-what-an-agent-reports §16](CLT-what-an-agent-reports.md)). It needs a reachable Server, so
  it cannot explain the failures that prevent one.
- **Size-based rotation.** `tracing-appender` does not offer it.
- **`keep = 0` as unlimited.** The setting that fills a disk.
- **Remotely configurable logging.** The Client's own configuration stays on the machine.
- **The log in the version directory.** A self-update would scatter it, and a rollback would take
  it away.
- **A product name such as `opamp-fleet-client` for the type, the package and the program.** The
  type would repeat a product name the row already carries as its instance name, and the fleet
  could not say what the Client *is*.
- **A reverse FQDN type, `io.opamp-fleet.supervisor`.** A recommendation enforced nowhere else
  here, in a table column where the short form is what gets read.
- **Rename only some layers.** A package name that is not the type splits the self-update's one
  rule into two names. A release named after the product does not fit the type a Client reports. A
  program named differently from its type is the doubled vocabulary this decision avoids.
- **A transitional release carrying both program names.** It re-creates one file with two names in
  the packer, the layout, the loader and the documentation. It also needs a second decision about
  when to withdraw the old name.
- **The package holds the whole install** (a shipped unit, configuration under `/etc`, an MSI
  `ServiceInstall`). It forks the install path per platform and sets the self-update against the
  package manager over the same files. A fleet update reverted by `apt upgrade` is worse than no
  package.
- **`nfpm`** for `.deb` and `.rpm`. A good tool and a close call, but a second ecosystem's binary to
  pin, restating the metadata the cargo subcommands read from `Cargo.toml`. Worth reconsidering
  when a third format is wanted.
- **`cargo-wix`.** The `.wxs` is hand-written either way for a custom dialog and action, and the
  tool predates WiX v6. A WiX Burn bundle is awkward in Intune, Group Policy and SCCM.
- **An MSI endpoint default in both sequences.** It pins every silent install to the loopback.
- **`http://localhost:4320/v1/opamp`, or `http://127.0.0.1` for HTTP polling, as the prefill.** The
  first is plaintext to a host name, which the Client refuses at startup. The second is plaintext to
  a Server whose Agent plane answers only TLS. `https://127.0.0.1` would work, but a second value
  beside the loader's default is one more thing to keep from drifting. Tests pin the two either way.
- **Rename the dpkg/rpm identity or the `UpgradeCode` with the program.** It costs every host a
  second product beside the first.
- **Build variables named for this project (`OPAMP_FLEET_PRODUCT_NAME`, …).** A build of
  `acme-agent` would set a variable that names a product the build is not.
- **No prefix (`PRODUCT_NAME`, `PRODUCT_TITLE`).** Common in other toolchains, but Xcode exports
  `PRODUCT_NAME` to every build phase. A stray slug would pass the grammar of clause 2 and silently
  rename the service and move the install path. `OPAMP_` names the protocol, not the product, so it
  holds in every variant.

## Sources / Prior art

- Specification Mission, Strategy and G-11 ([`docs/SPECIFICATION.md`](../SPECIFICATION.md));
  Strategy *Security before convenience* and Q-1 *Secure by default*.
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
  (<https://www.launchd.info/>, launchd.plist(5)); the OpenTelemetry Collector's MSI, which
  installs one Windows service named after the distribution
  (<https://opentelemetry.io/docs/collector/install/binary/windows/>); the
  `WaitToKillServiceTimeout` budget
  (<https://kb.firedaemon.com/support/solutions/articles/4000086193-increasing-service-shutdown-time>);
  [clap derive](https://docs.rs/clap/latest/clap/_derive/index.html).
- `systemd.service(5)`, "Command lines": quoting, `%%` and `$$` in `ExecStart=`. The Client writes
  each word in double quotes. Tests hold the reader of the state directory to that writer, not to
  the whole grammar. `launchd.plist(5)`: `ProgramArguments` is an array of strings. Microsoft,
  "Parsing C command-line arguments" and `CommandLineToArgvW`: how the SCM's binary path name
  splits into arguments. The `service-manager` crate quotes it that way, and `QueryServiceConfigW`
  returns it. These were consulted from the documentation as recalled, not re-fetched. Tests pin
  the readers against what the Client itself writes.
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
  ships one product per package, multiple instances being multiple installations.
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
- Kubernetes `kubeadm join`: a joining machine takes the CA and a bootstrap token, and waits for
  its first certificate before it serves.
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
- Positive: the layout says what it is (`/opt/opamp-fleet/current/supervisor`: the product, then
  the program). An illegal name fails the build, not the host. One Package updates every variant
  build. An MSI host and an archive host put the same things in the same places.
- Positive: the `.rpm` produces a service that starts on Fedora, RHEL and SUSE 16 with SELinux
  enforcing, with no labelling step to keep alive.
- Positive: a fresh host goes from a binary to a working, registered service in one command, and no
  secret is typed into the installer at all. A Windows service failure becomes readable, and the
  logs cover failures the Client's own telemetry structurally cannot.
- Positive: the first configuration a fresh host gets authenticates and encrypts beyond the
  loopback. An install never registers a service the Client would then refuse to start.
- Positive: no file the service's account can write moves what an administrator's install acts on
  (Q-1). An upgrade keeps the state directory the registration holds, a relocated one included. An
  operator relocates it once, with `--state-dir`.
- Positive: one word for one thing, from the Agent type in the fleet view to the file an operator
  edits.
- Positive: first contact is one command per platform (`apt install`, `dnf install`, a double
  click), and removal is real. The installers are wrappers around the one install path, and a
  self-update never fights the package manager.
- Negative / trade-offs: a Windows-only runtime path that Unix never exercises. System scope needs
  root or Administrator. launchd `status` is advisory, and its `install` does not start the
  service. Real registration runs only in the ignored smoke test and a manual checklist.
- Negative / trade-offs: the Linux system install spans two directories. `--data-root` is the flag
  that must be right where getting it wrong is silent: `--root` alone on an enforcing host roots the
  binary wherever the operator chose. A host that mounts `/opt` `noexec` breaks, loudly.
- Negative / trade-offs: a second installation needs a variant build, a minted `UpgradeCode` and a
  package-name entry. A forgotten `UpgradeCode` fails on a Windows host by removing the other
  installation. Two constants hold `supervisor` for different reasons, and conflating them would
  split the fleet's Packages.
- Negative / trade-offs: a configuration cannot be generated without installing. The questionnaire
  is a second place that follows the configuration schema, for the keys it asks. The log repeats
  the journal on Linux and macOS, is bounded by days and not bytes, and keeps on disk a week of
  whatever the Client logs. Nothing secret may therefore be logged.
- Negative / trade-offs: on a host not yet enrolled, an install that enrols needs the Server
  reachable, its enrolment window open, and an operator approving the request while the command
  waits. A host that cannot reach the Server at install time is completed by editing the file.
- Negative / trade-offs: a host whose file was written with a credential keeps that `[auth]`
  section, secret included, until an operator deletes it. Neither `install` nor the Client rewrites
  the file.
- Negative / trade-offs: around the state directory:
  - A host whose file names a state directory outside the roots, while its registration names
    another, keeps the registration's. Only the printed state directory shows it.
  - A first installation whose file names such a directory is refused until the operator names it
    with `--state-dir`.
  - On Windows the operator must create a Supervisors' root outside the roots.
  - A state directory beneath the layout root goes with it when a Linux package is removed, the
    identity included. The data root keeps it.
  - A registration is kept wherever it points. One that an install wrote from a file the service's
    account had rewritten stays until an administrator names another with `--state-dir`. The
    install warns of a registered state directory outside the roots on every re-registration.
  - The install reads three command-line formats.
- Negative: `supervisor` names three things: the specification's Supervisor inside a Client, the
  Agent type, and the program. Documentation never uses the bare word where a file or unit is
  meant. It is also a common word. Debian's `supervisor` package installs
  `supervisord`/`supervisorctl` and does not collide, but the margin is thin.
- Negative: `dpkg -l` and `rpm -q` report the *delivered* version, which diverges from the running
  one after the first self-update. `opamp-fleet --version` and the fleet view are the truth, and
  the manual must say so.
- Negative: `apt install` leaves a stopped service, which a Debian user does not expect. A
  click-through MSI install on a production host gets a loopback configuration rather than a
  warning. The prefill reaches a development Server only when that Server presents a certificate
  the Client trusts for `127.0.0.1`.
- Negative: three more build tools, one of them .NET, and no code signature on the installers.
  Windows shows an unknown publisher, and `rpm` reports no signature. The `UpgradeCode` cannot be
  revisited without stranding installed hosts.
- Follow-ups:
  - The service smoke test excludes hosts with SELinux or AppArmor in the way. Nothing automated
    exercises the reason of clause 12, and the manual checklist covers it.
  - Whether `uninstall` is to offer to remove the configuration and log it created.
  - A size cap beside the day count.
  - Remotely raising a Client's log level, as a decision of its own.
  - apt and yum repositories; Linux endpoint preseeding; a macOS `.pkg`.
  - A `PATH` entry through `current` on Windows, where the delivered payload and the running binary
    drift the same way.
  - How variant builds would be published and named beside the shared archive stem.

## Enforcement

- The product names (clause 2):
  - `crates/fleet-agent/build.rs` fails the build on a `PRODUCT_NAME` outside the grammar or a
    blank display name.
  - `product.rs` reads `OPAMP_PRODUCT_NAME` and `OPAMP_PRODUCT_TITLE` through `env!`.
  - `crates/fleet-agent/src/product.rs`: `product_name_satisfies_the_grammar`,
    `display_name_is_prose`.
- `crates/fleet-agent/src/service/manager.rs` (clauses 3, 11, 12, 17):
  `every_backend_renders_the_same_name`,
  `the_service_is_named_after_the_product_not_the_program`, `the_service_name_carries_no_suffix`,
  `the_names_a_human_reads`, `the_installed_command_line_is_the_marker_plus_absolute_paths`,
  `both_platforms_restart_after_the_same_delay`,
  `the_default_root_is_one_level_named_after_the_product`,
  `the_linux_system_layout_executes_from_opt`, `no_other_platform_splits`.
- `crates/fleet-agent/src/cli.rs` (clauses 1, 10, 12, 23): `instance_is_not_a_flag_any_more`,
  `both_roots_can_be_named`, `service_verbs_parse_with_scope_and_root`,
  `the_installed_command_line_parses`, `install_is_not_interactive_unless_asked`,
  `bare_invocation_has_no_subcommand`, `config_takes_the_install_roots`.
- `crates/fleet-agent/src/service/layout.rs` (clause 6, and the layout of
  [CLT-accounts-service-runs-under §6](CLT-accounts-service-runs-under.md)):
  `the_directory_name_is_base_plus_hash_never_the_prerelease`,
  `set_current_points_and_repoints`, `stage_writes_binary_manifest_and_pointer`,
  `restaging_identical_bytes_leaves_the_staged_binary_untouched`,
  `restaging_replaces_a_staged_binary_with_different_bytes`,
  `a_torn_pointer_is_healed_a_correct_one_left_alone`,
  `the_layout_is_found_from_the_pointer_the_service_was_registered_against`.
- `crates/fleet-agent/src/service/run_as.rs` (the account of
  [CLT-accounts-service-runs-under §1](CLT-accounts-service-runs-under.md)):
  `windows_forms_are_the_passwordless_ones`, `a_missing_account_is_refused_with_the_way_out`,
  `the_handover_walks_the_tree_and_skips_what_is_missing`.
- `crates/fleet-agent/src/config_init.rs`:
  - clauses 23 to 26: `an_existing_file_is_never_overwritten`,
    `an_endpoint_given_never_overwrites_an_existing_file`,
    `an_edit_changes_the_asked_keys_and_keeps_the_rest`, `the_defaults_are_what_the_file_holds`,
    `interactive_without_a_terminal_fails_instead_of_blocking`,
    `the_file_is_not_readable_by_the_rest_of_the_machine`,
    `the_rendered_file_loads_as_what_was_answered`,
    `a_bad_endpoint_is_refused_before_anything_is_written`;
  - clause 26: `a_complete_answer_writes_a_file_the_client_starts_with`. It renders the answers
    without a client identity and with the verification key as answered. The file passes
    `ClientConfig::check_admission` once the state directory holds an issued certificate.
  - clause 26: `installer_answers_alone_fail_validation_and_stay_on_disk`.
- `crates/fleet-agent/src/config_file.rs` (clauses 26, 27):
  `admission_needs_a_client_identity_and_nothing_else`. It asserts that
  `ClientConfig::check_admission` passes a file with a client identity and no `[auth]`. It also
  asserts that the check refuses a file without an identity, naming `[tls] cert_file` and
  `key_file`.
- The tests below hold what
  [SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md) and
  [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md) carry: the first
  configuration's questions, `--endpoint`, the post-install's printed steps, and the MSI's dialogs
  and actions.
  - [`config_init.rs`](../../crates/fleet-agent/src/config_init.rs):
    `the_bootstrap_pair_is_asked_only_before_enrolment`,
    `the_server_ca_is_asked_for_where_tls_applies`,
    `the_suggested_endpoint_is_tls_on_the_loopback`,
    `a_plaintext_endpoint_is_accepted_only_on_a_loopback_literal`,
    `the_answered_ca_is_copied_beside_the_configuration`, `a_rendered_file_has_no_auth_section`,
    `an_endpoint_given_is_written_and_loads`, `an_endpoint_is_validated_by_the_loaders_own_rule`;
  - [`cli.rs`](../../crates/fleet-agent/src/cli.rs):
    `install_takes_an_endpoint_without_a_terminal`,
    `an_endpoint_and_interactive_are_refused_together`;
  - [`config.rs`](../../crates/fleet-agent/src/config.rs):
    `a_leftover_auth_section_is_ignored_with_a_notice`, shared with
    [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md);
  - [`tests/enrolment_e2e.rs`](../../crates/fleet-agent/tests/enrolment_e2e.rs):
    `the_questionnaires_enrolment_stores_the_issued_certificate_and_copies_nothing`;
  - [`tests/linux_post_install.rs`](../../crates/fleet-agent/tests/linux_post_install.rs):
    `the_post_install_prints_no_endpoint_the_client_refuses_at_startup`,
    `the_post_install_steps_ask_for_the_client_identity`;
  - [`tests/msi_exe_command.rs`](../../crates/fleet-agent/tests/msi_exe_command.rs):
    `register_service_with_endpoint_survives_the_crt`, `register_service_survives_the_crt`,
    `stop_and_unregister_survive_the_crt`,
    `the_endpoint_dialog_names_the_identity_and_no_credential`.
- Logging (clauses 19 to 21):
  - `crates/fleet-agent/tests/logging.rs`: `a_service_run_writes_a_log_file`,
    `a_foreground_run_writes_no_log_file`;
  - `crates/fleet-agent/src/config.rs`:
    `the_log_file_is_on_by_default_and_its_retention_is_not_optional`;
  - `crates/fleet-agent/src/logging.rs`: `the_writer_discards_until_a_file_is_opened`,
    `the_log_directory_hangs_off_the_state_directory`.
- The service (clauses 17, 18):
  - `crates/fleet-agent/tests/shutdown.rs`: `sigterm_shuts_the_client_down_cleanly`;
  - `crates/fleet-agent/tests/service_smoke.rs`:
    `the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop`. The
    `service-smoke` workflow runs it with `--ignored` on an ephemeral runner.
- [`tests/msi_exe_command.rs`](../../crates/fleet-agent/tests/msi_exe_command.rs):
  - clause 12: `the_msi_names_no_root_so_no_directory_property_reaches_a_command_line`;
  - clause 30: `endpoint_prefill_is_the_development_server_and_interactive_only`. It asserts
    `wss://127.0.0.1:4320/v1/opamp` and passes the value through `ClientConfig::transport`, the
    endpoint rule the Client applies at startup.
  - The MSI is not built locally. This test reads `packaging/windows/supervisor.wxs` as text on
    every platform. `cargo test` thus checks the prefill wherever it runs, without building or
    installing the MSI.
- The `.rpm` scriptlet assertions in
  [`.github/workflows/release.yml`](../../.github/workflows/release.yml) (clauses 12, 31):
  - They require the removal to take the layout root, and the purge the data root (clause 12).
  - The step "The RPM carries its scriptlets, not their file names" uses `rpm -qp --scripts`. It
    asserts that the package holds `service install`, `service uninstall`, `ln -sfn`, and the
    removals of the layout root and the data root. It also asserts that the
    RPM's architecture is the ecosystem's.
- [`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs) (clauses 4, 6, 7):
  `the_clients_own_agent_type_is_the_one_name_this_program_has`,
  `the_clients_own_agent_reports_its_type_and_its_configured_name_separately`.
- [`config.rs`](../../crates/fleet-agent/src/config.rs) (clause 8):
  `the_configurations_old_name_beside_the_new_one_is_refused_rather_than_defaulted`.
- The install's enrolment (clause 26):
  - `config_init.rs`: `an_unasked_install_enrols_with_the_pair_beside_the_program_until_enrolled`.
    It asserts that a non-interactive install with a bootstrap pair beside the program enrols. It
    also asserts that an enrolled Client does not enrol again.
  - `tests/service_smoke.rs`:
    `an_install_enrols_with_a_bootstrap_p12_and_the_service_runs_on_the_issued_certificate`. It
    stores the issued certificate and runs the service on it.
- The state directory (clauses 13 to 16):
  - `config_file.rs`:
    `a_reregistration_keeps_the_registered_state_directory_whatever_the_file_says`;
  - `config_file.rs`: `a_state_directory_the_file_names_outside_the_installs_roots_is_refused`. It
    also refuses one that leaves the roots by `..`, and takes `--state-dir` anywhere.
  - `config_file.rs`: `a_state_directory_reached_through_a_link_beneath_the_root_is_refused`;
  - `manager.rs`: `a_units_state_directory_is_read_back_from_its_exec_start`,
    `a_plists_state_directory_is_read_from_its_program_arguments`,
    `a_windows_command_lines_state_directory_is_split_as_the_runtime_splits_it`,
    `a_registration_on_disk_offers_the_state_directory_it_was_written_with`,
    `only_an_absolute_state_dir_after_the_flag_is_registered`;
  - `windows_rights.rs`: `a_supervisors_root_outside_the_installs_roots_is_left_untouched`;
  - `tests/install_roots.rs`:
    `an_install_refuses_a_state_directory_the_file_names_outside_its_roots_before_any_change`,
    through the real command line.

**Not mechanically decidable:** these need a host policy no CI runner enforces.

- That the layout starts under an enforcing SELinux policy. No CI runner enforces it, so the
  manual checklist in [`README.md`](../../README.md) carries that step.
- What `service install` prints (clauses 26, 27). It validates only on the way to registering a
  service, so no test runs its warnings or its load of an existing file. The rules it applies are
  `ClientConfig::load` and `ClientConfig::check_admission`, exercised above.
- The questionnaire's prompts. They read a terminal, which `cargo test` does not have. These are
  review questions on `ask` in `config_init.rs`:
  - that no answer skips the bootstrap identity on a Client not yet enrolled;
  - that no question asks for a credential;
  - that the questionnaire asks again for a CA, certificate or key path naming no readable file,
    and for a refused endpoint;
  - that the questionnaire names an empty verification key.
- What `dpkg`, `rpm` and Windows Installer do with the maintainer scripts and the MSI tables on a
  real host. That covers the upgrade fall-through, the rpm `%posttrans` ordering, the dialog
  sequence and the permanence of the `UpgradeCode`. Only installing a release exercises it, and no
  CI job installs the packages.
- That the installer packages put their program where no bootstrap identity lies. Review holds it.
