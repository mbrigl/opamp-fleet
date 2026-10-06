# ADR-0028: The Client installs itself as a native service named after a build-time product name, from a versioned layout it can rewrite, with a first configuration that authenticates and encrypts

- **Status:** 🟢 accepted
- **Date:** 2026-10-06
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-agent/src/cli.rs`, `crates/fleet-agent/src/main.rs`, `crates/fleet-agent/src/service/`, `crates/fleet-agent/src/config_init.rs`, `crates/fleet-agent/src/logging.rs`, `crates/fleet-agent/src/product.rs`, `crates/fleet-agent/build.rs`, and every path, name or account an installed Client uses

## Context

The Agent plane admits a peer by its client certificate alone
([ADR-0026](0026-admission-by-a-client-certificate-alone.md)), as the specification asks (Strategy
"Security before convenience", Q-1, the Gateway Mode paragraph and G-15). There is no fleet
credential, so the installer neither asks for one nor writes one, and a first configuration is
complete with the endpoint, the CA where the Server's certificate needs one, and a client identity.

The specification puts security before convenience (Strategy "Security before convenience", Q-1
"Secure by default"): the first configuration the installer writes must authenticate and must not
send plaintext beyond the loopback. Since [ADR-0026](0026-admission-by-a-client-certificate-alone.md)
the Client authenticates with one proof, the client certificate it presents in the TLS handshake.

The specification commits the Client to install as a native operating-system service that updates
itself in place, on Linux, macOS and Windows (Mission, Strategy, goals 10 and 11). A fleet client
must run without a logged-in user, start at boot, survive the service restart a self-update
causes, and explain itself when it fails on a host nobody watches.

Forces:

- **The three service models differ.** systemd and launchd supervise an ordinary foreground
  process. The Windows Service Control Manager (SCM) launches the process and expects status
  reports within about 30 seconds or kills it with error 1053, and a process cannot reliably detect
  an SCM launch, so the registered command line must carry a marker. The SCM discards a service's
  stderr.
- **What the service points at decides how an update works.** Registering the binary's own path
  would force a re-registration on every update, and Windows locks a running `.exe`. A stable
  pointer into side-by-side version directories makes an update a pointer switch.
- **The layout is rewritten at runtime by the daemon itself** — the self-update stages versions and
  swings the pointer while the service runs ([ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)). So the
  layout must stand where the platform lets the service execute it *and* where the service's own
  account may write it. On Fedora, RHEL and openSUSE Leap 16 / SLES 16 the SELinux targeted policy
  is enforcing by default, and systemd (`init_t`) may not execute a file labelled `var_lib_t`: a
  binary under `/var/lib` installs, registers, and dies at its first start with `status=203/EXEC`
  and an AVC denial the unit's journal never explains, while the same binary runs fine from a
  shell. On Windows, `Program Files` is `TrustedInstaller`-owned and meant to be read-only once
  installation finishes.
- **A name that identifies an installation has to exist before the installation is read.** It
  names the directory the configuration is read from, and `service uninstall` must find the service
  without that file. It must at once be a systemd unit name, a launchd label, an SCM service name
  and a directory name on every platform.
- **Nothing ships the configuration.** A release artifact is the bare binary
  ([ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)), and the Client loads defaults
  when its file is absent ([ADR-0025](0025-five-crates-a-publishable-communication-layer-and-toml-configuration-axum-without-its-websocket.md)) — so a service
  installed without one starts, dials the development default, and manages nothing, silently.
  `install` is also the command an Ansible play, an MDM profile or an MSI invokes: a prompt there
  does not fail, it hangs. And a first configuration names a client identity: a certificate and
  its private key, files that must already be on the host.
- **Least privilege** is an operator requirement: the Client, and every Managed Process its
  Supervisors spawn, may have to run under a dedicated account instead of root or `LocalSystem`.

## Decision

We will make the Client a `clap` subcommand CLI that registers, controls and runs itself as a
native service on systemd, launchd and the Windows SCM, under a product name fixed at build time
that names the service, the install path, the package and the `PATH` entry; that executes from a
versioned layout behind a `current` pointer, kept apart from its data on Linux at system scope;
that writes its first configuration when asked and never one the Client would refuse at startup,
logs to a bounded file when it runs as a service, and may run under an operator-named account
that owns what it rewrites.

1. **One CLI, thin, over a `service` module.** The subcommands are `run` — the foreground daemon,
   and the default when no subcommand is given — and `service install | uninstall | start | stop |
   status`. The global flags are `--config` (default `supervisor.toml`) and `--state-dir`; every
   service verb takes `--user`. There is no environment-variable configuration: the Client is
   file-configured ([ADR-0025](0025-five-crates-a-publishable-communication-layer-and-toml-configuration-axum-without-its-websocket.md)) and a registered
   service carries the configuration's *path*, never its content. clap's `version` is wired to
   `opamp::version::current()` ([ADR-0017](0017-versions-resolved-in-the-internal-crate.md) clause 5). The daemon code lives in
   `crates/fleet-agent/src/service/` (`runtime`, `layout`, `manager`, `run_as`, and the Windows-only
   `windows`, `windows_config`, `windows_rights`), behind the narrow `ServiceControl` seam
   (`start`, `stop`, `state`) that the self-update depends on. Errors are `Result<_, String>`.

2. **`PRODUCT_NAME` is a build-time constant.** Default **`opamp-fleet`**, overridable with
   `OPAMP_FLEET_PRODUCT_NAME` for a variant build. `crates/fleet-agent/build.rs` validates it against
   the intersection of the systemd-unit, launchd-label, SCM-name and directory-name grammars —
   lowercase `[a-z0-9-]`, 1–32 characters, no leading or trailing `-`, never a Windows reserved
   device name (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`) — and **fails the build**
   on a name that breaks it, so an illegal name cannot reach a service manager. The display name,
   default **`OpAMP Fleet Agent`**, is a second variable, `OPAMP_FLEET_PRODUCT_DISPLAY_NAME`, only
   required to be non-blank: it is prose, cannot be derived from the slug by any rule that would
   read correctly for the next variant, and appears only where prose belongs — the Add/Remove
   Programs entry, the SCM's display column and the installer dialog's title. It is not the Agent
   type, so the fleet view never prints one word in both columns
   ([ADR-0012](0012-what-an-agent-reports-about-itself.md)).

3. **The service is `PRODUCT_NAME`, with no suffix, on every platform.** The label is a single
   token — no qualifier, no organization — so `service-manager` renders the same string for
   systemd, launchd and the SCM, and the name never contains a dot. The `PATH` symlink is
   `/usr/bin/<PRODUCT_NAME>`, so two variants never collide on one `PATH` entry. On Windows the
   SCM additionally carries the display name and a Description that says what the program does.

4. **Three names sit side by side, and none is derived from another.**

   | Name | Names | Appears in |
   |---|---|---|
   | `PRODUCT_NAME` (`opamp-fleet`) | the **product** | the install path, the service, the `.deb`/`.rpm` package, the `/usr/libexec/<PRODUCT_NAME>/` and `Program Files\<PRODUCT_NAME>` payload directories, the `PATH` symlink |
   | `supervisor` (`layout::COMPONENT`, `BINARY_FILENAME`) | the **program** | the file, its configuration `supervisor.toml`, the version directories, the archive member |
   | `supervisor` (`CLIENT_AGENT_TYPE`) | the **Agent type** | `service.name` on the wire, the package Set's key |

   The program's and the Agent type's names are
   [ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)'s; they share a string and are
   separate constants. Keeping the program's name off `PRODUCT_NAME` is what lets **one published
   package Set update every variant build**: the archive member a self-update extracts is the same
   in all of them.

5. **A second installation is a second build.** A variant has its own `PRODUCT_NAME` and therefore
   its own service name, package name, `/usr/libexec` payload directory, `PATH` symlink and MSI
   `UpgradeCode`. The `UpgradeCode` is the one identity that cannot be derived: each variant mints
   one GUID once and records it, because Windows Installer treats a shared `UpgradeCode` as an
   instruction to remove the other installation. An installation is an isolation boundary —
   separate Server, client identity, lifecycle and rollback. Scaling the number of *managed* Agents is
   the multiplexing inside one installation
   ([ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)); two variants on one host are told apart by
   `service.instance.name` ([ADR-0012](0012-what-an-agent-reports-about-itself.md)).

6. **There is no instance flag.** `--instance` is not a flag at all — not hidden, not accepted and
   ignored — so a command line carrying it fails to parse, and no registered service carries it.
   `service uninstall | start | stop | status` take no name: with one service per build there is
   nothing to look up. The name grammar of clause 2 is also the grammar of `[[supervisor]]` block
   names (`cli::parse_instance_name`).

7. **The install path is `<base>/<PRODUCT_NAME>`, one level, never fixed.** It is named by the
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
   (clause 8).

8. **Program and data split only on Linux at system scope, and only where the platform gives a
   reason.**
   - **Linux:** the executable layout is under `/opt`, whose default file context `usr_t` is an
     entrypoint type through which systemd starts a third-party service in
     `unconfined_service_t`. Every version the self-update stages later inherits that label, so
     staging needs no SELinux tooling, no policy module and no new runtime dependency — a label
     applied once at install time would expire at the next staged version or relabel. FHS 3.0
     places add-on software under `/opt` and its variable data under `/var`, so the split is the
     standard layout. The data root stays in `/var/lib`.
   - **Windows does not split.** The MSI delivers its payload to `Program Files\<PRODUCT_NAME>` and
     then runs the same `service install` the command line would, which builds the layout and the
     data under `%ProgramData%\<PRODUCT_NAME>`. The layout is not put in `Program Files`: the
     service's own account must be able to write it (clause 13), and granting a low-privileged
     account modify rights on a `TrustedInstaller`-owned tree it executes from is a
     privilege-escalation surface. The line is the one the Linux packages draw between the
     package-owned `/usr/libexec/<PRODUCT_NAME>/` payload and the program-owned `/opt` layout
     ([ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)). The MSI passes no root flag,
     so no directory property reaches a command line, and an MSI host and an archive host put the
     same things in the same places.
   - **Two flags.** `--root` names the layout root; given **alone** it collapses layout and data
     into that one directory, whose labelling and permissions are then the operator's business.
     `--data-root` names the data root; alone, it moves only the data. A packaged install passes
     neither and takes the platform defaults.
   - **What a package removal takes.** On **remove** the layout root and the `PATH` symlink — they
     hold nothing but staged binaries — and on **purge** additionally the data root, with the
     configuration and the identity in it. On Windows the MSI's uninstaller empties only
     `INSTALLFOLDER`, and the data under `%ProgramData%` survives: the same remove-versus-purge
     shape. An install rooted elsewhere with `--root` is manual, and no package touches it.

9. **The executable layout is versioned side by side behind a `current` pointer.**

   ```text
   <layout root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/supervisor[.exe]
   <layout root>/versions/supervisor-…/manifest.toml   # full version string, binary SHA-256
   <layout root>/current -> versions/supervisor-…/      # symlink on Unix, junction on Windows
   ```

   The directory name is the bare base and the commit short-hash of
   [ADR-0017](0017-versions-resolved-in-the-internal-crate.md)'s version, never the pre-release: the release `1.2.3` and a
   development build heading for it are told apart by their commit, and the manifest's full string
   answers release-or-dev. The service's program is `<layout root>/current/supervisor`, so a
   version switch never re-registers anything. On Unix the pointer is switched atomically — a
   temporary symlink `rename`d over `current`, never unlink-then-relink; on Windows the junction
   (which needs no symlink privilege) is recreated only while the service is stopped. At start the
   daemon verifies that `current` resolves to the version directory it runs from and repairs a torn
   switch. Staging a version already present replaces its contents and rewrites its manifest, and
   skips the write when the staged binary already holds the same bytes (an install arriving through
   the `PATH` symlink runs from that file). The default state directory is `<data root>/state` when
   the configuration names no absolute one. Every path is absolute at install time, and the
   registered command line is `run --service --config <abs> --state-dir <abs>`.

10. **An upgrade on an installed host is a re-registration, never a migration.** `--config` and
    `--state-dir` keep pointing into the data root; only the program path (`ExecStart`) changes;
    the instance's configuration, identity and state never move. A change that would move the data
    root of an installed host must bring its own migration and carries the full weight of this
    rule.

11. **The lifecycle runs over `service-manager`, system scope by default.** System scope is a
    systemd system unit, a launchd `LaunchDaemon` or an SCM service, because a fleet client runs
    without a logged-in user and starts at boot; `--user` is a development opt-in, refused on
    Windows, which has no user-scope services.
    - **Restart on failure, never after an explicit stop**, after 5 seconds and with no retry limit
      — what lets the self-update stop the service and switch `current` without the manager racing
      it. systemd and launchd carry the policy natively (`Restart=on-failure`,
      `KeepAlive{SuccessfulExit:false}`); the crate's `sc.exe` backend discards it, so
      `windows_config` sets SCM recovery actions with the same delay and enables them for non-crash
      failures, and sets the display name (`sc config`) and the Description (`sc description`).
    - **A system-scope install that lacks the rights fails before anything is written** — on
      Windows by probing the SCM for `CREATE_SERVICE` first — with a message saying so.
    - **`install` does not start the service** on launchd and says so, printing the follow-up
      `service start`; launchd `status` is advisory.
    - **`uninstall` deregisters only.** It never deletes the layout, the configuration or the state.

12. **Under the manager the daemon is the same `run` loop.** On Linux and macOS it is a plain
    foreground process: `SIGTERM` and `SIGINT` shut it down gracefully, through the transports, so
    the OpAMP `agent_disconnect` goodbye is sent on a service stop too, well within launchd's
    20-second `ExitTimeOut`; `SIGHUP` is explicitly ignored rather than left to terminate it. On
    Windows a `cfg(windows)` shim on the `windows-service` crate registers the control handler,
    reports `StartPending` → `Running` → `StopPending` → `Stopped`, and runs the identical body. The
    hidden `run --service` marker routes into the SCM dispatcher on Windows and, on every platform,
    says no terminal is watching (clause 22).

13. **The system service may run under an operator-named account, and both roots belong to it.**
    `service install --run-as <account>`, system scope only (it conflicts with `--user`); without
    it the service runs as root under systemd and launchd and as `LocalSystem` under the SCM.
    - **The service runs as the account:** systemd `User=`, launchd `UserName`, both through
      `service-manager`'s `username`; on Windows an `sc config obj=` step in `windows_config`. No
      *Log on as a service* grant is performed: the default policy grants it to
      `NT SERVICE\ALL SERVICES`, built-in accounts carry it, and a gMSA receives it from its
      domain; a host hardened against the default grant must restore it.
    - **Windows accepts only passwordless forms:** the service's own virtual account
      `NT SERVICE\<PRODUCT_NAME>` (recommended), a gMSA (`name$`), or
      `NT AUTHORITY\LocalService` / `NetworkService`. There is no password parameter — it would
      stand in the process list and the installer log — and a form needing one is refused with a
      message naming the passwordless forms.
    - **The account must already exist** on Linux and macOS; the install refuses before writing
      anything, showing the `useradd --system` line that creates it. Creating accounts is
      packaging's business, not this binary's.
    - **The hand-over covers both roots.** After laying out and registering, the install hands the
      account the data root (`supervisor.toml`, `state/`), because the service reads and rewrites
      them, and the layout root (`versions/`, `current`), because the self-update that stages into
      it is the service itself — `chown` on Unix (symlinks re-owned as links), an inheritable Modify
      ACL grant on Windows. Where the roots coincide this is one operation; where they split, an
      install that hands over only one has not succeeded. Re-running `install` with another
      `--run-as` re-owns the same directories.

14. **Without `--config`, the configuration is `<data root>/supervisor.toml`.** One rule for every
    platform, and the absolute path registered is the one just written. An explicit `--config`
    wins; because the flag has a default value, the two cases are told apart by clap's value
    source, not by comparing with the default string.

15. **Interactivity is an opt-in flag on `install`.** `service install --interactive` runs the
    questionnaire; without it `install` never asks anything, because it is the scripted command.
    `--interactive` with a stdin that is not a terminal is an **error**, not a fallback — a
    provisioning run must fail with a message rather than hang.

16. **A configuration file that exists is never overwritten** — by `--interactive` or by any
    installer-supplied answer. The questionnaire is skipped, `install` proceeds with that file and
    names it. A re-install can never eat the answers typed into the first, or the edits made to
    the file since.

17. **The questionnaire asks only what has no useful default on a fresh host, and offers no
    answer the Client would refuse.**
    - **The Server `endpoint`**, suggesting `wss://127.0.0.1:4320/v1/opamp`. A `ws://` or
      `http://` endpoint is refused unless its host is a loopback IP literal — `127.0.0.1` or
      `::1`; a host name is never loopback, `localhost` included — and the question is asked
      again with a message saying that beyond the loopback the endpoint must be `wss://` or
      `https://`.
    - **The Agent `name`**, reported as `service.instance.name`.
    - **The CA file**, only for a `wss://` or `https://` endpoint, when the Server's certificate
      is signed by a private CA ([ADR-0023](0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)).
    - **The client identity**, as one of two answers, each a certificate file and its key file
      written as `[tls] cert_file` and `key_file`: a **bootstrap certificate** and its key, with
      which the Client enrols and waits until an operator approves its request, or a **client
      certificate** and its key already issued by the Server's client CA. The identity is
      required: there is no answer without one, and a path that names no readable file is asked
      for again. It is the one proof the Server admits a peer by
      ([ADR-0026](0026-admission-by-a-client-certificate-alone.md)).
    - **The package verification key**, the Ed25519 public key written as
      `[packages] verification_key` ([ADR-0018](0018-signed-package-delivery-from-allowed-sources.md)). An empty
      answer is accepted, and the install then says that the Client installs no package, its own
      update included, until a key is configured.

    Last it asks whether the Server may update this Client, **defaulting to yes**, and the name of
    the package that carries it, defaulting to the Agent type; the consent's meaning and default
    are [ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)'s. Everything else is written as commented
    defaults, so the file stays a starting point for hand-editing. The non-interactive answers an
    installer can give — `--endpoint`
    ([ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)), `--no-self-update` and
    `--self-update-package` ([ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)) — conflict with
    `--interactive`, go through the same renderer, the same endpoint rule and the same
    never-overwrite write, and never include a client identity. A file written from them alone
    therefore fails clause 19 and stays on disk for the operator to complete.

    **No question asks for a credential, and no written file has an `[auth]` section.** The Agent
    plane admits by the client certificate alone
    ([ADR-0026](0026-admission-by-a-client-certificate-alone.md)), so there is nothing to ask. A
    Client whose `supervisor.toml` still has an `[auth]` section ignores it and sends nothing
    from it, with one startup notice naming the section
    ([ADR-0026](0026-admission-by-a-client-certificate-alone.md)); `install` keeps such a file
    (clause 16), and the section is never a reason to fail the install or to warn beyond that
    notice.

18. **The written file is treated as holding a secret:** mode `0600` on Unix; on Windows it
    inherits the data root's ACL, administrator-owned at system scope. It holds no credential. It
    names where the client identity's private key lies, and it may hold `[packages] archive_key`,
    the fleet's archive decryption secret, which an operator adds by editing the file the
    installer created.

19. **The configuration is validated before the service is registered, by the rules the Client
    applies at startup.** The order is write → load through `ClientConfig::load` → stage the
    layout → register, so a broken configuration fails at install and not at the service's first
    start. The load is the one the Client runs at startup: a `ws://` or `http://` endpoint off the
    loopback fails the install, naming the setting. A missing client identity is what the Client
    refuses at startup; the install names it in a warning and registers the service all the same,
    because a packaged install has an endpoint and no client identity to write, and no installer
    starts the service it registers. The service then refuses to run until the file is complete,
    so nothing ever runs unauthenticated. A file that already exists (clause 16) is validated the
    same way. A written file that fails to load is left on disk and
    named in the error, so a typo is corrected by editing, and a re-run of `install` proceeds with
    the corrected file.

20. **A non-interactive install with no configuration file warns.** Not an error — automation must
    not break — but a printed line naming the path registered and saying the Client refuses to
    start until that file exists with a client identity.

21. **The prompts come from `dialoguer`.** It validates an answer and asks again, offers a
    default and a yes/no confirmation, the same way on three operating systems; `dialoguer` is
    MIT-licensed and brings no TLS or crypto backend. The questionnaire asks for no secret, so no
    prompt hides its input. The terminal check itself is `std::io::IsTerminal`, no dependency.

22. **A Client running as a service writes its own log to a rotating file, on every platform.**
    `run --service` writes it; a foreground run writes stderr alone, because somebody is reading
    it. The file is written on Linux and macOS too, although the journal already holds stderr
    there, so "where are the logs" has one answer everywhere, including a container. It lives in
    `<state dir>/logs/` as `supervisor.<date>.log`: the state directory survives self-updates and
    `uninstall`, so a log explaining a failed start is still there afterwards.

23. **The log is bounded, and `[logging]` is the machine's.** It rotates daily and keeps a fixed
    number of days, 7 by default. `[logging]` in `supervisor.toml` takes `dir` (moves the file),
    `keep` (days kept) and `enabled = false` (off, for an operator whose platform collects stderr).
    `keep = 0` is refused at load rather than read as "forever", and an unknown key fails startup.
    The bound is on age and file count, not bytes: the Client is quiet and backs off when it cannot
    reach the Server. The section never arrives over the wire — a Server able to redirect or silence
    a Client's log could hide its own effects
    ([ADR-0027](0027-connection-settings-offered-without-a-credential-and-server-capabilities.md)).

24. **The file never costs the Client its run.** `tracing` takes one subscriber per process,
    installed before the command line is parsed, so the file layer is installed from the start with
    a writer that discards until the run opens the file; events before that still reach stderr.
    `tracing-appender` (from `tokio-rs/tracing`, beside `tracing-subscriber`) does the rolling,
    non-blocking. A directory that cannot be written or a full disk is reported once and the Client
    runs without the file.

**Out of scope:** the program's and the Agent type's names, the release artifacts and the native
installers ([ADR-0029](0029-releases-installers-and-the-name-supervisor-secure-by-default.md)); staging, verifying,
rolling back and pruning versions in this layout, and the self-update consent
([ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)); whether variant builds are ever published, and
where their `UpgradeCode`s are recorded; how an operator discovers what is installed on a host;
creating the `--run-as` account in the packages; a confined SELinux policy module; configuration
reload on `SIGHUP`; `Type=notify` integration; the startup rules themselves and the Server's
enrolment window and approval, which the installer only applies and names
([ADR-0026](0026-admission-by-a-client-certificate-alone.md)); the properties a native installer passes to `install`.

## Alternatives considered

- **Separate installers or OS packages owning the service** (shipping a unit file, wrapping
  `sc.exe`) — spreads the logic across artifacts and gives two registrations that disagree; the
  packages ship the binary and call `service install`.
- **Hand-write the three backends** — three code paths to own; `service-manager` abstracts exactly
  install/start/stop/status/uninstall, and the wrapper stays thin enough to finish what it omits
  (`windows_config`). **A daemonizing crate or double-fork** — Unix-only, and init systems want to
  supervise a foreground process.
- **Register the binary's own path instead of `current`** — every update would re-register the
  service with administrator rights, and Windows locks the running `.exe`.
- **Environment-variable configuration in the unit** — a second, diverging source of truth. **`anyhow`**
  — the crate uses `Result<_, String>` throughout.
- **A runtime instance flag** — no package can pass it, nothing enumerates instances, the running
  process ignores it, and it keeps an extra level in every path. A build variant is something every
  delivery path carries.
- **Keep the product's name a runtime value read from configuration** — it cannot name the
  directory the configuration is read from, and `uninstall` would need the file to find the
  service.
- **Derive the program's name from `PRODUCT_NAME`** — each variant would need its own published Set
  of the same bytes, because the archive member is extracted by name. **Name the program `agent`** —
  the system's word for every managed thing.
- **Keep the Linux layout under `/var/lib` and fix the label** — a persistent file context
  (`semanage fcontext` + `restorecon`) needs `policycoreutils-python-utils`, and a guarded fallback
  fails silently; `chcon` does not survive an autorelabel and puts SELinux tooling in the runtime
  path on every distribution; a shipped SELinux policy module is the heaviest answer (it stays
  possible if a confined domain is wanted — CIS Server Level 2 flags `unconfined_service_t`).
- **The layout under `/usr/lib` or `/usr/libexec`, or in `Program Files`** — the package manager's
  and the installer's hierarchies are theirs; a layout rewritten at runtime does not belong there,
  and on Windows it would need a privilege-escalating grant.
- **Everything, state included, under `/opt`** — parks variable data in `/opt` against FHS and gives
  back the `Program Files` problem on the other platform.
- **Root/`LocalSystem` only** — refused; least privilege is the requirement. **systemd
  `DynamicUser=`/`StateDirectory=`** — Linux-only, and ephemeral UIDs fight an identity that must
  persist. **Windows accounts with a password** — the password stands in the process list and the
  installer log. **A root-owned layout with self-update disabled under `--run-as`** — breaks goals
  10 and 11 for exactly those installs. **A privileged updater helper** — a second service, an IPC
  surface and a privilege boundary for one flag. **Creating the account in `service install`** —
  three platform APIs for what packaging does in one line.
- **A separate `config init` command** — a second entry point and a second place resolving the
  path. **Interactive by default with `--non-interactive`** (Elastic Agent's shape) — would hang
  every scripted install. **Overwrite with `--force`** — discards answers typed once and the edits
  made since. **`read_line` without a dependency** — re-implements validation, defaults and asking
  again for every question. **`inquire`, `cliclack`** — more than the questions need. **Ship an example configuration in the artifact** —
  it still has to be edited before the service does anything.
- **An installer that may write an unauthenticated or plaintext configuration** — the Client
  refuses that file at startup, so the installer would register a service that cannot start, and
  an answer without a client identity or a `ws://` default teaches the insecure setup as the
  normal one.
  **Treat `localhost` as loopback** — a name resolves through whatever the host's resolver says,
  so only the IP literal guarantees the traffic stays on the host.
- **Keep asking for the fleet credential, as optional, for a Server that still requires it** — the
  Agent plane no longer reads one ([ADR-0026](0026-admission-by-a-client-certificate-alone.md)),
  and the upgrade order is the Server first, so a question kept for older Servers would teach a
  setting nothing reads.
- **Remove a leftover `[auth]` section at install, or refuse the file** — `install` never rewrites
  a file that exists (clause 16), and refusing the section would stop a self-updated Client,
  whose file still has it, from connecting on a host nobody watches; the Client ignores the
  section and says so at startup.
- **The Windows Event Log** — an event source registered at install time, a message resource, and a
  code path only one platform exercises. **A file only on Windows** — makes "where are the logs"
  platform-dependent and leaves containers without one. **Rely on the OTLP own-logs bridge**
  ([ADR-0022](0022-own-telemetry-over-tls-1-3-and-plaintext-only-to-the-loopback.md)) — needs a reachable Server, so it cannot explain the failures
  that prevent one. **Size-based rotation** — `tracing-appender` does not offer it. **`keep = 0` as
  unlimited** — the setting that fills a disk. **Remotely configurable logging** — the Client's own
  configuration stays on the machine. **The log in the version directory** — a self-update would
  scatter it and a rollback take it away.

## Sources / Prior art

- Specification Mission, Strategy and goals 10 and 11 ([`docs/SPECIFICATION.md`](../SPECIFICATION.md));
  Strategy "Security before convenience" and Q-1 "Secure by default".
- Loopback names: [RFC 6761 §6.3](https://www.rfc-editor.org/rfc/rfc6761#section-6.3) (`localhost`
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
  service names ([`GetServiceKeyName`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-getservicekeynamea)).
- Versioned directories and a pointer: Elastic Agent's `<component>-<version>-<hash>` directories
  and upgrade design (<https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md>); Scoop's
  `current` junction (<https://github.com/ScoopInstaller/Scoop/wiki/The-'Current'-Version-Alias>);
  the Chromium updater (<https://chromium.googlesource.com/chromium/src/+/main/docs/updater/design_doc.md>);
  atomic symlink replacement (<https://blog.moertel.com/posts/2005-08-22-how-to-change-symlinks-atomically.html>).
- systemd [unit names](https://man7.org/linux/man-pages/man5/systemd.unit.5.html),
  [kill](https://man7.org/linux/man-pages/man5/systemd.kill.5.html),
  [daemon(7)](https://man7.org/linux/man-pages/man7/daemon.7.html) on `SIGHUP`,
  [`Restart=on-failure`](https://man7.org/linux/man-pages/man5/systemd.service.5.html); launchd
  (<https://www.launchd.info/>, launchd.plist(5)); Telegraf's Windows service
  (<https://github.com/influxdata/telegraf/blob/master/docs/WINDOWS_SERVICE.md>); the
  `WaitToKillServiceTimeout` budget
  (<https://kb.firedaemon.com/support/solutions/articles/4000086193-increasing-service-shutdown-time>);
  [clap derive](https://docs.rs/clap/latest/clap/_derive/index.html).
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
- Service accounts: [Service User Accounts](https://learn.microsoft.com/en-us/windows/win32/services/service-user-accounts),
  [virtual accounts](https://docs.delinea.com/online-help/privilege-manager/install/upgrades/virtual-accounts.htm),
  [`sc config`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/sc-config),
  [`ChangeServiceConfig`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfiga);
  [Elastic Agent unprivileged mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged);
  [OpenTelemetry Collector Linux packages](https://opentelemetry.io/docs/collector/install/binary/linux/).
- First configuration: [Elastic Agent command reference](https://www.elastic.co/docs/reference/fleet/agent-command-reference)
  (`install` prompting, `--non-interactive`, `--force` overwriting — the two behaviours inverted
  here); [`dialoguer`](https://crates.io/crates/dialoguer); [`inquire`](https://crates.io/crates/inquire);
  [Rust CLI prompts compared](https://fadeevab.com/comparison-of-rust-cli-prompts/);
  [`std::io::IsTerminal`](https://doc.rust-lang.org/stable/std/io/trait.IsTerminal.html);
  [machine communication](https://rust-cli.github.io/book/in-depth/machine-communication.html),
  [isatty](https://blog.jez.io/cli-tty/); the OpenTelemetry `opampsupervisor`
  (<https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>),
  which has no scaffolding command.
- Logging: [Elastic Agent logging](https://www.elastic.co/docs/reference/fleet/elastic-agent-standalone-logging-config)
  (logs under its data directory, size rotation); [OpenTelemetry Collector #5300](https://github.com/open-telemetry/opentelemetry-collector/issues/5300)
  (file logging for a Windows service, the Event Log output reported unreliable);
  [`tracing-appender`](https://crates.io/crates/tracing-appender).

## Consequences

- Positive: one binary installs, controls and deregisters itself as a native service on all three
  platforms, from any location, and still runs in the foreground. A service stop sends the OpAMP
  goodbye. A self-update stages a version and switches `current` without re-registering.
- Positive: the layout says what it is — `/opt/opamp-fleet/current/supervisor`, the product, then
  the program. An illegal name fails the build, not the host. One Set updates every variant build.
  An MSI host and an archive host put the same things in the same places.
- Positive: the `.rpm` produces a service that starts on Fedora, RHEL and SUSE 16 with SELinux
  enforcing, with no labelling step to keep alive.
- Positive: the Client and every Managed Process it spawns can drop root and `LocalSystem`, with no
  password anywhere in the Windows story.
- Positive: a fresh host goes from a binary to a working, registered service in one command, and
  no secret is typed into the installer at all. A Windows service failure becomes readable, and
  the logs cover failures own-telemetry structurally cannot.
- Positive: the first configuration a fresh host gets authenticates and encrypts beyond the
  loopback; an install never registers a service the Client would then refuse to start.
- Negative / trade-offs: a Windows-only runtime path Unix never exercises; system scope needs root
  or Administrator; launchd `status` is advisory and its `install` does not start; real
  registration runs only in the ignored smoke test and a manual checklist.
- Negative / trade-offs: the Linux system install spans two directories, and `--data-root` is the
  flag that must be right where getting it wrong is silent — `--root` alone on an enforcing host
  roots the binary wherever the operator chose. A host mounting `/opt` `noexec` breaks, loudly.
- Negative / trade-offs: a second installation needs a variant build, a minted `UpgradeCode` and a
  package-name entry; a forgotten `UpgradeCode` fails on a Windows host by removing the other
  installation. Two constants hold `supervisor` for different reasons, and conflating them would
  split the fleet's package Sets.
- Negative / trade-offs: the `--run-as` account is a trust boundary — whoever holds it can replace
  the binary in the layout, and the `PATH` symlink through `current` makes an administrator invoking
  the CLI execute account-owned code. Managed Processes inherit the account, so ports below 1024 and
  root-only telemetry sources fail under it.
- Negative / trade-offs: a configuration cannot be generated without installing; the questionnaire
  is a second place following the configuration schema, for the keys it asks. The log duplicates the
  journal on Linux and macOS, is bounded by days and not bytes, and keeps a week of whatever the
  Client logs on disk, so nothing secret may be logged.
- Negative / trade-offs: the questionnaire asks for a certificate and its key and a verification
  key, and an operator must have them in hand before the first install. A
  scripted install that passes `--endpoint` alone no longer registers a service: it writes the
  file, fails validation, and is completed by editing that file and re-running `install`, or by
  provisioning a complete file first.
- Negative / trade-offs: a host whose file was written with a credential keeps that `[auth]`
  section, secret included, until an operator deletes it: neither `install` nor the Client rewrites
  the file. The startup notice names the section on every start until then.
- Follow-ups: the service smoke test excludes hosts with SELinux or AppArmor in the way, so nothing
  automated exercises clause 8's reason — the manual checklist covers it; `.deb`/`.rpm` account
  creation and `--run-as` wiring, and a virtual-account option in the MSI; whether `uninstall`
  should offer to remove the configuration and log it created; a size cap beside the day count;
  remotely raising a Client's log level, as a decision of its own.

## Enforcement

- `crates/fleet-agent/build.rs` fails the build on a `PRODUCT_NAME` outside the grammar or a blank
  display name; `crates/fleet-agent/src/product.rs` `product_name_satisfies_the_grammar`,
  `display_name_is_prose`.
- `crates/fleet-agent/src/service/manager.rs`: `every_backend_renders_the_same_name`,
  `the_service_is_named_after_the_product_not_the_program`, `the_service_name_carries_no_suffix`,
  `the_names_a_human_reads`, `the_installed_command_line_is_the_marker_plus_absolute_paths`,
  `both_platforms_restart_after_the_same_delay`,
  `the_default_root_is_one_level_named_after_the_product`,
  `the_linux_system_layout_executes_from_opt`, `no_other_platform_splits`.
- `crates/fleet-agent/src/cli.rs`: `instance_is_not_a_flag_any_more`, `both_roots_can_be_named`,
  `service_verbs_parse_with_scope_and_root`, `the_installed_command_line_parses`,
  `install_is_not_interactive_unless_asked`, `bare_invocation_has_no_subcommand`.
- `crates/fleet-agent/src/service/layout.rs`: `the_directory_name_is_base_plus_hash_never_the_prerelease`,
  `set_current_points_and_repoints`, `stage_writes_binary_manifest_and_pointer`,
  `restaging_identical_bytes_leaves_the_staged_binary_untouched`,
  `restaging_replaces_a_staged_binary_with_different_bytes`,
  `a_torn_pointer_is_healed_a_correct_one_left_alone`,
  `the_layout_is_found_from_the_pointer_the_service_was_registered_against`.
- `crates/fleet-agent/src/service/run_as.rs`: `windows_forms_are_the_passwordless_ones`,
  `a_missing_account_is_refused_with_the_way_out`,
  `the_handover_walks_the_tree_and_skips_what_is_missing`.
- `crates/fleet-agent/src/config_init.rs`: `an_existing_file_is_never_overwritten`,
  `run_keeps_an_existing_file_without_asking`,
  `an_endpoint_given_never_overwrites_an_existing_file`,
  `interactive_without_a_terminal_fails_instead_of_blocking`,
  `the_file_is_not_readable_by_the_rest_of_the_machine`,
  `a_private_ca_is_only_asked_about_where_tls_applies`,
  `the_rendered_file_loads_as_what_was_answered`,
  `a_bad_endpoint_is_refused_before_anything_is_written` (clauses 16–18);
  `the_suggested_endpoint_is_tls_on_the_loopback`,
  `a_plaintext_endpoint_is_accepted_only_on_a_loopback_literal`, which refuses `localhost`
  (clause 17); `a_complete_answer_writes_a_file_the_client_starts_with`, which renders an identity
  as `[tls] cert_file` and `key_file` and the verification key as answered, and passes
  `ClientConfig::check_admission` (clauses 17, 19);
  `installer_answers_alone_fail_validation_and_stay_on_disk` (clauses 17, 19);
  `a_rendered_file_has_no_auth_section`, which renders complete answers and asserts
  that the file holds no `[auth]` (clause 17).
- `crates/fleet-agent/src/config_file.rs`: `admission_needs_a_client_identity_and_nothing_else`,
  which asserts that `ClientConfig::check_admission` passes a file with a client identity and no
  `[auth]` and refuses one without an identity, naming `[tls] cert_file` and `key_file` (clauses
  19, 20). `crates/fleet-agent/src/config.rs`, shared with
  [ADR-0026](0026-admission-by-a-client-certificate-alone.md):
  `a_leftover_auth_section_is_ignored_with_a_notice`, which loads a file with an `[auth]`
  section, asserts that it loads, that nothing is sent from it, and that the notice names `[auth]`
  (clause 17).
- Logging: `crates/fleet-agent/tests/logging.rs` (`a_service_run_writes_a_log_file`,
  `a_foreground_run_writes_no_log_file`); `crates/fleet-agent/src/config.rs`
  `the_log_file_is_on_by_default_and_its_retention_is_not_optional`; `crates/fleet-agent/src/logging.rs`
  `the_writer_discards_until_a_file_is_opened`, `the_log_directory_hangs_off_the_state_directory`.
- `crates/fleet-agent/tests/shutdown.rs` `sigterm_shuts_the_client_down_cleanly`;
  `crates/fleet-agent/tests/service_smoke.rs`
  `the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop`, run with
  `--ignored` by the `service-smoke` workflow on an ephemeral runner.
- `crates/fleet-agent/tests/msi_exe_command.rs`
  `the_msi_names_no_root_so_no_directory_property_reaches_a_command_line`; the `.rpm` scriptlet
  assertions in [`.github/workflows/release.yml`](../../.github/workflows/release.yml) require the
  removal to take the layout root and the purge the data root.

**Not mechanically decidable:** that the layout starts under an enforcing SELinux policy — no CI
runner enforces it, so the manual checklist in [`README.md`](../../README.md) carries that step.
The questionnaire's prompts (clause 17) read a terminal, which `cargo test` does not have: that no
answer skips the client identity, that no question asks for a credential, that a certificate or key
path naming no readable file and a refused endpoint are asked for again, and that an empty
verification key is named, are review questions on `ask` in `config_init.rs`.
`service install` (clauses 19, 20) validates only on the way to registering a service, so the
warnings it prints and its load of an existing file are not run by any test; the rules it applies
are `ClientConfig::load` and `ClientConfig::check_admission`, exercised above.
