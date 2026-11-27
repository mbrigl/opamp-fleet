# CLT-accounts-service-runs-under: The accounts the Client's service and its programs run under

- **Status:** 🟢 accepted
- **Date:** 2026-11-15
- **Deciders:** Markus Brigl
- **Applies to:** the account a `service install` chooses, accepts, sets and hands the roots to, `--run-as`, `--process-account` and `--no-process-account`, and the second service in `crates/fleet-agent/src/main.rs`, `crates/fleet-agent/src/service/run_as.rs`, `crates/fleet-agent/src/service/manager.rs`, `crates/fleet-agent/src/service/runtime.rs`, `crates/fleet-agent/src/service/windows.rs`, `crates/fleet-agent/src/service/layout.rs`, `crates/fleet-agent/src/service/windows_config.rs`, `crates/fleet-agent/src/service/windows_rights.rs` and `crates/fleet-agent/src/service/withdrawal.rs`, the systemd units, launchd plists and SCM entries the install registers, `service start`, `service stop` and `service uninstall` on Windows, the install lock, the pipe, the job objects, the security of the second service's process and threads and the handle-based walks in `crates/fleet-agent/src/win32.rs` and `crates/fleet-agent/src/win32/`, the no-follow adapter `crates/fleet-util/src/nofollow.rs` and the state the Client keeps through it in `crates/fleet-agent/src/storage.rs`, `crates/fleet-agent/src/csr.rs`, `crates/fleet-agent/src/connection.rs` and `crates/fleet-agent/src/tls.rs`, every place a Supervisor starts a program in `crates/fleet-agent/src/supervisor/process.rs`, the channel in `crates/fleet-agent/src/supervisor/channel.rs` and `crates/fleet-agent/src/supervisor/channel/windows.rs`, `supervisor_dir` and the Supervisors' directories with their modes, owning accounts and ACLs, the modes a package archive gives a program, `packaging/linux/postinst` and `packaging/linux/postrm`, the `[package.metadata.deb]` and `[package.metadata.generate-rpm]` tables of `crates/fleet-agent/Cargo.toml`, `packaging/windows/supervisor.wxs`, the Windows service smoke in `crates/fleet-agent/tests/service_smoke.rs`, the enrolment `service install` and `service config` run in `crates/fleet-agent/src/service/enrol.rs` and what the questionnaire and `--endpoint` hand it in `crates/fleet-agent/src/config_init.rs`, and every document that tells an operator which account the Client or a Managed Process runs as or what the separation by a process account holds against a Managed Process on Windows

## Context

The Client runs as a system service. Every program its Supervisors start runs under the account
that starts it, and so does every command their configuration language can name: an exec input,
a check command, a task that installs software
([CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)). The Server sends those
configurations. The account that starts a program is therefore the most a compromised or
misconfigured Server reaches on a host.

A Managed Process is code the Supervisor does not vouch for
([CLT-what-an-agent-reports §8](CLT-what-an-agent-reports.md)). Under the Client's own account it
reads everything the Client reads: the whole `state_dir` with the Client's private key
`client-key.pem`, the `config/` of every other Supervisor, and the Client's environment through
`/proc`. It can rewrite its own `program/` and so outlast the next package. With the Client's key
it can present itself to the Server for any `instance_uid` of its host
([SEC-admission-by-client-certificate §12](SEC-admission-by-client-certificate.md)).

The specification places a Managed Process and its account, where a process account is
registered, below the Client's account on its host: it must not gain the Client's key, program, or
configuration. Its Strategy puts security before convenience, and Q-1 asks for a secure default.
G-11 asks that the Client run and update itself as an OS service on every platform. The
self-update stages into the layout root, so the service's account must own that root.

Forces:

- **Least privilege by default.** A packaged Linux host and a fresh Windows host must not hand a
  Server-delivered command root or `LocalSystem` unless an operator chose it.
- **Some agents need root.** An inventory agent reads the DMI tables (`dmidecode`), and a task
  that installs software installs operating-system packages. Inputs that read SMART or IPMI data,
  and many monitoring plugins, run through `sudo` or a setuid helper (`check_icmp`, `check_dhcp`).
  A listener on a port below 1024 needs `CAP_NET_BIND_SERVICE`. On Windows a full inventory, an
  agent that installs a service, or a reader of the Security event log needs `LocalSystem` or a
  group. A host with such an agent must be able to keep the privileged account by an explicit
  choice of the operator, and that choice must survive an upgrade.
- **systemd ties most sandboxing to `no_new_privs`.** For a unit without `CAP_SYS_ADMIN`, which is
  every unit with a non-root `User=`, these directives imply `NoNewPrivileges=yes`:
  `ProtectKernelTunables=`, `ProtectKernelModules=`, `ProtectKernelLogs=`, `ProtectClock=`,
  `ProtectHostname=`, `PrivateDevices=` and `RestrictAddressFamilies=`. So do the seccomp-based
  directives `LockPersonality=`, `RestrictRealtime=`, `RestrictSUIDSGID=`, `RestrictNamespaces=`,
  `MemoryDenyWriteExecute=`, `SystemCallFilter=` and `SystemCallArchitectures=`. With
  `no_new_privs` set, `sudo` and every setuid or file-capability helper stop working for the
  service and for each of its Managed Processes.
- **`service-manager` renders a fixed unit.** Its systemd renderer writes `Description`,
  `ExecStart`, `Restart`, `RestartSec`, `User` and `WantedBy`, and has no place for any other
  directive. It joins the arguments of `ExecStart` with spaces and does not quote them, so a root
  path that contains a space produces a unit that names the wrong program.
- **The Client writes the places it needs.** It writes its data root (`supervisor.toml`, `state/`)
  and its layout root (`/opt/<PRODUCT_NAME>`, where the self-update stages versions and switches
  `current`). It writes each Supervisor's directory. It starts Managed Processes that may write
  wherever their kind keeps state: a monitoring agent under its own directories in `/etc` and
  `/var/lib`, a task that installs software anywhere a package installs.
- **Upgrades run unattended.** The Linux post-install runs inside `apt upgrade` and `dnf upgrade`,
  the MSI inside a major upgrade, often on hosts nobody watches. An upgrade must not change which
  account runs a configured host.
- **Privileged steps work in directories the account owns.** `service install` and
  `service config` run as root or as an Administrator, on every package upgrade. They write the
  enrolment's files into the state directory, stage the program and its manifest into the layout
  root, and re-own what they wrote. Done with plain path operations, each goes where a link
  points. A link at `client-key.pending.pem` truncates the file behind it and narrows it to
  `0600`. A state directory that is a link has the directory behind it narrowed and filled. The
  program's mode change reaches the file behind a link. On Windows a junction does the same at a
  directory, and the account holds delete on the roots themselves.
- **Starting a program under another account takes a privilege.** On Linux, `CAP_SETUID` and
  `CAP_SETGID` permit arbitrary changes of a process's user and group IDs, uid and gid 0
  included, and SafeSetID still allows arbitrary `setgroups()`. A process signals another
  account's process only with `CAP_KILL`, which the process-group stop needs. On macOS a non-root
  process cannot change its user ID, and launchd's `UserName` applies only in the system domain.
  On Windows a restricted token is unproven for the agents' network and RPC use. A second service
  under its own virtual account needs no password.
- **The second service shares its account with what it starts.** A Managed Process can remove the
  second service's socket and bind its own, or create an instance of its pipe. It can leave its
  process group or job and so outlive the Client's connection. A check of the peer's account
  alone accepts such an impostor. The impostor then receives each program to start, its arguments
  and environment, and the Supervisor Endpoint's token. Each service manager knows the process it
  runs as the second service, and each platform reports the process at the other end of a local
  connection. A process id can be reused (polkit, CVE-2013-4288). A process of the same account can
  reach into the real second service by `ptrace`, `/proc/<pid>/mem` or `pidfd_getfd` where Yama
  permits it, unless that process is not dumpable.
- **On Windows a process is open to its own account by default.** A process or thread whose
  creator names no security descriptor gets one from the creator's token. That default lets every
  process of the same account open it with the rights the account holds, among them
  `PROCESS_VM_READ`, `PROCESS_VM_WRITE`, `PROCESS_CREATE_THREAD` and `PROCESS_DUP_HANDLE`. A
  Managed Process can so read what the Client sends the second service, the Supervisor Endpoint's
  token among it, run code in it, or duplicate its pipe handle and answer the Client in its place.
  Windows checks every `OpenProcess` and `OpenThread` against the object's DACL. A caller without
  `SeDebugPrivilege`, which the process account does not hold (clause 2), gets only what the DACL
  grants. The account that owns a process keeps `READ_CONTROL` and `WRITE_DAC` implicitly unless an
  `OWNER RIGHTS` entry says otherwise. A thread is an object with a DACL of its own, and
  `THREAD_SET_CONTEXT` on one is enough to run code in its process. Changing the token's default
  DACL reaches every program the second service starts.

The enrolment that `service install` and `service config` run connects to the Server with the
bootstrap identity the administrator provided
([SEC-bootstrap-certificates-and-enrolment §16](SEC-bootstrap-certificates-and-enrolment.md)). Its
endpoint and trust (`[tls] ca_file`) stand in `supervisor.toml`, which the questionnaire or
`--endpoint` wrote. That file and the CA copy beside it lie in the data root, which the service's
account owns (clause 1). Clause 11 keeps a privileged step from going through a link the account
placed, and the enrolment reads the CA file and the bootstrap pair once, bounded, through the held
directory. Clause 11 does not cover the *content* of a file the account owns. An account that is
taken over can rewrite `endpoint` or the CA copy between the write and a read-back. The privileged
step would then present the administrator's bootstrap identity to a Server the account chose.

The gain for such an account is small. The certificate an enrolment obtains lands in the state
directory, which the account owns anyway, and the service connects wherever the file says once it
runs. What the account cannot otherwise do is make root present the administrator's bootstrap
identity to a Server of its choosing; the bootstrap key may be one the account cannot read.
[`HARDENING.md`](../HARDENING.md) lists the measure and its residual, in service of Q-1.

Four runs enrol, and they differ in what the administrator said:

- **`--interactive` and `service config`:** the questionnaire asks for the endpoint and the CA.
  Its defaults come from the file, and the administrator sees and confirms each.
- **`service install --endpoint` writing a new file:** the administrator names the endpoint. The
  CA comes from beside the program
  ([SEC-bootstrap-certificates-and-enrolment §17](SEC-bootstrap-certificates-and-enrolment.md)).
- **`service install --endpoint` over an existing file:** the file is kept as it is
  ([CLT-installed-service-and-state §24](CLT-installed-service-and-state.md)), so the service
  runs with its endpoint, not the one named. The MSI's custom action runs this way on an upgrade
  that is given an endpoint. The Linux packages' `postinst` only prints the command for the
  administrator.
- **`service install` without either, over an existing file:** nothing is answered. An upgrade
  with a bootstrap identity beside the program enrols with what the file says.

## Decision

We will bound what a Server-delivered command reaches on a host by the accounts the Client runs
under:

- the installed Client runs under the account its registration names;
- every program its Supervisors start runs under a process account, through a second service,
  where the operator names one;
- everything the Client keeps in its roots is reached through a no-follow adapter;
- a privileged enrolment connects with the endpoint and CA its own run answered.

On a first registration the account is the one the Linux packages create, or on Windows the
service's own virtual account. The binary's own first registration under systemd or launchd runs
as root unless an account is named. No privileged install step goes through a link either account
placed.

1. **The system service runs under the account its registration names, and both roots belong to
   it, except what the process account keeps there.** `service install --run-as <account>` names
   the account. The flag applies at system scope only and conflicts with `--user`. On Linux and
   macOS an operator chooses root with `--run-as root`. The install writes it as `User=root` or
   `UserName` `root`, so a unit records that root was chosen. On Windows an operator chooses
   `LocalSystem` with `--run-as LocalSystem` or `--run-as "NT AUTHORITY\SYSTEM"`. Without the
   flag, the install takes the account in this order:
   - **A re-registration keeps the account the registration names.** Under systemd that is
     `User=` in the system unit `/etc/systemd/system/<PRODUCT_NAME>.service`. Under launchd it is
     `UserName` in `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`. The install reads the file the
     Client wrote. It reads neither a drop-in nor the loaded state of the service manager. On
     Windows the install reads the logon account the SCM holds for the service
     (`QueryServiceConfig`) and keeps it, `LocalSystem` included. A host registered as
     `LocalSystem` therefore stays as it is. The install reads and keeps the second service's
     account the same way (clause 16). The install reconfigures a service the SCM already holds in
     place:
     - it rewrites the service's program, arguments and start type;
     - it sets the logon account only by the `sc config obj=` step of `windows_config`;
     - what clause 7 and `windows_config` set follows as on a first registration.
   - **A unit or plist that names no account, on a data root that a non-root account owns,** was
     rewritten by a re-registration that dropped its account. The install takes the account that
     owns the data root. It prints that it did so, naming the data root. Every other registration
     that names no account stays root.
   - **A first registration** is one where no unit, plist or SCM entry exists. It runs as root
     under systemd and launchd. Under the SCM it runs as the service's own virtual account
     `NT SERVICE\<PRODUCT_NAME>`. The Linux packages name their account themselves (clause 3).

   The install prints the account and where it came from: `--run-as`, the unit, the plist, the
   SCM's entry, the account that owns the data root, or the default. Where the account is
   `LocalSystem`, named or kept, the install also warns that every Managed Process the Client
   starts itself runs as `LocalSystem`. The warning names `--run-as "NT SERVICE\<PRODUCT_NAME>"`
   as the way to the virtual account.
   - **The service runs as the account.** Under systemd the account is `User=` in the unit the
     Client writes (clause 7). Under launchd it is `UserName`, through `service-manager`'s
     `username`. On Windows it is an `sc config obj=` step in `windows_config`, on every
     registration, the default included. The install performs no *Log on as a service* grant. The
     default policy grants that right to `NT SERVICE\ALL SERVICES`, built-in accounts carry it,
     and a gMSA receives it from its domain. An operator who hardened a host against the default
     grant must restore it.
   - **Windows accepts only passwordless forms:** the service's own virtual account
     `NT SERVICE\<PRODUCT_NAME>` (recommended, and the default), a gMSA (`name$`),
     `NT AUTHORITY\LocalService` / `NetworkService`, or `LocalSystem` / `NT AUTHORITY\SYSTEM`.
     There is no password parameter, because a password would stand in the process list and the
     installer log. The install refuses a form that needs a password, with a message naming the
     passwordless forms.
   - **The account must exist** on Linux and macOS, whether it was named or kept. The install
     refuses an account that does not exist before it writes anything, and fails closed. For a
     kept account, the message names the account and the file or directory it came from. The
     message shows the `useradd --system` line that creates the account. It says that
     `--run-as <account>` or `--run-as root` names another. The binary never creates an account.
     The packages do (clause 3).
   - **The hand-over covers both roots, on every registration that runs under an account,** a
     kept account and root included. After laying out and registering, the install hands the
     account the data root (`supervisor.toml`, `state/`), because the service reads and rewrites
     them. It also hands over the layout root (`versions/`, `current`), because the self-update
     that stages into it is the service itself. On Unix the hand-over changes the owning account.
     On Windows it grants Modify to the account. The grant is inherited by what each root holds
     (`(OI)(CI)`) and set explicitly on every entry that exists. The install grants `LocalSystem`
     nothing, since it keeps full control of every root (clause 26). Where a process account is
     registered, the grant also carries `WRITE_DAC`. The Client so grants the process account its
     share of what the install laid out (clause 21). The grant never carries `WRITE_OWNER`. Where
     the roots coincide, the hand-over is one operation. Where they split, an install that hands
     over only one root has not succeeded.
   - **Where a process account is registered, the hand-over leaves its data alone.** The
     hand-over skips an entry the process account owns, together with everything beneath it. An
     entry in the process account's group keeps that group, and a directory there keeps its mode
     too. So no re-registration, every package upgrade among them, takes from a Managed Process
     the data it wrote or reads.
   - **The hand-over never follows a link, and neither does any other privileged step**
     (clause 11). The account owns the trees that root walks, and clause 3 makes that walk part of
     every package upgrade. So the hand-over:
     - opens each directory without following a link, and changes its owning account through
       that open handle;
     - reads the directory's entries from the handle, and re-owns a symbolic link as a link;
     - skips a regular file with more than one hard link and names it in a warning (clause 14).

     On Windows the hand-over walks as clause 21 says, with the backup, restore and
     take-ownership privileges held. Re-running `install` with another `--run-as` re-owns the same
     directories. The install withdraws from the former account what it was granted (clause 26).

2. **The install grants the Windows account no group and no privilege.** A Managed Process may
   need more than the virtual account holds, such as the Security event log through *Event Log
   Readers* or performance counters through *Performance Monitor Users*. The operator then adds
   `NT SERVICE\<PRODUCT_NAME>` to that group, or runs the host as `LocalSystem` with `--run-as`.
   The manual shows the `net localgroup` line for each.

3. **The Linux packages create the account `<PRODUCT_NAME>` and name it on a first
   registration.**
   - **The account.** Before `service install`, the post-install checks for the account with
     `getent passwd`. Where it is missing, the post-install creates it with
     `useradd --system --user-group --no-create-home --home-dir /var/lib/<PRODUCT_NAME> --shell
     /usr/sbin/nologin <PRODUCT_NAME>`. Where a group of that name already exists without the
     account, the post-install passes `--gid <PRODUCT_NAME>` in place of `--user-group`, which
     would refuse it. The account gets no password. Its home is the data root, so a Managed
     Process that writes under `$HOME` writes into a directory the account owns. The post-install
     uses an existing account of that name as it is, and never modifies it.
   - **On every run.** The post-install creates the account on every run, also on a host whose
     registration stays root. Moving such a host to the account is so one command.
   - **The name only on a first registration.** The post-install passes `--run-as <PRODUCT_NAME>`
     only where `/etc/systemd/system/<PRODUCT_NAME>.service` is absent. Otherwise it runs
     `service install` without the flag, which keeps the account the unit names (clause 1). A host
     that chose root with `--run-as root` therefore stays root through every upgrade.
   - **Variants and dependencies.** A variant build creates the account of its own
     `PRODUCT_NAME`, so two variants never share one. The `.deb` declares a dependency on the
     package that ships `useradd`. The `.rpm` requires the file `/usr/sbin/useradd`, which
     `cargo-generate-rpm` can state only as a plain `Requires`.

4. **The package delivers one file; `service install` installs.**

    | | delivers | after install | before removal |
    |---|---|---|---|
    | `.deb` / `.rpm` | `/usr/libexec/<PRODUCT_NAME>/supervisor` | the account `<PRODUCT_NAME>` where it is missing (clause 3), then `… service install --run-as <PRODUCT_NAME>` where no unit is registered, `… service install` where one is | `service stop`, then `service uninstall` |
    | `.msi` | `INSTALLFOLDER\supervisor.exe` | `… service install [--endpoint <ENDPOINT>] [--no-self-update]` | `service stop`, then `service uninstall` |

    - **No package ships a systemd unit, a `LaunchDaemon`, a `sysusers.d` entry or an MSI
      `ServiceInstall` element.** The packaging leaves `cargo-deb`'s `systemd-units` and WiX's
      `ServiceInstall`/`ServiceControl` unused on purpose. Every platform has one install path.
    - **What the package owns and what the layout holds never overlap.** The package owns its
      payload and nothing in the layout. `service install` stages a *copy* into the layout. The
      service runs that copy, and a self-update replaces it. So `dpkg -V` and `rpm -V` stay quiet
      through every fleet update.
    - **The pre-removal runs only on a real removal.** On an upgrade it falls straight through,
      because the successor's post-install runs `service install` again. Unregistering would stop
      a managed host in the middle of `apt upgrade`. The pre-removal calls the payload path, never
      the symlink, and tolerates a service that is already unregistered.
    - **An upgrade stays an upgrade.** The dpkg/rpm package identity is `PRODUCT_NAME`. The MSI's
      identity is its display name and `UpgradeCode`. So an `apt`, `dnf` or MSI upgrade stays an
      upgrade.

5. **The CLI on `PATH` is a symlink through `current`, and a package removal takes the layout and
   leaves the account.** `/usr/bin/<PRODUCT_NAME>` points to
   `/opt/<PRODUCT_NAME>/current/supervisor`. The operator's first diagnostic command so answers
   for the binary the service runs, not for the one the package delivered. The maintainer scripts
   lay the symlink, rather than shipping it as a packaged file, and both formats honour them:

    | hook | does |
    |---|---|
    | `postinst` / `%post` | the account where it is missing (clause 3), `service install` as clause 4 states, then `ln -sfn` the symlink |
    | `%posttrans` (rpm only) | `ln -sfn` again. rpm erases the old package's files *after* the new `%post`, and that may take the link. dpkg removes them during unpack and needs nothing |
    | `postrm` / `%postun`, real removal only | remove the symlink (only if it is one), and the layout root `/opt/<PRODUCT_NAME>` with every staged version and `current`; on dpkg **purge**, also the data root `/var/lib/<PRODUCT_NAME>` |

    - **A removal keeps the data root.** The Agent's identity and the configuration the operator
      wrote are not binaries, and a reinstall picks them up.
    - **A removal takes the layout.** A reinstall so comes up on the package it installed, not on
      a surviving `current`. Purge is dpkg's "leave nothing"; rpm has none.
    - **No hook removes the account or its group, on removal or on purge.** Files the account
      owns outside the two roots, such as a Managed Process's output, would otherwise pass to
      whichever account later gets the same uid.
    - **The removal is the package's.** `service uninstall` itself deletes nothing
      ([CLT-installed-service-and-state §17](CLT-installed-service-and-state.md)).
    - **Re-staging skips the write when the staged binary already holds the running bytes.**
      `service install` invoked through the symlink runs from the very file it would overwrite,
      and Linux refuses that (`ETXTBSY`).
    - **Only the default system roots.** The symlink, the account and the removal name the default
      system roots only, the only roots a packaged install uses. An install rooted with `--root`
      is a manual one, and nothing packaged touches it.

6. **The executable layout is versioned side by side behind a `current` pointer, and the
   registered command line names the second service where there is one.**

   ```text
   <layout root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/supervisor[.exe]
   <layout root>/versions/supervisor-…/manifest.toml   # full version string, binary SHA-256
   <layout root>/current -> versions/supervisor-…/      # symlink on Unix, junction on Windows
   ```

   - **Directory name.** It is the bare base and the commit short-hash of the version
     [OPS-cargo-decides-product-version §3](OPS-cargo-decides-product-version.md) resolves,
     never the pre-release. Their commit tells the release `1.2.3` apart from a development build
     heading for it. The manifest's full string answers release or development build.
   - **The program.** The service's program is `<layout root>/current/supervisor`, so a version
     switch never re-registers anything.
   - **The switch.** On Unix the pointer is switched atomically: a temporary symlink is `rename`d
     over `current`, never unlinked and then linked again. On Windows the junction, which needs
     no symlink privilege, is recreated only while the service is stopped. At start the daemon
     verifies that `current` resolves to the version directory it runs from, and repairs a torn
     switch.
   - **Restaging.** Staging a version already present replaces its contents and rewrites its
     manifest. Staging skips the write when the staged binary already holds the same bytes,
     because an install arriving through the `PATH` symlink runs from that file.
   - **The state directory** is the one
     [CLT-installed-service-and-state §14](CLT-installed-service-and-state.md) decides.
   - **Absolute paths.** Every path is absolute at install time. The registered command line is
     `run --service --config <abs> --state-dir <abs>`. Where a process account is registered,
     `--agents-socket <abs> --process-uid <uid>` follows (clause 17). On Windows
     `--agents-socket \\.\pipe\<PRODUCT_NAME>-agents --process-sid <SID>` follows instead, with
     the process account's SID, which the Client accepts as the account that owns the pipe
     (clause 19).

7. **The Client writes its systemd system units itself, and `service-manager` installs them.**
   `manager::install` passes `contents: Some(<unit>)` at system scope under systemd.
   `service-manager` still writes the file (mode `0644`), enables it, which reloads the manager's
   configuration, and uninstalls it. The unit holds:
   - `[Unit]`: `Description=<PRODUCT_NAME>`. The display name stays where
     [CLT-installed-service-and-state §17](CLT-installed-service-and-state.md) puts prose. Where a
     process account is registered, the section also holds `Wants=<PRODUCT_NAME>-agents.service`
     and `After=<PRODUCT_NAME>-agents.service` (clause 17).
   - `[Service]`: `ExecStart=` with the program and every argument in double quotes. A `\` and a
     `"` are escaped, a `%` is written `%%` and a `$` is written `$$`, so any absolute path
     reaches the program as it was given. Then `Restart=on-failure`, and `RestartSec=` set to the
     restart delay the SCM's recovery actions use
     ([CLT-installed-service-and-state §17](CLT-installed-service-and-state.md)). Then `User=`
     set to the account of clause 1, and the directives of clause 8.
   - `[Install]`: `WantedBy=multi-user.target`.

   The user scope and the launchd plist stay as `service-manager` renders them. `service install`
   never writes, reads or removes anything under `/etc/systemd/system/<PRODUCT_NAME>.service.d/`.
   Drop-ins there belong to the operator, outrank the unit, and survive every re-registration.

   - **The second service's unit** is written the same way. `User=` names the process account.
     `ExecStart=` runs `run --agents <socket> --client-uid <uid>`. `RuntimeDirectory=` holds the
     socket. `TimeoutStopSec=` exceeds the time the second service takes to stop what it started.
     The directives are those of clause 8 for its account.
   - **Under launchd** the second service's plist is the one `service-manager` renders, with
     `UserName` naming the process account. launchd's default exit timeout of 20 seconds exceeds
     the time the second service takes to stop what it started. launchd keeps no runtime directory
     across a boot (`/var/run` is emptied). The socket so lives in
     `/Library/Application Support/<PRODUCT_NAME>-agents/`, a directory on a path only root
     writes. `service install` makes it, owned by the process account and its group at `0750`, and
     the install removes it with the second service's registration. The second service passes
     with `O_SEARCH` through directories it may only search. macOS has `O_SEARCH` from macOS 13
     on, and macOS 13 is the oldest macOS a Client runs on.
   - **Under the SCM** the second service runs under the process account,
     `NT SERVICE\<PRODUCT_NAME>-agents` or the account clause 16 names, set without a password.
     Its command line is `run --agents \\.\pipe\<PRODUCT_NAME>-agents --client-sid <SID>`, with
     the Client account's SID. It starts automatically. Like the Client's own entry, it carries
     the recovery actions of
     [CLT-installed-service-and-state §17](CLT-installed-service-and-state.md). That is one
     restart after the restart delay, repeated for every failure, and run on a reported failure
     too. It carries
     its display name and a Description. Its required privileges are `SeChangeNotifyPrivilege`
     alone (SERVICE_REQUIRED_PRIVILEGES_INFO). The SCM so removes every other privilege from its
     token, and so from every program it starts. No Managed Process therefore holds
     `SeImpersonatePrivilege`. A stop waits until the service has reported itself stopped and its
     process has ended, up to 60 seconds.

8. **A system unit carries only directives that neither set `no_new_privs` nor take away what a
   supported agent needs, and the account decides the set.**
   - **Every system unit** carries `ProtectControlGroups=yes`, which makes `/sys/fs/cgroup`
     read-only. It does not imply `NoNewPrivileges=`. None of the agents the core's kinds start
     writes the cgroup tree itself. A kind of its own states the same of its agent in its ADR
     ([CLT-wrappers-act-through-the-core §21](CLT-wrappers-act-through-the-core.md)).
   - **A unit that runs as root** (`User=root`) also carries `ProtectKernelTunables=yes`,
     `LockPersonality=yes` and `RestrictRealtime=yes`. Under root they imply nothing, because the
     unit holds `CAP_SYS_ADMIN`. They make `/proc/sys` and `/sys` read-only, fix the execution
     domain, and refuse realtime scheduling. No supported agent needs any of that.
   - **Under any other account these three are left out.** Each would set `no_new_privs` and
     break `sudo` and the setuid helpers the agents use.
   - **Left out everywhere**, each for its reason:
     - `NoNewPrivileges=`, for the same reason.
     - `ProtectSystem=` (`yes`, `full` or `strict`) and `RestrictSUIDSGID=`: a task that installs
       software, or a configuration-management run under the `command` kind, installs
       operating-system packages under root. A monitoring agent writes its own directory under
       `/etc`.
     - `ProtectHome=`: a root service's home is `/root`, and a log receiver may read under
       `/home`.
     - `PrivateTmp=`: a Managed Process may reach a local daemon through a socket under `/tmp`.
     - `MemoryDenyWriteExecute=`: it breaks every JIT a check or exec script may run (Java,
       `Node.js`, .NET, LuaJIT).
     - `RestrictNamespaces=`: it breaks `ip netns exec` around a check, and any sandboxing helper.
     - `CapabilityBoundingSet=`: it takes from root what root was chosen for, and from a setuid
       helper what it grants.
     - `ProtectKernelModules=`: it hides `/usr/lib/modules`, which hardware inventory reads.
     - `PrivateDevices=`: SMART, IPMI and sensor inputs read `/dev`.
     - `ProtectKernelLogs=`: `dmesg` readers need the kernel log.
     - `ProtectClock=`: it implies a device allow-list.
     - `ProtectHostname=`: a UTS namespace would freeze the host name the Client reports
       ([CLT-what-an-agent-reports §5](CLT-what-an-agent-reports.md)).
     - `ProtectProc=`: process inputs read other users' `/proc` entries.
     - `RestrictAddressFamilies=` and `SystemCallFilter=`: Managed Processes use netlink, raw and
       packet sockets and arbitrary system calls.

   An operator may add in a drop-in what a host can take (clause 7): `NoNewPrivileges=yes` where
   nothing uses `sudo`, `AmbientCapabilities=CAP_NET_BIND_SERVICE` for a port below 1024,
   `SupplementaryGroups=adm` for logs. An operator may remove in a drop-in what a host cannot
   take. The documentation shows each.

9. **No installer starts a service it registers anew; an upgrade starts again what ran.** The
   Linux post-install and the MSI register the service and leave it stopped. They depart
   knowingly from Debian Policy and `dh_installsystemd`. A Client with no configuration holds no
   client certificate and is refused at startup, at fleet scale where nobody watches a terminal.
   The exceptions are upgrades of a host whose services were running:
   - On Linux an upgrade restarts a running service, so a configured host finishes the
     upgrade on the delivered binary.
   - On Linux and macOS an install that names another process account than the registration,
     drops it, or names another `--run-as` stops the services first. Once it has succeeded, it
     starts again what ran (clause 26).
   - On Windows every `service install` stops both services before it changes any right or SCM
     entry, every MSI upgrade among them, with a process account or without one. Once it has
     succeeded, it starts again what ran before it. A first install finds nothing to stop and
     starts nothing.

    The post-install prints the two remaining steps:

    ```console
    sudo opamp-fleet service install --interactive
    sudo systemctl start opamp-fleet
    ```

    - **The second `service install` is a re-install.** It is idempotent, and writes the
      configuration that `service install` refuses to overwrite once it exists.
    - **The first step is `--interactive`, not `--endpoint` alone.** `--interactive` asks for the
      client identity and for the bootstrap identity found beside the program
      ([SEC-bootstrap-certificates-and-enrolment §14](SEC-bootstrap-certificates-and-enrolment.md)).
      The Client refuses at startup a configuration that holds only an endpoint. Printing
      `--endpoint` as the whole first step would present an unauthenticated Client as the default.
    - **The scripted form follows the two steps.** The post-install prints the scripted form of a
      packaged host's enrolment
      ([SEC-bootstrap-certificates-and-enrolment §19](SEC-bootstrap-certificates-and-enrolment.md)):
      `--endpoint` with `--fleet-files`, and for a `bootstrap.p12` `--bootstrap-password-file`.
      This is the way to enrol without a terminal.
    - **The manual alternative names the identity and no credential.** The alternative it prints
      for writing the file by hand names the endpoint together with `[tls] cert_file` and
      `key_file`, the identity the operator adds before the start. It names no credential and no
      `[auth]` section, because the Client reads none
      ([SEC-admission-by-client-certificate §3](SEC-admission-by-client-certificate.md)).

    On Windows the stop and the start go as follows. So nothing runs under an account whose rights
    the install changes or takes away, nor under a registration it rewrites:
    - **The stop comes before every change, and is waited for.** The install writes the
      configuration and the copies beside it while the Client still runs. It then stops the
      Client and then the second service, before it restricts, takes over, hands over or
      withdraws anything, and before it rewrites an SCM entry. It waits for each until the service
      has reported itself stopped and its process has ended (clause 7). The second service's
      programs end with it (clause 19). Where the second service cannot be stopped, the install
      starts the Client again if the stop stopped it, and changes nothing more.
    - **What ran is recorded before the stop.** The install asks the SCM which of the two run. A
      service starting or stopping counts as running. The install writes that, with a withdrawal
      that is due (clause 26), to the record of clause 27 in one write before it stops anything. A
      record that cannot be written stops nothing. The install adds what the stop then finds
      running beyond the record.
    - **A failed stop records what runs no more.** Where the stop fails, the install sets the
      services to start again from what the SCM reports then. Only a service running or paused
      counts as running then. The set is what an earlier install left to start again, plus what
      ran when this install looked or was stopped by it and does not run now:
      `before ∪ ((ran ∪ stopped) ∖ running now)`. The next install so starts again a service still
      stopping or starting, and waits it out. Where the SCM cannot be asked, the record keeps all
      of `before ∪ ran ∪ stopped`.
    - **Success starts again what ran.** Once the registration, the withdrawal, the hand-over and
      the layout's share are done, the install starts what is recorded. Where the Client ran, it
      starts the second service and then the Client. Where the second service alone ran, it
      starts that alone. It passes over a service that runs, and waits out one still starting or
      stopping, up to 60 seconds. Then it strikes the services from the record.
    - **A failed install leaves the services stopped, and says so.** Its message names how the SCM
      reports both now. It says that the next `service install` that succeeds starts again what
      ran, as the record names it, or that `service start` starts both now. Until then the
      services stay stopped, an MSI that rolled back among the causes.
    - **Only the CLI's `service start` and `service stop` hand the run state to the operator.**
      Once either has succeeded, it strikes the services from the record, so no later install
      starts what the operator stopped. `sc.exe` and the Services console know nothing of the
      record and strike nothing. A `service stop` that failed in part strikes nothing either.

10. **The MSI asks for the installation folder, the endpoint and the self-update consent, all as
    public properties, and stops and unregisters only on a real removal.**
    - **The dialogs.** The UI is a copy of the stock `WixUI_InstallDir` set with one dialog
      inserted: `WelcomeDlg → InstallDirDlg → EndpointDlg → VerifyReadyDlg`. It has **no licence
      page**: Apache-2.0 needs no click-through, and the dialog wants a second copy of `LICENSE`
      as RTF. The UI is a copy with the navigation re-pointed, as WiX's customization guide
      prescribes. Overriding a stock set's rows in place depends on control-event ordering the
      toolset may renumber.

    | property | dialog | effect |
    |---|---|---|
    | `INSTALLFOLDER` | `InstallDirDlg`, via `WIXUI_INSTALLDIR` | where the payload goes; default `C:\Program Files\<PRODUCT_NAME>` |
    | `ENDPOINT` | `EndpointDlg`, an `Edit` control | `service install --endpoint`; empty means none |
    | `SELFUPDATE` | `EndpointDlg`, a checkbox, default `1` | the self-update consent of [CLT-self-update-signed-package §3](CLT-self-update-signed-package.md) |

    - **`INSTALLFOLDER` is one directory the operator configures**, holding only the delivered
      payload. The layout and the state directory go to `%ProgramData%\<PRODUCT_NAME>`, and no
      root reaches the command line
      ([CLT-installed-service-and-state §12](CLT-installed-service-and-state.md)). An uninstall so
      empties the folder, and the configuration survives where Windows expects data.
    - **The properties are public.** All names are upper-case and listed `Secure`. Windows
      Installer resets private properties between the UI and execute sequences, and `Secure` lets
      the same MSI run unattended:

    ```console
    msiexec /i supervisor_1.2.3_windows_amd64.msi /qn ^
      ENDPOINT="wss://fleet.example.com/v1/opamp" SELFUPDATE=0
    ```

    - **Registration is a deferred type 18 custom action with `Impersonate="no"`**, after
      `InstallFiles`, since the executable it runs is the one being installed. Its formatted
      command line receives the properties without a `CustomActionData` round trip. It is two
      actions under opposite conditions on `ENDPOINT`. The loader rejects `--endpoint ""`, while
      no flag is the ordinary install that configures later.
    - **Stop and unregister run only on a real removal.** `service stop` and then
      `service uninstall` run best-effort, before `RemoveFiles`, under the condition
      `REMOVE="ALL" AND NOT UPGRADINGPRODUCTCODE`. A major upgrade removes the old product with
      `REMOVE` set to `ALL` as well, and sets `UPGRADINGPRODUCTCODE` while it does. A removal from
      Add or Remove Programs does not set it. `REMOVE="ALL"` alone would unregister the services in
      the middle of every upgrade, where the successor's `service install` registers them again
      (clauses 4 and 9).
    - **The `UpgradeCode` is minted once and never changed.** Windows Installer recognises 1.2.4
      as an upgrade of 1.2.3 by it, and a new one strands every installed host. It is paired with
      `MajorUpgrade`, and the `ProductCode` is regenerated per build.
    - **The MSI asks for no client identity.** The certificate and its key are files the operator
      puts on the host. `EndpointDlg` says that they are added to `supervisor.toml` afterwards, or
      through `service install --interactive` from an elevated prompt. It mentions no credential,
      because the Client reads none
      ([SEC-admission-by-client-certificate §3](SEC-admission-by-client-certificate.md)).

11. **No privileged step goes through a link the account can place.** The rule covers every
    read, write, rename, removal, mode change and change of the owning account by
    `service install` and `service config`. It covers them in the data root, the state directory
    and the layout root. These are the enrolment's files, the staged program and its manifest,
    and what `service config` re-owns.
    - **A directory is held.** A step opens the directory it works in without following a link at
      its own name, and holds it open while the step runs. A step reaches a directory below it one
      held name at a time: `versions/` and the version directory below the layout root.
    - **On Unix** a step opens, creates, renames and removes each name relative to the held
      descriptor, with `O_NOFOLLOW`. It reads a file only when it is a regular file.
    - **On Windows** a step refuses a directory or file that is a reparse point. It opens a held
      directory without `FILE_SHARE_DELETE`, so nobody can rename or replace it while it is held.
    - **A file is written whole under a new name and renamed over its own.** The step creates the
      new name exclusively (`O_CREAT | O_EXCL`, `CREATE_NEW`). On Unix it sets the mode through
      the file's descriptor. It syncs the file before the rename. The rename replaces a link at
      the name, and never writes through it. No file the Client keeps is stored under the new name.
    - **A program is executable from its creation**, never by a mode change after it.
    - **What `service config` re-owns** it re-owns through a handle on the entry itself, a link as
      a link, in a directory opened without following a link. It reaches the data root the
      administrator named as clause 13 says.

12. **A state directory that is itself a link is refused, at install and at service start.** The
    refusal names the directory. The same holds for `versions/` and a version directory below the
    layout root, and for any directory the Client keeps state in through the adapter of clause 15.
    The path up to the directory a step opens is followed:
    - For a state directory directly in a root, the default `<data root>/state`, that path is the
      root's own, which clause 13 settles.
    - For one deeper beneath a root, the components between the root and its parent are followed
      too. The check of what lies beneath a root
      ([CLT-installed-service-and-state §15](CLT-installed-service-and-state.md)) inspects them
      before the install writes anything.

13. **On Windows, a data root or layout root that is a reparse point is refused, and the data root
    is held.** The account holds delete on the roots themselves, so their names are not the
    operator's alone. `service install` and `service config` hold the data root without
    `FILE_SHARE_DELETE` from their first write to their last. On Linux and macOS a root's name
    lies in a directory only root writes, and is followed. This refusal is a check of its own,
    beside the check of what lies beneath a root
    ([CLT-installed-service-and-state §15](CLT-installed-service-and-state.md)), which does not
    inspect the roots.

14. **A file with a second hard link is skipped and named.** Another name for it may stand
    anywhere, and re-owning it would re-own that file too. The hand-over and `service config` each
    leave such a file as it is and print a warning naming it.

15. **The Client keeps its state through the same adapter at run time.** The Client reads and
    writes the following as clause 11 states, whoever runs it:
    - the instance identity;
    - the key, the request and the certificates;
    - the connection settings;
    - the installed-package record and the high-water marks;
    - the stored remote configuration with its entry files.

    The Client's run then refuses what the install refuses. The runtime creates a file it replaces
    as a new file. The new file belongs to the account that runs the Client. Where the replaced
    file was open to its owning account alone, the new file is too.

16. **The process account is named at registration, and kept.**
    `service install --process-account <account>` names it, at system scope only. The flag
    conflicts with `--user`. A re-registration without the flag keeps the account the second
    service's registration names (clause 17), read the way clause 1 reads the Client's. Without
    the flag and without such a registration, there is no process account and no second service.
    The Client then starts its programs itself, and clauses 23 and 24 hold on every host.
    - **The operator creates the account.** On Linux and macOS it must exist. The install refuses
      an account that does not exist before it writes anything. The message shows the
      `useradd --system` line that creates it, and on macOS the `sysadminctl -roleAccount` line.
      The binary never creates an account (clause 1).
    - **Windows accepts only passwordless forms**, as clause 1 does: the second service's own
      virtual account `NT SERVICE\<PRODUCT_NAME>-agents` (recommended), a gMSA, or
      `NT AUTHORITY\LocalService` / `NetworkService`. The install looks a gMSA up in its domain,
      and refuses one it cannot find before it writes anything.
    - **It is never root and never the Client's account.** The install refuses
      `--process-account root`, `LocalSystem` or the account the Client runs as. The refusal names
      `--run-as root` as the way to run everything as root. A host whose agents need root runs as
      root as a whole (clause 1), with no second service.
    - **On Windows the Client then runs under an account of its own.** A process account separates
      the Managed Processes from a Client that runs under its own account. The install so refuses
      a process account where the Client would run as `LocalSystem`. `--run-as` names the Client's
      account, or the registration keeps the one the SCM holds (clause 1). The refusal names
      `--run-as` as the way to name it, with `NT SERVICE\<PRODUCT_NAME>` where no account was
      named.
    - **It is dropped on request.** `service install --no-process-account` stops and deregisters
      the second service, and the Client starts its programs itself again. The flag conflicts with
      `--process-account`. A refusal that a kept process account causes names
      `--no-process-account`. The install withdraws from the dropped account what it was granted
      (clause 26).

17. **The service is `PRODUCT_NAME`, with no suffix, on every platform, and the second service is
    `<PRODUCT_NAME>-agents`.** The label is a single token, with no qualifier and no organization.
    `service-manager` so renders the same string for systemd, launchd and the SCM, and the name
    never contains a dot. The second service exists only where a process account is named
    (clause 16). Its label is the product's with the suffix `-agents`. The `PATH` symlink is
    `/usr/bin/<PRODUCT_NAME>`, so two variants never collide on one `PATH` entry. On Windows the SCM
    also carries each service's display name and a Description that says what the program does.
    - **It is the same program.** The second service runs `supervisor run --agents` from the same
      layout and `current` (clause 6), under the process account. The package still delivers one
      file (clause 4).
    - **The two are registered, started, stopped and deregistered together.** `service install`,
      `start`, `stop` and `uninstall` act on both. The second service comes first on start and last
      on stop. Under systemd the Client's `Wants=` and `After=` start it first. Under launchd and
      the SCM the start names it first. Its unit carries the directives clause 8 sets for its
      account. Its plist names the account in `UserName`. Under the SCM it is registered as
      clause 7 says.
    - **On Windows the Client's service does not depend on it.** The SCM starts no service whose
      dependency failed to start (StartService, `ERROR_SERVICE_DEPENDENCY_FAIL`). A dependency
      would so hold the Client down with a second service that does not come up. No SCM entry
      names a dependency. `service start` starts the second service first and then the Client.
    - **The Client runs without it, and neither waits for the other.** Under systemd the Client's
      unit wants the second service and is ordered after it (`Wants=`, `After=`), never
      `Requires=`. While the second service is down, the Client stays reported to the Server. Its
      Supervisors say that their programs do not start, and start them again once it is back. A
      Supervisor whose start finds the second service unreachable is not held for failing to
      start. It tries again with backoff. The Client connects again whenever the second service
      has restarted. A program the second service stops because it is stopping itself has not
      exited on its own, and counts for no restart policy.
    - **What arrives while the second service is away waits for it.**
      - The Client stages and keeps an offered package, and never swaps it in while the second
        service is away. It swaps the package in, and the apply grace runs, once it reaches the
        second service again. The previous program so stays in place, and a rollback only ever goes
        back to a program that ran.
      - The Client writes a delivered Configuration as usual. The first start of an apply, or of
        the retry, that reaches the second service and survives the apply grace gives its
        acknowledgement.
      - A restart command's start acknowledges nothing and swaps nothing in
        ([CLT-wrappers-act-through-the-core §5](CLT-wrappers-act-through-the-core.md)). The retry
        follows it at once.
      - A Supervisor held after a package that failed leaves no Configuration waiting: it is
        reported `FAILED`.
      - Until then the package stays `Installing` and the Configuration `APPLYING`, each naming the
        second service as the reason.
      - A newer offer or Configuration replaces the waiting one. A Client that stops while one
        waits leaves its files as the record names them.
    - **A self-update restarts whichever end is behind.** The self-update switches `current` and
      restarts the Client. A second service exits when it meets a Client of another build while
      `current` no longer names its own version. Its service manager then starts it from `current`.
      A Client that meets a second service of another build, while `current` no longer names its
      own version, exits for the same restart. Each end otherwise refuses a peer of another build
      (clause 20). Only a peer of the admitted account can cause either exit (clause 19).

18. **Every program a Supervisor starts is started by the second service.** Where a process
    account is registered, the Managed Process, the version probe and the preflight of a staged
    package run under the process account, through the second service. The Client starts none of
    them itself.

19. **The channel admits the Client's account alone, the Client speaks to the second service
    alone, and the channel ends with the Client.** On Linux and macOS the second service listens on
    a Unix socket, on Windows on a named pipe. The second service checks every connection by the
    peer's credentials (`SO_PEERCRED`, `getpeereid`, the pipe client's token). It refuses the
    connection unless its user is the Client's account. It serves one connection at a time. When
    the connection closes, the second service stops every program it started: by its process group
    on Unix, by its job on Windows. A Client that restarts so finds no process it did not start.
    - **The pipe is the process account's, and the Client reaches it for reading and writing data
      alone.** The pipe is `\\.\pipe\<PRODUCT_NAME>-agents`. Its security descriptor names the
      process account as the account that owns it. It carries a protected DACL of two entries:
      - full access for the process account, which creates the pipe's instances;
      - read and `FILE_WRITE_DATA` alone for the Client's account (`0x12008b`).

      The Client is granted neither `FILE_GENERIC_WRITE` nor `FILE_APPEND_DATA`, which is
      `FILE_CREATE_PIPE_INSTANCE` on a pipe, so it cannot serve the pipe itself. Nobody else is
      granted anything.
    - **The pipe cannot be taken over.** The second service creates its first instance as the
      first of its name (`FILE_FLAG_FIRST_PIPE_INSTANCE`). A pipe someone else made under the name
      first so fails the second service's start. Every instance rejects remote clients
      (`PIPE_REJECT_REMOTE_CLIENTS`).
    - **Each end checks the other's account.** The Client opens the pipe at the identification
      level (`SECURITY_IDENTIFICATION`), so the second service may identify it and act as nobody.
      The Client accepts the pipe only where the process account owns it. On Unix the Client
      accepts a peer whose uid is the process account's. The second service reads the client's
      token by impersonating it at that level, and refuses a client that is not the Client's
      account. It refuses to run as `LocalSystem`, to serve `LocalSystem`, or to serve its own
      account.
    - **The Client checks the process too.** Before it writes anything on a new connection, the
      Client reads the process at the other end:
      - on Linux by `SO_PEERPIDFD`, and by `SO_PEERCRED` only where the kernel does not know that
        option (`ENOPROTOOPT`); any other error is a process that cannot be read;
      - on macOS by `LOCAL_PEERPID`;
      - on Windows by `GetNamedPipeServerProcessId`.

      The Client then asks the service manager for the process it runs as the second service. It
      asks systemd for the unit's `MainPID`, launchd for the job's `pid`, and the SCM for the
      service's process id from `QueryServiceStatusEx`. It accepts the connection only where both
      name the same process. With a pidfd, it also requires that the process the pidfd refers to
      still runs once the service manager has answered. The id so cannot pass to another process
      in between. On Windows the Client opens the process `GetNamedPipeServerProcessId` names with
      `SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` before it asks the SCM. It holds that
      handle until it has decided, and requires that the process has not ended once the SCM has
      answered. While the handle is open, the id cannot pass to another process. A process the
      Client cannot open is one that cannot be read. The Client still reads the account as the
      bullet above says, from the peer's uid or from the account that owns the pipe. The Client
      refuses a connection whose process cannot be read, or that the service manager names no
      process for, as one whose process differs.
    - **A refused peer learns nothing.** The Client closes a connection it refuses before it sends
      the handshake or any request. It logs an error naming the socket or pipe and the peer's
      process id. It treats the second service as away. It tries again with the backoff it uses
      for a second service that is not running, each time on a new connection checked anew. The
      Supervisors report the second service unreachable as they do then (clause 17).
    - **The second service closes its process to the Managed Processes where it can.**
      - On Linux it marks itself not dumpable (`PR_SET_DUMPABLE` 0) as the first thing its program
        does, before it reads its configuration or sets up its log. It does not listen while
        `/proc/self/status` names a tracer (`TracerPid` other than 0). From then on no process of
        its own account without `CAP_SYS_PTRACE` can attach to it, read or write its memory, or
        take its file descriptors, whatever `kernel.yama.ptrace_scope` says. The programs it starts
        are dumpable as usual, since `execve` resets the mark.
      - On macOS the platform already refuses `task_for_pid` to a caller of the same user without
        the debugging entitlement.
      - On Windows it gives its process and every one of its threads a protected DACL as the first
        thing each does. This comes before the program reads its configuration or sets up its log
        (below).
    - **On Windows the process account may only ask about and wait for the second service.**
      - The process carries
        `D:P(A;;0x101000;;;<process-account>)(A;;0x101000;;;<Client>)(A;;RC;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)`.
        That is `SYNCHRONIZE` and `PROCESS_QUERY_LIMITED_INFORMATION` for the process account and
        for the Client's account, and `READ_CONTROL` alone for the account that owns the process
        (`OWNER RIGHTS`), which so holds no implicit `WRITE_DAC`. `LocalSystem` and the
        Administrators get full access.
      - Each thread carries
        `D:P(A;;0x100800;;;<process-account>)(A;;RC;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)`: `SYNCHRONIZE`
        and `THREAD_QUERY_LIMITED_INFORMATION` for the process account.
      - A Managed Process holds no `SeDebugPrivilege`. It so can neither read nor write the second
        service's memory, start or steer a thread in it, take its handles, suspend it, nor end it.
      - The second service sets the DACLs through `SetSecurityInfo` on the process's and each
        thread's own handle. The main thread sets the process's and its own. It sets, from a
        snapshot, those of the threads that exist by then. The thread the SCM starts for the
        service, and every thread of the runtime, set their own as their first act.
      - The second service sets the DACLs of threads the system starts in the process, such as
        those of its thread pool, from a snapshot of every thread. It takes the snapshot again once
        a second while it serves.
      - Before it listens, the second service sets the DACLs once more. It then reads back the DACL
        of its process and of every thread it has, compared by what they say as clause 21 compares
        an ACL. While one differs, it does not listen, and ends with an error.
      - `GA` stands for the access it maps to on a process and on a thread, `0x1fffff`. That value
        is what the second service writes, so that what it reads back compares equal.
      - The second service leaves the token's default DACL as it is. The programs it starts are
        created with the security their own account gives them, and so are not touched.
    - **Each program runs in a job of its own.** On Windows the second service starts every program
      suspended. It assigns the program to a new job object with
      `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and only then lets it run. Everything the program starts
      is so in that job too. At the end of each connection, and so before it reports itself
      stopped, the second service ends every job it holds. It waits, up to five seconds, until no
      job holds a process.

20. **The requests are the ones this clause lists, and both ends are one build.** The Client may
    ask the second service:
    - to start a program with its arguments, environment and working directory;
    - to signal or stop a program, and to report its exit;
    - to remove what the process account wrote under a Supervisor's directory when that Supervisor
      is purged.

    The second service streams each program's output back. The programs run with the second
    service's own umask, never one widened for the Client's sake. The messages are private to the
    build. Both ends run from `current`, and each refuses a peer that states another version.
    Windows has no signals. There the second service refuses a signal, and a stop ends the
    program's job with everything in it. A kind that reloads its agent by a signal restarts it
    there instead, as it does without a process account.

21. **The process account reads what it runs and writes only its own data.** On Linux and macOS
    the operator makes the process account's primary group a supplementary group of the Client's
    account. The group is the process account's own. The install refuses to register where:
    - the group is not a supplementary group of the Client's account;
    - the process account's primary group is group 0 or the Client's own primary group;
    - the group has a member other than the Client's account;
    - the group is the primary group of an account other than the process account;
    - the process account is a member of the Client's primary group.

    Every file and directory the Client keeps under `state_dir` that the table does not name, its
    logs among them, stays the Client's alone. Then:

    | Path | Owning account : group | Mode |
    |---|---|---|
    | `state_dir`, each Supervisor's directory | Client : the process account's group | `0710` |
    | the Client's key, certificates, `remote-config.pb`, `instance-uid`, package state and staging | Client | `0600` / `0700` |
    | `program/` and its files | Client : the process account's group | `0750`, files without group or other write |
    | the preflight's staging directory | Client : the process account's group | `0750` |
    | `config/` and its files | Client : the process account's group | `0750` / `0640` |
    | a kind's data directories | Client : the process account's group | `2770` |

    The Client creates a file it replaces through the no-follow adapter (clause 15) with the owning
    account, group and mode this table names.

    On Windows the install makes the same split by ACL grants to the process account's SID. It
    checks or shares no group:

    | The process account | Grant on the directory itself | Grant on what it holds |
    |---|---|---|
    | passes through (`state_dir`, each Supervisor's directory) | none: it passes by `SeChangeNotifyPrivilege` (clause 7) | none |
    | reads (`program/`, the preflight's staging, `config/`) | read and execute, `(OI)(CI)` | read and execute |
    | writes (a kind's data directories) | modify without `DELETE` | modify, `(OI)(CI)` and inherit-only on the directory, explicit on each entry |
    | runs (`versions\` of the layout) | read and execute, `(OI)(CI)` | read and execute |

    Every grant is explicit on what exists and inherited by what is created after it. It replaces
    the explicit allow entries the process account held there. No other entry grants the process
    account anything by name, the Client's key and logs among them. In a root the install takes
    over, nothing grants it anything through a group either. A root the operator named keeps the
    explicit entries it had.

    - **Every walk goes one handle at a time.** A walk opens each entry as itself, never through a
      symbolic link or junction, and enters none. It holds a directory on the way open without
      sharing delete, so nobody can rename or replace it under the walk. It skips a regular file
      with a second hard link and names it in a warning, since its other name may stand anywhere
      on the volume. A walk that grants, hands over (clause 1), takes over or cuts off leaves an
      entry the process account owns as it is, with everything beneath it.
    - **What a walk cannot hold, it either fails on or reports.** A walk that takes rights away
      fails where it finds a directory it cannot hold, or an entry it cannot open. Such a walk is a
      withdrawal (clause 26), or the take-over or cut-off of a root a system-scope install
      restricts to `LocalSystem` and the Administrators. So it leaves nothing it cannot reach
      safely as it was. A walk that only adds rights, a grant or the hand-over, skips such an entry
      and names it in a warning.
    - **Every DACL set is read back.** A walk sets a DACL on the entry's own handle, and the change
      reaches that object alone. The walk itself applies what a directory passes on to what it
      already holds. The walk reads back the DACL the object then holds and compares it with what
      it set, by what it says. The comparison asks for the same entries in the same order and the
      same protection, however they are spelled. A difference fails the change.
    - **The install's walks hold the backup, restore and take-ownership privileges** for as long
      as they run. They so open an entry another account made, or one a Managed Process protected
      from the Administrators.

    An install grants and withdraws the process account's rights in the data root, the layout
    root and the state directory alone. A Supervisors' root outside them is the operator's
    ([CLT-installed-service-and-state §16](CLT-installed-service-and-state.md)). The install
    leaves it as it is and names it in a warning.

22. **A program inherits nothing of the Client's.** Its environment is the second service's own,
    then the block's `env` laid over it, then the Supervisor Endpoint's token, last
    ([CLT-modes-and-gateway §28](CLT-modes-and-gateway.md)). On Linux and macOS the second
    service's own environment has `HOME`, `USER` and `LOGNAME` set to the process account's. On
    Windows it is the environment the SCM gives the second service under the process account,
    unchanged.

23. **Modes come from a `.tar.gz` on Unix, never with write for group or others; the program is
    always made executable.** The Client applies a tar's member modes, each without the group's
    and others' write bits. It does so on every host, whether or not a process account is
    registered. A member written `0777` or `0666` lands `0755` or `0644`. No account but the
    Client's can so rewrite what a Supervisor runs. A `.7z` or `.zip` carries Windows attributes,
    so the Client takes no mode from it. The Client sets the program, the single file or the
    member at `program_path`, to `0755` regardless. Whether it runs so never depends on how the
    archive was built. An agent with further executables beside its program ships as `.tar.gz`.

24. **One directory per Supervisor, under a relocatable root, never on an entry of the
    Client's.** The top-level key `supervisor_dir` defaults to `<state_dir>/supervisors`. The
    Client makes it absolute at load. Under it each Supervisor owns `<supervisor_dir>/<name>/`,
    holding everything it needs:

    ```
    <supervisor_dir>/<name>/
      instance-uid            # its Agent identity
      remote-config.pb        # the last received configuration
      installed-package.json  # what is installed
      config/                 # the written configuration entries
      program/<file>          # the Managed Process — or program/tree/<program_path> for a tree
      packages/               # this Supervisor's download staging
    ```

    - **One knob moves state and program together.** Staging beside `program/` makes a raw
      artifact's install a rename on one filesystem.
    - **The directory is `program/`, not `bin/`.** It holds a whole tree for a multi-file package
      ([PKG-signed-packages-and-delivery §56](PKG-signed-packages-and-delivery.md)).
    - **A move migrates nothing.** Moving `supervisor_dir` on a running host leaves the old tree
      behind. Each Supervisor then registers as a new Agent.
    - **No entry of the Client's.** Where `supervisor_dir` is `state_dir`, no Supervisor may be
      named after an entry the Client keeps there itself: `instance-uid`, `config`, `packages`,
      `logs`, `gateway-packages`, `supervisors`. Otherwise `supervisor_dir` must not be or hold
      `state_dir`. The Client refuses at startup a block that would break either rule. An offered
      set that carries one fails as a whole
      ([CLT-server-manages-the-supervisor-set §2](CLT-server-manages-the-supervisor-set.md)).
    - **On every host.** The rule holds with a process account or without one. A purge of such a
      Supervisor would delete the Client's own entry, and its share would expose that entry to the
      process account's group.
    - **The changelog names the rename** this rule asks of a host that has such a Supervisor.

25. **An orphaned directory is reported at startup, never reaped, except a tombstone, which is
    reaped.**
    - **An orphaned directory** is one under the root that no block names. A purge cut short
      leaves one, and so do a block removed by hand while the Client was down and a moved root.
      The Client logs it as a warning with its path and leaves it in place. Startup cannot tell a
      leftover from a block temporarily commented out. Deleting would cost that Agent its identity
      and program. Removing it is the operator's call.
    - **A tombstone is different.** A purge first renames the Supervisor's directory to
      `.purge-<name>-<nonce>`, a name no block can carry. The name itself so records that the
      Client meant to remove it. The Client removes a tombstone the purge failed to delete again at
      the next start and with the next purge. Where a process account is registered, it does so
      through the second service. It logs the tombstone as a warning until it is gone.

26. **A former account keeps no share.** An install may drop the process account
    (`--no-process-account`), name another, or name another `--run-as` for the Client. It then
    withdraws every share with the former account in the roots an install acts on (clause 21), on
    every platform. A file with a second hard link is the exception, and the install names it in a
    warning (clause 21). A Supervisors' root outside those roots stays the operator's.
    - **On Linux and macOS a change of the owning account withdraws it.** The hand-over (clause 1)
      gives what the former account owns there, and what is in its group, to the Client's account
      and group. It leaves to the process account named now what that account keeps.
    - **On Linux and macOS an account change stops the services first.** This applies where the
      registration names a process account and the install names another or drops it
      (`--no-process-account`), or where `--run-as` names another account than the registration.
      - The install stops the Client and then, where one is registered, the second service. It does
        so before it rewrites a unit or plist or changes an owning account. It waits for each until
        its process has ended, up to 60 seconds each, as clause 7's stop does. A service whose
        process has not ended by then has not stopped. So nothing runs under an account whose files
        are being re-owned.
      - What ran is what systemd reports active or activating, and what launchd reports loaded with
        a process.
      - Once the install has succeeded, it starts again what ran of what is still registered, the
        second service first (clause 17).
      - An install that changes no account, such as one that adds a process account where none
        was, stops nothing beyond what clause 9 says.
      - **A failed stop changes nothing more.** Where a service cannot be stopped, the install
        starts again what it stopped, changes nothing more, and says so.
      - **A failed install leaves the services stopped, and says so.** Its message names which
        services ran before it and that `service start` starts them. Unix keeps no record of them,
        so no later install starts them; `service start` does.
    - **On Windows its entries are removed and what it owns is taken over.** The install removes
      every entry that allows the former account anything, explicit or inherited. It does so on
      every entry of the layout root, the data root, the state directory, the configuration file
      and the fleet files copied beside it. It gives an entry the former account owns to the
      Administrators. An entry that denies the former account stays. The walk goes as clause 21
      says, and fails closed. The install never withdraws `LocalSystem`: it keeps full control of
      every root.
    - **Former accounts are resolved before anything changes.** The install reads the accounts the
      two SCM entries name. It resolves each that is to go to its SID before it writes anything.
      One that cannot be resolved, such as a gMSA whose domain cannot be reached, fails the install
      there, since its grants would otherwise stay. The install withdraws neither an account that
      stays named nor one named now in the same part.
    - **The withdrawal is recorded before the SCM forgets the account.** The install writes it,
      with the SIDs and the roots, to the record of clause 27. It does so before the stop of
      clause 9, and so before it switches the SCM entries. It carries the withdrawal out after the
      switch. That comes before the hand-over and the layout's share, which grant what the accounts
      named now are to hold.
      It strikes the withdrawal from the record only once all of it has succeeded. An install that
      finds a withdrawal recorded completes it.
    - **An uninstall always deregisters, and withdraws only once both services are stopped.**
      `service uninstall` on Windows stops both services and waits for each, starting neither
      again. Where it holds the install lock (clause 27) and both are stopped, it completes a
      recorded withdrawal and removes the record. Otherwise it leaves the record as it is, and says
      that the next `service install` or `service uninstall` completes it. Either way it then
      deregisters both services, so none is left to start at the next boot.

27. **On Windows the install keeps what it left to finish, and runs one at a time.**
    - **The record.** What an install left to finish is the string value `Pending` of
      `HKEY_LOCAL_MACHINE\SOFTWARE\<PRODUCT_NAME>\WithdrawalPending`. It names the former accounts
      by part and SID, the roots to withdraw them from, and the services to start again. The
      install creates the key owned by the Administrators (`O:BA`). The key carries a protected
      DACL that grants `LocalSystem` and the Administrators full control and nobody else anything
      (`D:P(A;;KA;;;SY)(A;;KA;;;BA)`). The install reads and writes the key only where
      `LocalSystem` or the Administrators own it and it carries that DACL. A key that does not
      fails the install. The install then changes nothing until an Administrator has looked at the
      key and removed it. Nothing the Client or a Managed Process may write holds the record. Unix
      withdraws by a change of the owning account and keeps no record.
    - **The install lock.** The mutex `lock` in the private namespace `<PRODUCT_NAME>-install`
      serialises `service install`, `service uninstall`, `service start` and `service stop`. The
      namespace's boundary holds the Administrators' SID and the high integrity level, so only an
      elevated Administrator or `LocalSystem` can create it. The namespace and the mutex both carry
      `O:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)`. Only the process that created the namespace holds the
      lock. A process that finds the namespace there waits on its mutex until the creator lets it
      go. It then begins again in a namespace of its own. It does so for up to 300 seconds in all,
      after which it fails, saying that another run holds the lock. The install refuses a mutex in
      the namespace that anyone but `LocalSystem` or the Administrators owns.
    - **An uninstall without the lock still stops and deregisters.** It leaves the record alone,
      since another run may be rewriting it, and says so.

28. **A privileged enrolment connects where its own run answered.** The enrolment of
    `service install` and `service config` uses the endpoint and CA its own run answered, held in
    memory. It reads either from `supervisor.toml` only where the run answered none.
    - **An answered endpoint**, the questionnaire's answer or `--endpoint`, is the endpoint the
      enrolment connects to. The enrolment does not read the file back for it.
    - **An answered CA** is held in memory as the certificates the run copied beside the
      configuration. The enrolment trusts these, and does not read the copy again. An answer that
      names no CA leaves the built-in roots, whatever the file says by then.
    - **`--endpoint` over a kept file** answers nothing, because the file stands for the service.
      The enrolment must not run when the kept file names another endpoint than `--endpoint`. The
      install then fails and names both endpoints. Where they are the same, the run enrols as one
      that answered nothing.
    - **A run that answered nothing** takes both from the file, read once as clause 11 says. This
      is a residual, and the documents that describe the install say so.

**Out of scope:**

- macOS sandboxing, and an account of the macOS Client's own. Its first registration stays root,
  since macOS has no virtual account and a package would have to create an account there.
- Account creation by the binary; a `sysusers.d` entry; directives for user-scope units; a
  confined SELinux policy module
  ([CLT-installed-service-and-state §12](CLT-installed-service-and-state.md)).
- Moving a host that runs as root or `LocalSystem` to the unprivileged account automatically; an
  operator does that with `--run-as`. The MSI's command lines, which name no account and so take
  the default of clause 1.
- The trust boundary an administrator crosses when running the account-owned binary through the
  `PATH` symlink (see *Consequences*).
- Which directory is the state directory, and whether `supervisor.toml` may name it
  ([CLT-installed-service-and-state §14](CLT-installed-service-and-state.md)).
- The `[tls]` files and the CA an operator names, read where their paths point.
- `[logging] dir`, which only the service's own run uses.
- An account per Supervisor; making a process account the default where none is named; job
  objects for a host without a process account, the Client's own Managed Processes among them;
  what the Managed Processes can read of one another, which they share by this decision; a
  compromised Client, which can place any program under `program/`.
- On Windows:
  - the second service's process from its creation until its program's first act;
  - a thread the runtime creates, from its creation until its own first act;
  - a thread the system starts, until the next snapshot, at most a second later;
  - a broker outside the job, such as a WMI process creation or a scheduled task, through which a
    Managed Process outlives its job.
- On Linux where Yama is absent or `ptrace_scope` is 0, a Managed Process that kills the second
  service and attaches to it between its `execve` and the mark. Its service manager starts the
  killed second service again. As a tracer it can also make the second service read a
  `TracerPid` of 0.
- A process id reused:
  - on Linux before 6.5 and on macOS, where the platform gives the Client no handle that pins the
    peer's process;
  - on Windows before the Client opens the process the pipe names.
- The second service noticing that its socket was replaced.
- A socket the service manager creates and owns (socket activation).
- A peer that is the second service's process but runs a build other than the Client's, which
  the handshake already refuses (clause 20).
- For the enrolment of clause 28: the file the service runs with afterwards, which stays the
  account's (clause 21); `[tls] cert_file` and `key_file`, which an enrolment does not present
  while a bootstrap identity stands in; whether an unanswered run enrols at all.

## Alternatives considered

- **Root or `LocalSystem` only.** Least privilege is the requirement. Rejected.
- **Root as the packaged Linux default.** It is the smallest choice, and every agent works. But
  every packaged host then hands a Server-delivered command root, and least privilege stays
  something an operator must know to ask for. Rejected.
- **`LocalSystem` as the Windows default, relying on the process account.** The process account
  needs the second service, is opt-in, and is refused under a `LocalSystem` Client. A host without
  it would keep `LocalSystem` as its bound. Rejected.
- **`NT AUTHORITY\LocalService` or `NetworkService` as the Windows default.** Many services share
  both, so a group or ACL granted to the Client is granted to all of them, and a file one of them
  writes is the Client's. The virtual account is a SID of this service alone. Rejected.
- **Windows accounts with a password.** The password would stand in the process list and the
  installer log. Rejected for the passwordless forms of clause 1.
- **An MSI property that names the account** (`RUNAS=`). Every later MSI command line, an upgrade
  included, would have to leave it out, or it would override what an operator chose with
  `--run-as` since. The binary's default reaches the `.zip` install too. Rejected.
- **Move every root or `LocalSystem` host to the unprivileged account on its next upgrade.** The
  fleet would be hardened sooner. But a Managed Process that needs root or `LocalSystem`, such as
  a full inventory, a service it installs, or the Security log, would stop working on an upgrade
  nobody chose. The registration would also have to tell a chosen `LocalSystem` from a default an
  older registration took, which the SCM does not record. Rejected.
- **The install adds the Windows account to *Event Log Readers* and *Performance Monitor
  Users*.** It would grant every host rights only some need. An install that changes group
  membership would have to withdraw it when the account changes, as clause 26 does for ACLs.
  Linux leaves `SupplementaryGroups=` to a drop-in (clause 8). Rejected for clause 2.
- **A root-owned layout with the self-update disabled under `--run-as`.** It breaks G-10 and G-11
  for exactly those installs. Rejected.
- **A privileged updater.** A second service, an IPC surface and a privilege boundary for one
  flag. Rejected.
- **Creating the account in `service install`.** Three platform APIs for what packaging does in
  one line. Rejected for clause 3.
- **A wrapper script at `/usr/bin`, or `service install` writing `/usr/bin` itself.** A second code
  path does what one symlink does. An application writing into the package manager's directory
  crosses the line of what the package owns, and a custom-root install must not touch `/usr/bin`.
  Rejected for clause 5.
- **Hand the Client's account everything in both roots.** A re-registration, every package upgrade
  among them, would take from a Managed Process the data the process account wrote or reads.
  Rejected for clause 1.
- **Apply a tar's member modes as they are.** A member written `0777` or `0666` would let an
  account other than the Client's rewrite what a Supervisor runs. Rejected for clause 23.
- **Never reap an orphaned directory, a tombstone included.** A tombstone's name records that the
  Client meant to remove it, and a purge cut short would leave it for the operator. Rejected for
  clause 25.
- **systemd `DynamicUser=`.** The UID is allocated per start, and the state moves under
  `/var/lib/private`. An account that does not persist cannot own the layout root
  `/opt/<PRODUCT_NAME>`, so the self-update would end. `NoNewPrivileges=` and `RestrictSUIDSGID=`
  are implied and cannot be turned off, which breaks `sudo` and setuid plugins. The persistent
  identity of the installed service rules it out too
  ([CLT-installed-service-and-state §13](CLT-installed-service-and-state.md)). Rejected.
- **Only sandbox, and keep root.** This confines a root service a little. Under root the
  directives that would matter (`ProtectSystem=`, `NoNewPrivileges=`, `CapabilityBoundingSet=`)
  are the ones that break the agents root was kept for. The account is what bounds a
  Server-delivered command. Rejected as the whole answer; clause 8 keeps the root tier.
- **The Server's unit (`docs/manual/setup.md`): `ProtectSystem=strict`, `NoNewPrivileges=`,
  `MemoryDenyWriteExecute=` and the rest.** It fits a program that writes one directory and spawns
  nothing. The Client starts other programs whose needs it does not know. Rejected.
- **One directive set for every account.** Either the root tier is lost, or `no_new_privs` arrives
  for the account through an implied setting. Rejected for the two tiers of clause 8.
- **Add the directives through a drop-in that `service install` writes, and keep
  `service-manager`'s unit.** Two files to keep in step. `service uninstall` would leave a
  directory `service-manager` does not know, and the Client's drop-in would share a directory with
  the operator's. It also leaves the unquoted `ExecStart`. Rejected.
- **A flag that names the account only for a first registration (`--run-as-if-new`).** This keeps
  the check in Rust. But it is a public flag forever, for the one caller that can test for a file.
  Rejected; clause 3's test reads the same file that clause 1 reads.
- **Default the binary to the account `<PRODUCT_NAME>` wherever it exists.** An archive install
  would then depend on what some earlier package left behind. Rejected.
- **Read the account from `systemctl show -p User`.** This sees drop-ins. But it needs a running
  manager, which a container image build or a chroot does not have, and it reports what is
  loaded, not what was written. Rejected.
- **Leave hosts whose unit lost its account as root.** Every packaged host installed with
  `--run-as` and upgraded once would stay root without saying so. Rejected for the data-root rule
  of clause 1.
- **Remove the account on purge.** Files it owns elsewhere would pass to a later account with the
  same uid. Debian and Fedora packaging practice both leave system accounts in place. Rejected.
- **A `sysusers.d` file in the package.** The declarative form that Fedora's packaging guidelines
  prefer. It is a second packaged file beside the one clause 4 allows, and Debian's tooling needs
  its own trigger for it. Not taken now.
- **`AmbientCapabilities=CAP_NET_BIND_SERVICE` by default.** It would give every Managed Process
  on every host a right few hosts need. Rejected; a drop-in adds it where it is needed.
- **Start a service the installer registers, as Debian Policy and `dh_installsystemd` expect.** A
  Client with no configuration holds no client certificate and is refused at startup, on hosts
  nobody watches. Rejected for clause 9.
- **Re-register on Windows without stopping the services, as Linux restarts only what runs.** A
  running service would go on under an account whose grants the install takes away, or under a
  registration it rewrites. Rejected for clause 9's stop.
- **Stop and unregister the services on every `REMOVE="ALL"`.** A major upgrade removes the old
  product with `REMOVE` set to `ALL` too, so the services would be unregistered in the middle of
  every upgrade. Rejected for the condition of clause 10.
- **Do the privileged writes as the account.** Fork, drop to the account's uid, and write. The
  enrolment runs the whole connection engine, which does not fork cleanly in a multi-threaded
  runtime. Windows has no equivalent short of a second process under the account's token.
  Rejected as not simpler.
- **Check with `lstat` before each write.** It leaves a window between the check and the write,
  which an account looping on the name wins. Rejected.
- **`openat2` with `RESOLVE_NO_SYMLINKS` or `RESOLVE_BENEATH`.** Linux only, since 5.6; macOS and
  Windows still need their own path. Rejected for one mechanism on every platform.
- **Refuse every link on the whole path, from `/` down.** That breaks the links platforms ship
  (`/var` on macOS) and directories an operator placed above the roots, for no gain: those
  components lie in directories only root writes. Rejected; clause 12 follows them.
- **Keep the runtime on plain path operations, and go no-follow only during an install.** The
  runtime would then accept a state directory the install refuses, and two code paths would read
  the same files. Rejected for clause 15.
- **Follow a link at a state directory, for operators who placed one.** That is the link the
  account can plant. Rejected; the operator names the real path instead.
- **A crate for capability-style directory access (`cap-std`).** It covers both platforms, but
  adds a dependency for what about twenty `libc` calls and `std` already do. Not taken now.
- **Let the Client switch accounts itself.** On Linux it needs `CAP_SETUID`, `CAP_SETGID` and
  `CAP_KILL` in its unit, which permit uid and gid 0, so a compromised Client is root. Every child
  must also clear the ambient and inheritable sets, or it inherits them. On macOS the Client must
  be root. On Windows a restricted token is unproven for the agents' network and RPC use. Rejected
  for making the Client root-equivalent.
- **A setuid-root program bound to the process account.** It is a second file in the package,
  against clause 4, lives outside the layout the self-update rewrites, and is the classic target.
  Rejected.
- **A root parent that keeps privileges and drops them for the Client and the processes**, as
  nginx and postfix do. It is a long-running root process, which the Client's own account exists
  to avoid. Rejected.
- **Transient units through systemd's D-Bus API, or `sudo -u`.** The first needs the polkit right
  to manage units, which cannot be narrowed to one account and exists on Linux only. The second
  does not relay `SIGKILL` (sudo(8)). Rejected.
- **An account per Supervisor.** It also separates the Supervisors from one another, but needs one
  service or one privilege per account. Not taken now.
- **Keep the key out of the processes' reach instead** (kernel keyring, TPM, a key service). It
  protects the key alone, and leaves every other Supervisor's `config/` and the program
  directories writable. Rejected.
- **Create the process account in the packages and by `service install`.** The packages could
  create it as they create the Client's (clause 3). Not taken: the operator names and creates it.
- **Make the Client's SCM entry depend on the second service.** It would order the start without
  the CLI. But the SCM starts no service whose dependency failed, so a second service that does
  not come up would hold the Client down. Rejected: the Client runs without it (clause 17).
- **A mutex under `Global\` as the install lock.** Any account may create an object of that name
  first and hold it. Rejected for the private namespace of clause 27.
- **The record in the data root or beneath the service's SCM key.** The Client may change the data
  root's DACL where a process account is registered, and a later `--data-root` may name it
  differently. The SCM's key carries the system's own, undocumented DACL and goes with
  `sc delete`. Rejected for the install's own registry key.
- **A socket the service manager owns.** systemd (`ListenStream=` in a socket unit) and launchd
  (`Sockets`) can create the socket as root in a directory the process account cannot write, and
  hand it to the second service. A Managed Process then cannot replace it. That closes the
  replacement on Linux and macOS without asking for a process. It does not close a Managed Process
  reaching into the second service, and Windows has no such mechanism for a named pipe, so the
  Client would need the process check there anyway. Two mechanisms, and a change to the units, the
  plist, the socket's place and the command line. Not taken.
- **The second service checks that its socket is still its own** and binds it again where it was
  replaced. It narrows the window but does not close it, since the impostor can replace the socket
  again between two checks, and it does nothing on Windows. Rejected; the Client's check closes
  the replacement.
- **A secret only the Client and the second service share**, a token the second service proves at
  the handshake. The second service would have to keep it where its own Managed Processes, running
  under its account, cannot read it. On Linux a process that is not dumpable would keep it so, but
  the process check needs nothing kept. Rejected.
- **Report a refused peer to the Server too.** The Server would show it in the fleet view. The
  Client's health already shows the second service unreachable, and the log names the impostor. A
  new health detail is a protocol matter of its own. Not taken now.
- **Keep the replacement of the second service as a residual.** The separation holds everywhere
  else. Leaving this case means a Managed Process can lie about what runs and keep what it starts
  alive past the Client's connection. Rejected; what remains is named under *Out of scope*.
- **On Windows, leave the second service's process with the default security its account gives
  it, and check the peer's process by its id alone.** Every process of the process account, each
  Managed Process among them, could open the second service with the rights that account holds. It
  could read the Supervisor Endpoint's token from its memory, run code in it, or duplicate its pipe
  handle and answer the Client in its place, which no check of the peer can tell. An id read from
  the pipe can pass to another process before the SCM answers. Rejected; clause 19 closes the
  process and its threads and holds the peer's process by a handle.
- **Change the token's default DACL instead of each thread's.** Every later thread would be covered
  from its creation. But every Managed Process would inherit the DACL and be closed to its own
  account too, which may break an agent that inspects its own processes. Starting each program
  under a token of its own instead brings back the token handling of `CreateProcessAsUserW` that
  this decision keeps out of the second service. Rejected.
- **Leave `PROCESS_TERMINATE` to the process account.** A Managed Process could end the second
  service, which the service manager restarts. That is a denial and no takeover, and the shared
  account allows other denials anyway. Not taken: the rule is the narrowest grant that still lets
  the Client check the process.
- **Set the DACL only when the second service starts to listen.** Robust in operation, but not
  fail closed, and the memory read before that holds the configuration. Rejected.
- **A protected process (PPL).** It needs a signing level the product does not have. Rejected.
- **Only a unit test of the DACL.** It would not show that a process of the same account is
  refused. Rejected in favour of the smoke under *Enforcement*.
- **A privileged step never takes the endpoint or the trust from a file in the account's
  directory.** An unanswered `service install` would not enrol, and an upgrade with a bootstrap
  identity beside the program would need `--endpoint` again. Rejected: it changes what automation
  relies on, for a gain the Context weighs as small.
- **Accept the residual of a read-back enrolment and change no code.** The file is the account's
  word, and the enrolment produces nothing the account does not control already. Rejected: where
  the administrator just answered, the run already holds the true values, and using them costs
  little.
- **Re-read the file and compare it with the answers, refusing on a difference.** It detects a
  rewrite instead of ignoring it. Rejected for a run that wrote the file: holding the answers
  needs no comparison. Chosen for `--endpoint` over a kept file, where no answer was written.
- **`--endpoint` over a kept file enrols with `--endpoint`.** The certificate would come from one
  Server while the service connects to the other. Rejected.
- **`--endpoint` over a kept file counts as no answer, without a check.** The MSI passes
  `--endpoint` on an upgrade, so an account that rewrote the file would have root present the
  bootstrap identity to its Server on a common path. Rejected.

## Sources / Prior art

- `service-manager` 0.11.0, `src/systemd.rs`: `install` writes `contents` verbatim when given, and
  otherwise `make_service`. `User=` is written only at system scope, and the unit goes under
  `/etc/systemd/system`. Arguments are joined unquoted. No `daemon-reload` is issued;
  `systemctl enable` reloads by default (recalled, not re-read). `src/launchd.rs`: `UserName`,
  under `/Library/LaunchDaemons`.
- systemd.exec(5), read in the Debian bookworm (systemd 252) and current upstream pages:
  - `ProtectKernelTunables=`, `ProtectKernelModules=`, `ProtectKernelLogs=`, `ProtectClock=`,
    `ProtectHostname=`, `PrivateDevices=` and `RestrictAddressFamilies=` each say that
    `NoNewPrivileges=yes` is implied when the unit lacks `CAP_SYS_ADMIN` (for example, with
    `User=`).
  - The `NoNewPrivileges=` entry lists `LockPersonality=`, `MemoryDenyWriteExecute=`,
    `RestrictNamespaces=`, `RestrictRealtime=`, `RestrictSUIDSGID=`, `SystemCallArchitectures=`,
    `SystemCallFilter=` and `DynamicUser=` among the settings that override it.
  - `ProtectControlGroups=` mentions neither. `ProtectClock=` implies `DeviceAllow=char-rtc r`.
  - Versions: `ProtectSystem=` 214, `RestrictRealtime=` 231, `ProtectKernelTunables=` and
    `ProtectControlGroups=` 232, `LockPersonality=` 235. An older systemd logs and ignores an
    unknown directive (recalled, not re-read).
  - The `NoNewPrivileges=` entry says that in a new mount namespace without SELinux, file systems
    are mounted `MS_NOSUID`. This is read as applying only when `no_new_privs` is in effect, which
    clause 8 keeps off for the account (recalled from systemd's `namespace.c`, not re-read). The
    service smoke under *Enforcement* checks it on a real host.
  - [systemd.exec(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html):
    `AmbientCapabilities=`, `NoNewPrivileges=`, `ProtectControlGroups=`.
- systemd.unit(5), specifiers (`%%`), and systemd.service(5), the quoting and `$$` of `ExecStart=`
  and the main process of a service: recalled, not re-read. `org.freedesktop.systemd1(5)`
  (`MainPID`); `launchctl(1)` (`print`, whose output it does not promise as an interface).
- `docs/manual/client.md` and `docs/manual/setup.md`: what operators are told.
- Agents' inputs that run through `sudo`, setuid monitoring plugins, and an inventory agent's root
  inventory and software installation: recalled from their documentation, not re-read.
- Debian Policy and `dh_installsystemd`, from which the installers knowingly depart (clause 9);
  Debian and Fedora packaging practice on system accounts and `sysusers.d` (recalled, not
  re-read).
- WiX's customization guide for the stock dialog sets; Windows Installer's `REMOVE`,
  `UPGRADINGPRODUCTCODE`, `MajorUpgrade` and `Secure` properties.
- CWE-59, "Improper Link Resolution Before File Access", and
  [CWE-367](https://cwe.mitre.org/data/definitions/367.html), "Time-of-check Time-of-use (TOCTOU)
  Race Condition": the classes clauses 11 to 15 close, and the class clause 28 applies to content
  the account can change.
- POSIX `openat`, `mkdirat`, `renameat`, `unlinkat`, `fchmod`; `open(2)` on `O_NOFOLLOW` and
  `O_EXCL` (an exclusive create fails on an existing link), and `rename(2)` (a link at the new name
  is replaced, not followed). Recalled from the Linux and macOS manual pages, not re-read.
- Linux `fchownat(2)` with `AT_EMPTY_PATH` on an `O_PATH` handle; `fs.protected_hardlinks`, which
  Linux sets and macOS lacks. Recalled.
- Microsoft, `CreateFileW`: `FILE_FLAG_OPEN_REPARSE_POINT`, `FILE_FLAG_BACKUP_SEMANTICS`,
  `CREATE_NEW`, and the share modes. NTFS refuses to rename a directory with a handle open below
  it. Recalled, not re-read; the Windows tests are the check.
- [capabilities(7)](https://man7.org/linux/man-pages/man7/capabilities.7.html): `CAP_SETUID`,
  `CAP_SETGID`, `CAP_KILL`, and the ambient set across `execve`.
- [kill(2)](https://man7.org/linux/man-pages/man2/kill.2.html): who may signal whom.
- [SafeSetID](https://docs.kernel.org/admin-guide/LSM/SafeSetID.html): `setgroups()` stays
  unrestricted.
- [sudo(8)](https://www.sudo.ws/docs/man/sudo.man/): `SIGSTOP` and `SIGKILL` are not relayed.
- [nginx `user`](https://nginx.org/en/docs/ngx_core_module.html#user): worker credentials.
- [Elastic Agent, unprivileged
  mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged)
  and [Datadog's `ddagentuser`](https://docs.datadoghq.com/agent/guide/windows-agent-ddagent-user/):
  how two fleet agents run without root, the latter with a stored password.
- [launchd.plist(5)](https://keith.github.io/xcode-man-pages/launchd.plist.5.html) and
  [setuid(2)](https://keith.github.io/xcode-man-pages/setuid.2.html) on macOS; Apple DTS on
  [role accounts](https://developer.apple.com/forums/thread/692593).
- Microsoft on accounts:
  [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw),
  [Restricted tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens),
  [LocalService](https://learn.microsoft.com/en-us/windows/win32/services/localservice-account),
  [service accounts](https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/understand-service-accounts).
  *LocalSystem Account*: the account holds extensive privileges on the local computer and acts as
  the computer on the network. *Service Accounts*, the section on virtual accounts: a managed
  local account per service, named `NT SERVICE\<service name>`, with no password to manage, that
  acts as the computer account on the network. The built-in local groups *Event Log Readers* and
  *Performance Monitor Users*. Recalled for their shape, not re-read for detail; and that
  `SeImpersonatePrivilege` opens paths to `SYSTEM`.
- The OpenTelemetry Collector's Windows service, which registers as `LocalSystem` unless its
  installer is told otherwise: the common default this decision departs from.
- Microsoft on services:
  [SERVICE_REQUIRED_PRIVILEGES_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_required_privileges_infow)
  (the SCM removes every privilege not listed from the process token, and never removes
  `SeChangeNotifyPrivilege`);
  [StartService](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-startservicew)
  (`ERROR_SERVICE_DEPENDENCY_FAIL` for a service whose dependency failed to start);
  [SetServiceStatus](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-setservicestatus)
  (after `SERVICE_STOPPED` the service process can be terminated at any time, so a stop waits for
  the process, not for the state).
- Microsoft on pipes and jobs:
  [Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
  (`FILE_APPEND_DATA` and `FILE_CREATE_PIPE_INSTANCE` are one right, so `FILE_GENERIC_WRITE` lets
  a client create an instance; the default descriptor grants `Everyone` read);
  [CreateNamedPipeW](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew)
  (`FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS`);
  [ImpersonateNamedPipeClient](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient)
  (an impersonation below `SecurityImpersonation`, such as `SecurityIdentification`, needs no
  `SeImpersonatePrivilege`);
  [JOBOBJECT_BASIC_LIMIT_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)
  (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ends every process of a job when its last handle closes).
- [Object Namespaces](https://learn.microsoft.com/en-us/windows/win32/sync/object-namespaces) and
  [AddIntegrityLabelToBoundaryDescriptor](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-addintegritylabeltoboundarydescriptor):
  only a caller within its boundary and at or above its integrity level creates a private
  namespace, and nobody can open it once its creator has closed it.
- Microsoft on object security:
  [SetKernelObjectSecurity](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-setkernelobjectsecurity),
  [SetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-setsecurityinfo)
  and
  [SetPrivateObjectSecurityEx](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-setprivateobjectsecurityex):
  Learn points file systems to `SetSecurityInfo`, which propagates what a directory passes on to
  what it holds. Clause 21 does not want that propagation, since nothing documents that it stays
  out of a junction or out of what the process account owns. The rules of automatic inheritance
  are documented for `SetPrivateObjectSecurityEx` alone. That a DACL set on a file's handle
  follows them, and keeps its protection, rests on the Windows CI runs of the tests under
  *Enforcement*, and every DACL set is read back (clause 21).
- Microsoft Learn, *Process Security and Access Rights*: the default security descriptor comes
  from the creator's token, `OpenProcess` is checked against the process's DACL,
  `SeDebugPrivilege` overrides it, and `PROCESS_DUP_HANDLE` alone yields a handle with full access.
  *Thread Security and Access Rights*; *SID Strings*: `OW` (`OWNER RIGHTS`), `SY`, `BA`;
  `SetSecurityInfo`, `OpenProcess`, `CreateToolhelp32Snapshot`, `GetNamedPipeServerProcessId`,
  `QueryServiceStatusEx` (`SC_STATUS_PROCESS_INFO`, `SERVICE_STATUS_PROCESS.dwProcessId`).
- Recalled, not re-read: that the account that owns an object holds `READ_CONTROL` and
  `WRITE_DAC` implicitly unless an `OWNER RIGHTS` entry names what it holds, and that
  `THREAD_SET_CONTEXT` is enough to run code in a thread's process. The Windows service smoke
  under *Enforcement* decides both on a real host.
- `unix(7)`, `SO_PEERCRED`: the peer's credentials as they were when it called `connect(2)` or
  `listen(2)`; `SO_PEERPIDFD` (Linux 6.5): a pidfd for that process. `pidfd_open(2)`,
  `pidfd_send_signal(2)`: a pidfd refers to one process and never to a later holder of its id.
- `prctl(2)`, `PR_SET_DUMPABLE`, and `ptrace(2)`, *Ptrace access mode checking*: a process that is
  not dumpable is denied to a caller without `CAP_SYS_PTRACE` in the user namespace, which governs
  `PTRACE_ATTACH`, `/proc/<pid>/mem` and `pidfd_getfd(2)`; `execve(2)` resets the mark. The Yama
  documentation (`Documentation/admin-guide/LSM/Yama.rst`) for `ptrace_scope`.
- Apple, `<sys/un.h>`: `LOCAL_PEERPID` at `SOL_LOCAL`; `task_for_pid` and the
  `com.apple.security.cs.debugger` entitlement.
- polkit's `unix-process` subject and CVE-2013-4288: a check of a caller by process id alone is
  open to the id's reuse, which polkit answered with the process's start time and later pidfds.
- The code: `crates/fleet-agent/src/supervisor/process.rs`, the three places a program is started
  (the Managed Process, the version probe, the preflight) and the token set last;
  `crates/fleet-agent/src/storage.rs` and `crates/fleet-util/src/nofollow.rs`, the no-follow
  adapter; `crates/fleet-agent/src/service/run_as.rs`, the account resolution and the
  hand-over's descriptor-based walk; `crates/fleet-agent/src/service/windows_config.rs`;
  `crates/fleet-agent/src/supervisor/channel/windows.rs` and `crates/fleet-agent/src/win32.rs`,
  the DACLs of the second service's process and threads and the Client's hold on the peer's
  process; `packaging/linux/postinst`; `packaging/windows/supervisor.wxs`; `docs/HARDENING.md`.

## Consequences

- Positive: a packaged Linux host runs the Client and every Managed Process as an unprivileged
  account from its first install, and a fresh Windows host runs them as the virtual account. A
  Server-delivered command so runs as neither root nor `LocalSystem`. Without a process account
  the virtual account bounds what a Managed Process can do on Windows. It cannot read another
  service's files, write outside what the hand-over and the default ACLs give it, or hold
  `SeDebugPrivilege` or `SeTcbPrivilege`. A host installed with `--run-as` keeps its account
  through every upgrade, and a host whose unit lost its account gets it back at its next upgrade.
- Positive: root and `LocalSystem` are recorded choices, and every install warns about
  `LocalSystem`. A fresh Windows host can register a process account without naming the Client's
  account first.
- Positive: the unit is one file the Client renders and tests. A root path with a space or a `%`
  works. Operators loosen or tighten the unit with ordinary drop-ins that survive upgrades.
- Positive (clauses 11 to 15): the service's account cannot make an install or a package upgrade
  overwrite, truncate, narrow, widen or re-own a file it cannot write itself. The install and the
  runtime read the state directory by one rule. The Client replaces every file it keeps whole, so
  a crash leaves the old file or the new one.
- Positive (clauses 16 to 27): where a process account is registered, a Managed Process, and
  anything it starts, can read neither the Client's key, certificates and package state, nor the
  Client's environment. It cannot rewrite the program it runs. A staged package's preflight runs
  under the process account too. No process on the host holds a privilege it did not hold without
  the separation, and on Windows no Managed Process holds `SeImpersonatePrivilege`. A Managed
  Process cannot take the second service's place by replacing its socket or making an instance of
  its pipe. So the Client tells it no program, environment or Supervisor Endpoint token, and the
  programs the Client knows of end with its connection. On Linux and Windows the Managed
  Processes cannot read or write the second service's memory either, run code in it or take its
  handles.
- Positive (clause 28): an administrator who answered the questionnaire or gave `--endpoint`
  enrols with what they answered, whatever the account does to the file meanwhile. In the common
  runs the enrolment reads no file back for its endpoint or trust.
- Negative / trade-offs: on a fresh host, agents that need root or `LocalSystem` fail under the
  account. These include an inventory agent's full inventory and software installation, inputs
  without a `sudo` rule, any listener on a port below 1024, a file log receiver reading `root:adm`
  files, and on Windows a full inventory, an agent that installs a service, or a reader of the
  Security log. The operator either grants what is missing (a `sudoers` entry, a drop-in with
  `AmbientCapabilities=` or `SupplementaryGroups=`, `usermod -aG adm`, `net localgroup`) or
  chooses root or `LocalSystem` with `--run-as`. The manual and the post-install text say so.
- Negative / trade-offs: the sandbox is thin under the account. The account is the confinement,
  and `ProtectControlGroups=` adds little. The root tier adds three directives. Strong
  confinement stays a per-host drop-in, because systemd ties it to `no_new_privs`.
- Negative / trade-offs: hosts registered as root or `LocalSystem` stay so. On Linux the account
  exists on them after an upgrade but stays unused until the operator runs
  `service install --run-as <PRODUCT_NAME>` and restarts the service. On Windows the warning is
  what moves them. An MSI uninstall followed by a fresh install is a first registration, so a host
  that ran as `LocalSystem` comes back under the virtual account. An operator who returns a Linux
  host to root with a plain re-install, while the account still owns its data root, finds it back
  on the account; root is chosen with `--run-as root`. An account set only in a drop-in's `User=`
  governs the process, but the hand-over does not give the roots to it.
- Negative / trade-offs: a kept account that has been deleted stops the post-install, and the
  package stays unconfigured until the operator creates the account or names another. Falling
  back to root without a word is what this decision rules out.
- Negative / trade-offs: an archive host that only self-updates keeps the unit its last
  `service install` wrote. New directives reach it at its next re-registration.
- Negative / trade-offs: the account owns the layout, so `sudo opamp-fleet …` through the `PATH`
  symlink runs code the account can replace. Under root every Managed Process would hold root
  already; the account narrows that to an administrator's own command.
- Negative / trade-offs: a state directory, `versions/` or a version directory that is itself a
  link is refused, at install and at service start. The operator names the real path with
  `--state-dir` and removes the link. On Windows a data root or layout root that is a junction, a
  symbolic link or a mount point is refused; the operator names the real directory with `--root`
  or `--data-root`. A link at a file the Client keeps in the state directory is replaced by a plain
  file at the next write, and refused when read. A file with a second hard link keeps its owning
  account and is named; the operator removes the second link and runs the command again. A file
  the runtime rewrites is a new file, owned by whoever writes it. A Client run as root by hand
  against an installed state directory, outside `service install` and `service config`, so leaves
  root-owned files the service's account cannot read. The operator runs such a command as the
  service's account, or runs `service install` again.
- Negative / trade-offs: a second service, and a private protocol between two processes of one
  build. A restart of the second service stops every Managed Process. The Managed Processes still
  share one account, so they read one another's `config/` and environment. Grants that name the
  Client's account for an agent, such as a sudoers rule for an input that runs through `sudo` or
  the rights an agent needs on its directories, must name the process account instead; the manual
  says so. The separation is opt-in, so a host gains it only once its operator creates the account
  and names it.
- Negative, on Windows: a job holds what a program starts itself, not what an out-of-process
  broker starts for it, such as a WMI process creation or a scheduled task; on Unix a process that
  leaves its process group outlives the stop in the same way. `LocalService` and `NetworkService`
  are shared with other services of the host, so as a process account they separate less than the
  second service's own virtual account. Every `service install` on Windows, an MSI upgrade among
  them, stops the Managed Processes for as long as it runs. One that fails after its stop leaves
  both services stopped until the next one succeeds, or until `service start`. On Linux and macOS
  an install that changes or drops an account stops them too. One that fails after that stop
  leaves the services stopped until the operator's `service start`, since no record names what
  ran.
- Negative, of the check of the process: on Linux before 6.5 and on macOS a Managed Process can
  still take the second service's place by waiting for a process id to come round again; on
  Windows only before the Client opens the process the pipe names. That is named, not closed. On
  Windows the second service's process is open from its creation until its program's first act,
  and a thread the runtime starts for the instant before it sets its own DACL. An administrator's
  tool that opens the second service on Windows needs to run elevated. Every new connection asks
  the service manager once, by a `systemctl` or `launchctl` run or an SCM query, which adds a few
  milliseconds at the Client's start and after the second service restarts. On Linux that needs
  systemd's system bus, and on macOS it reads `launchctl print`, whose output is not a promised
  interface. Where either fails, every connection is refused and no Managed Process starts until
  it answers again, which is logged. The second service writes no core dump on Linux. A Managed
  Process can still remove or replace the socket, and while it does the Client finds the second
  service away. That is a denial of what the process account's own programs run, which the shared
  account already allows.
- Negative / trade-offs (clause 28): the questionnaire and `--endpoint` hand the enrolment the
  answered endpoint and certificates, which `config_init` returns. An unanswered run keeps the
  residual, and so does the CA of a kept file under a matching `--endpoint`; `HARDENING.md` names
  both. An upgrade with `--endpoint` over a file that names another endpoint does not enrol, and
  the administrator corrects the file or the flag.
- Follow-ups:
  - a way for an administrator to run the installed CLI without executing account-owned code;
  - an account of its own for the macOS Client;
  - a `sysusers.d` entry;
  - whether the unit orders itself after `network-online.target`;
  - the groups each Supervisor kind needs on Windows, stated in its documentation;
  - whether `icacls /T` crosses a junction inside a tree, for the hand-over and the cut-off on
    Windows;
  - holding the layout root on Windows from the first write to the last, as the data root is held;
  - reaching a state directory deeper beneath a root one held name at a time from the root, which
    closes the window between the check of what lies beneath a root and the open;
  - an account per Supervisor, and making the separation the default;
  - job objects for the process tree on a host without a process account;
  - pinning the peer's process on macOS;
  - a socket the service manager owns on Linux and macOS;
  - a health detail that names a refused peer;
  - whether an unanswered privileged run enrols at all, if the residual proves to matter.

## Enforcement

Tests that carry `Verifies: CLT-accounts-service-runs-under`:

- `crates/fleet-agent/src/service/manager.rs` (clauses 7, 8, 17):
  - `the_system_unit_is_the_clients_own_text`: the rendered unit holds `Description=<PRODUCT_NAME>`,
    a quoted `ExecStart` with the marker and absolute paths, `Restart=on-failure`,
    `RestartSec=<RESTART_DELAY_SECS>` and `WantedBy=multi-user.target`.
  - `exec_start_survives_spaces_percent_and_dollar`: a root with a space, a `%` and a `$` renders
    `"…"`, `%%` and `$$`.
  - `an_account_unit_sets_nothing_that_implies_no_new_privs`: under `User=opamp-fleet` the unit
    holds `ProtectControlGroups=yes`. It holds none of `NoNewPrivileges`, `ProtectKernelTunables`,
    `ProtectKernelModules`, `ProtectKernelLogs`, `ProtectClock`, `ProtectHostname`,
    `PrivateDevices`, `RestrictAddressFamilies`, `LockPersonality`, `RestrictRealtime`,
    `RestrictSUIDSGID`, `RestrictNamespaces`, `MemoryDenyWriteExecute`, `SystemCallFilter`,
    `SystemCallArchitectures` or `DynamicUser`.
  - `a_root_unit_carries_the_root_tier`: under `User=root` the unit adds
    `ProtectKernelTunables=yes`, `LockPersonality=yes` and `RestrictRealtime=yes`.
  - `no_unit_carries_what_breaks_a_supported_agent`: no unit holds `ProtectSystem`, `ProtectHome`,
    `PrivateTmp` or `CapabilityBoundingSet`.
  - `a_user_unit_is_left_to_service_manager`: the user scope passes `contents: None`.
  - `every_backend_renders_the_same_name`, `the_service_is_named_after_the_product_not_the_program`
    and `the_service_name_carries_no_suffix` (clause 17).
- `crates/fleet-agent/src/service/run_as.rs`, and `manager.rs` for the plist (clause 1):
  - `a_reregistration_without_run_as_keeps_the_units_account`: a unit fixture with
    `User=otelcol-contrib` yields `otelcol-contrib`, from the unit.
  - `a_plists_user_name_is_kept`.
  - `run_as_root_is_written_as_user_root`.
  - `a_unit_without_an_account_on_an_account_owned_data_root_takes_the_owner`.
  - `a_unit_without_an_account_on_a_root_owned_data_root_stays_root`.
  - `no_registration_means_root`.
  - `a_kept_account_that_no_longer_exists_is_refused_naming_its_source`: the message names the
    account, the unit path, `useradd` and `--run-as`, and says that nothing was written.
  - `the_handover_never_follows_a_directory_swapped_for_a_link`: in a tree, a directory is
    replaced by a link to a directory outside it. The outside directory's entries keep their
    owning account.
  - `the_handover_skips_a_file_with_a_second_hard_link`.
  - On every platform: with no `--run-as` and no SCM entry, the Windows choice is
    `NT SERVICE\<PRODUCT_NAME>` from the default. The install keeps an SCM entry naming
    `LocalSystem` as the registration's. `LocalSystem` and `NT AUTHORITY\SYSTEM` pass the
    passwordless-form check. The message for a `LocalSystem` account, named or kept, carries the
    warning and names the virtual account
    (`on_windows_a_first_registration_runs_as_the_services_virtual_account`,
    `windows_forms_are_the_passwordless_ones`, `local_system_is_named_and_warned_about`).
- `crates/fleet-agent/tests/linux_post_install.rs` (clauses 3, 5, 9):
  - `the_post_install_creates_the_account_before_it_registers`: `getent passwd` and the `useradd`
    line appear before every `service install`. The line is `--system`, without a password and
    with a `nologin` shell.
  - `the_post_install_names_the_account_only_where_no_unit_is_registered`:
    `--run-as opamp-fleet` appears only in the branch where
    `/etc/systemd/system/opamp-fleet.service` is absent. The other branch runs `service install`
    without `--run-as`.
  - `no_maintainer_script_removes_the_account`: no `userdel` or `groupdel` in `postrm`, `prerm` or
    `posttrans`.
  - `the_post_install_prints_no_endpoint_the_client_refuses_at_startup` and
    `the_post_install_steps_ask_for_the_client_identity`: the first printed step is
    `service install --interactive`, and no endpoint-only step is offered. The manual alternative
    names `cert_file` and `key_file`. The scripted form names `--fleet-files` and
    `--bootstrap-password-file`. The printed text names neither `[auth]` nor a credential.
- `crates/fleet-agent/src/service/layout.rs` (clause 6):
  `the_directory_name_is_base_plus_hash_never_the_prerelease`, `set_current_points_and_repoints`,
  `stage_writes_binary_manifest_and_pointer`,
  `restaging_identical_bytes_leaves_the_staged_binary_untouched`.
- `scripts/check-shell.sh` lints `packaging/linux/*` as POSIX `sh`. In
  `.github/workflows/release.yml`, the release job has the step "The RPM carries its scriptlets,
  not their file names". It asserts with `rpm -qp --scripts` that the package holds
  `service install`, `service uninstall`, `ln -sfn`, `useradd`, `--run-as`, and the layout-root
  and data-root removals (clauses 3 to 5).
- `crates/fleet-agent/tests/service_smoke.rs`, run with `--ignored` by the `service-smoke`
  workflow:
  - `a_reregistration_keeps_the_account_and_the_service_keeps_its_privileges`: the test installs
    with `--run-as` a temporary system account, then re-installs without the flag. Then
    `systemctl show -p User` still names the account, and `/proc/<MainPID>/status` shows
    `NoNewPrivs: 0`. `/proc/<MainPID>/mountinfo` shows `/` without `nosuid`, and
    `systemd-analyze verify` accepts the unit.
  - `on_windows_a_first_install_runs_as_the_virtual_account_and_local_system_is_named`: a first
    install without `--run-as` registers the virtual account with the SCM and grants it both
    roots. A re-install without the flag keeps it. `--run-as LocalSystem` registers `LocalSystem`
    and withdraws the virtual account's grants. A re-install without the flag keeps `LocalSystem`.
- `crates/fleet-agent/tests/install_follows_no_link.rs` (clauses 11 to 15), unless named otherwise:
  - Unix:
    - A link at the pending key or the request is replaced by the request. The file behind it
      keeps its bytes and its mode.
    - A link at the staged certificate, its temporary names, the issued certificate and the
      issued key is replaced when an issued certificate is stored.
    - A link at the identity file and at the connection settings is replaced.
    - Recovery, the storage, a request and the settings store refuse a state directory that is a
      link, and write nothing behind it. These are the calls a service start makes.
    - Staging refuses a `versions/` that is a link. A link at the program or its manifest is
      replaced, and the file behind it is neither written nor made executable.
    - `service config` refuses to re-own through a linked state directory. It re-owns through a
      data root reached as clause 13 says, and names a file with a second hard link.
    - The tests are `a_certificate_request_writes_through_no_link`,
      `an_issued_certificate_is_stored_through_no_link`,
      `the_identity_and_the_connection_settings_are_written_through_no_link`,
      `a_state_directory_that_is_a_link_is_refused`, `staging_the_program_writes_through_no_link`,
      `handing_the_written_files_over_follows_no_link`,
      `the_configuration_is_written_through_no_link` and
      `the_state_directory_is_created_through_no_link`.
  - Windows:
    - A state directory that is a junction is refused.
    - A `versions/` that is a junction is refused, and so is a layout root that is one. The same
      call opens the data root, so this test decides its refusal too.
    - A file symbolic link at the connection settings is replaced. The test fails when the run
      cannot create one.
    - The tests are `a_state_directory_that_is_a_junction_is_refused`,
      `staging_refuses_a_versions_junction`, `staging_refuses_a_layout_root_that_is_a_junction`,
      `a_file_symbolic_link_in_the_state_directory_is_replaced`,
      `the_configuration_is_not_written_through_a_data_root_junction`,
      `the_state_directory_is_created_through_no_junction` and
      `the_current_pointer_is_not_switched_through_a_layout_root_junction`.
  - `crates/fleet-agent/src/storage.rs`: `no_entry_is_lost_to_another_entrys_temporary_name`,
    `a_replaced_entry_is_the_runtimes_own_and_owner_only`.
- The process account and the second service (clauses 16 to 27):
  - In `crates/fleet-agent/src/`: the install refuses `--process-account` before it writes
    anything when given with `--user`, with `root`, `LocalSystem` or the Client's account, or with
    an account that does not exist (`cli.rs` `the_process_account_conflicts_with_user`;
    `service/run_as.rs` `the_process_account_is_never_root_and_the_client_never_is`,
    `the_process_account_is_never_the_clients`,
    `a_missing_process_account_is_refused_with_the_way_out`). A re-registration without the flag
    keeps the account the second service names
    (`a_process_account_is_named_or_kept_from_the_second_services_unit`). The rendered unit and
    plist of the second service carry its account and the directives of clause 8
    (`service/manager.rs` `the_second_services_unit_runs_the_program_under_the_process_account`,
    `the_second_services_plist_is_the_one_service_manager_renders`). The Windows service smoke
    below decides that its required privileges are `SeChangeNotifyPrivilege` alone.
  - In `crates/fleet-agent/src/supervisor/` and `crates/fleet-agent/tests/supervisor.rs`: with a
    process account registered, the second service starts the Managed Process, the version probe
    and the preflight, and the Client starts none
    (`a_client_told_of_the_second_service_starts_its_program_through_it`,
    `the_version_probe_runs_through_the_second_service`,
    `the_preflight_runs_through_the_second_service`). The second service refuses a connection
    whose peer is not the Client's account (`a_peer_of_another_uid_is_refused`). Closing the
    Client's connection stops every program the second service started
    (`closing_the_connection_stops_the_whole_process_group`).
  - In `crates/fleet-agent/src/storage.rs` and `csr.rs`: the modes and owning accounts of
    clause 21, and that the Client's key stays `0600` and owned by the Client
    (`with_a_process_account_its_group_reads_only_what_it_runs_on`,
    `with_a_process_account_the_request_and_the_certificate_are_owner_only`).
  - `crates/fleet-agent/tests/service_smoke.rs`
    `a_managed_process_runs_under_the_process_account_and_reaches_only_what_it_runs`, run under
    `sudo` by `.github/workflows/service-smoke.yml` on Linux. In it a Managed Process under the
    process account fails to read `client-key.pem` and to write its `program/`.
  - The Windows service smoke in the same workflow,
    `on_windows_a_managed_process_runs_under_the_process_account_and_reaches_only_what_it_runs`,
    run elevated on `windows-latest`:
    - the second service's required privileges are `SeChangeNotifyPrivilege` alone;
    - a Managed Process runs as `NT SERVICE\<PRODUCT_NAME>-agents` and holds no
      `SeImpersonatePrivilege`;
    - it cannot read the Client's key or logs nor write its `program/`, and it reads its `config/`
      and writes its data directory;
    - it runs again after the second service's process is killed;
    - a re-registration without either flag keeps both accounts, and one while both run starts
      both again;
    - `--no-process-account` returns the program to the Client's account.
  - On Windows, in `crates/fleet-agent/src/`:
    - the pipe and the channel (clause 19): `the_pipe_admits_the_clients_account_alone`,
      `a_pipe_made_first_by_another_is_not_taken_over`,
      `the_client_talks_only_to_the_process_account`, `a_peer_of_another_account_is_refused`,
      `only_the_clients_account_is_admitted_and_never_local_system`,
      `closing_the_connection_ends_the_whole_job`, `a_job_ends_the_process_it_holds` and
      `a_signal_is_refused`;
    - the command lines (clause 6): `on_windows_both_command_lines_name_the_pipe_and_a_sid`;
    - the ACLs and the walks (clauses 1 and 21):
      `the_process_account_is_granted_what_it_reads_and_writes_and_nothing_else`,
      `on_windows_a_share_is_made_through_no_junction`,
      `on_windows_the_handover_enters_no_junction_and_leaves_the_process_accounts_own`,
      `on_windows_the_handover_lets_the_client_share_with_the_process_account`,
      `on_windows_a_restricted_root_keeps_the_keepers_alone_and_enters_no_link`,
      `a_held_directory_cannot_be_renamed` and `a_dacl_is_compared_by_what_it_says`;
    - the accounts (clauses 1 and 16):
      `on_windows_the_process_account_is_a_passwordless_account_of_its_own` and
      `on_windows_a_reregistration_keeps_the_scms_logon_account`;
    - the stop and the start (clause 9):
      `a_stop_waits_for_the_process_it_confirmed_and_fails_on_one_that_keeps_changing`,
      `what_runs_is_recorded_before_the_stop_and_amended_after_it`,
      `a_failed_stop_records_what_runs_no_more`,
      `what_a_failed_install_stopped_is_started_by_the_next`,
      `an_operators_start_or_stop_strikes_what_is_to_run_again`,
      `a_resume_starts_what_ran_in_the_order_start_starts_it` and
      `a_resume_passes_over_only_what_runs_and_waits_on_what_is_pending`;
    - the withdrawal (clause 26): `on_windows_a_former_accounts_grants_are_withdrawn`,
      `on_windows_a_former_account_that_cannot_be_found_fails_before_any_change`,
      `an_interrupted_withdrawal_is_completed_by_the_next_install` and
      `an_uninstall_always_deregisters_and_withdraws_only_once_both_are_stopped`;
    - the record and the lock (clause 27):
      `on_windows_the_record_round_trips_through_a_key_only_the_administrators_write` and
      `the_install_lock_excludes_across_logon_sessions`.
  - On Linux and macOS, in `crates/fleet-agent/src/service/manager.rs` (clause 26): an install
    that changes or drops the process account, or changes `--run-as`, stops both services. It
    waits for their processes before it re-owns anything, and starts again what ran once it has
    succeeded. A stop that fails, a wait that runs out among them, changes nothing more. An install
    that changes no account, or adds a process account where none was, stops nothing
    (`an_account_change_is_a_former_account_named_no_more`,
    `what_ran_is_what_the_manager_reports_running`, `a_halt_that_fails_changes_nothing`).
  - The check of the process (clause 19), in `crates/fleet-agent/src/supervisor/channel.rs`,
    `crates/fleet-agent/src/supervisor/channel/windows.rs` and
    `crates/fleet-agent/tests/supervisor_process.rs`:
    - With a fake service manager that names one process as the second service, the Client
      accepts a peer of that process and the process account. Before it writes anything, it
      refuses each of these peers:
      - a peer of the process account whose process differs;
      - one whose process cannot be read;
      - one the service manager names no process for.

      It treats each as a second service that is away, and retries each on a new connection.
    - On Linux CI runs with a kernel that has `SO_PEERPIDFD`. A peer whose listening process has
      ended is refused while another process holds the listening socket. The refusal holds
      although the service manager names a process of the same id. The test fails rather than
      passes without `SO_PEERPIDFD`. An error of `SO_PEERPIDFD` other than `ENOPROTOOPT` refuses
      the peer and falls back to nothing.
    - On Linux the second service, once listening, reports itself not dumpable
      (`PR_GET_DUMPABLE`). The test, as its parent and without `CAP_SYS_PTRACE`, so that Yama
      permits it, is refused `PTRACE_ATTACH`, `/proc/<pid>/mem` and `pidfd_getfd`. A program the
      second service starts is dumpable. The second service does not listen while it is traced.
    - The tests are `the_client_speaks_only_to_the_process_the_manager_names`, on Windows
      `the_client_talks_only_to_the_process_the_scm_names`,
      `a_peer_that_is_not_the_second_service_is_away_and_tried_anew`,
      `a_listener_that_ended_is_refused_though_its_id_is_named`,
      `an_error_of_so_peerpidfd_falls_back_to_nothing`,
      `a_second_service_that_is_dumpable_does_not_listen`,
      `the_second_service_closes_its_process_to_its_own_account` and
      `the_second_service_does_not_listen_while_it_is_traced`.
  - The service smoke tests `a_managed_process_in_the_second_services_place_is_refused` and
    `on_windows_a_pipe_the_scm_names_no_process_for_is_refused`:
    - On Linux and macOS a Managed Process the second service started leaves its process group
      and replaces the socket. The Client, restarted, refuses it, writes it nothing, and logs it.
      It starts no Managed Process until the second service is restarted.
    - On Windows the process account is `NT AUTHORITY\LocalService`, and the second service is
      stopped. A task scheduled under that account creates the pipe. The Client, restarted,
      refuses it as a peer the SCM names no process for, writes it nothing and logs it. Once the
      task has ended and the second service runs again, the Client reaches the second service.
    - Which instance of a pipe a client opens, while the second service holds one too, is not
      determined. The test with a fake service manager therefore decides the case of a differing
      process on Windows.
  - The DACLs of the second service on Windows (clause 19), in `crates/fleet-agent/src/`:
    - The process and thread DACLs are the ones clause 19 names, and read back as such. A second
      service whose process or a thread carries another DACL does not listen. The Client refuses a
      peer whose process it cannot open, and one whose process has ended once the SCM has answered
      (`win32.rs`
      `the_second_services_process_and_threads_leave_its_account_only_asking_and_waiting`;
      `supervisor/channel/windows.rs`
      `a_second_service_listens_only_once_closed_to_the_process_account`,
      `the_client_refuses_a_peer_whose_process_it_cannot_open`,
      `the_client_refuses_a_peer_whose_process_ended_once_the_scm_answered`).
    - The Windows service smoke
      `on_windows_the_second_services_process_and_threads_are_closed_to_the_process_account`: a
      task scheduled under `NT AUTHORITY\LocalService`, the smoke's process account, opens the
      second service's process with `PROCESS_VM_READ`, `PROCESS_DUP_HANDLE` and
      `PROCESS_CREATE_THREAD`, and one of its threads with `THREAD_SET_CONTEXT`. It is refused
      `ERROR_ACCESS_DENIED` each time, while a Managed Process still runs.
- `crates/fleet-agent/tests/msi_exe_command.rs`, which reads `packaging/windows/supervisor.wxs`
  and `packaging/windows/EndpointDlg.wxs` as text (clause 10):
  `the_msi_stops_and_unregisters_only_on_a_real_removal` checks that the stop and unregister
  actions run under the condition `REMOVE="ALL" AND NOT UPGRADINGPRODUCTCODE` and no other. Beside
  it stand `register_service_with_endpoint_survives_the_crt`, `register_service_survives_the_crt`,
  `stop_and_unregister_survive_the_crt`,
  `the_msi_names_no_root_so_no_directory_property_reaches_a_command_line`, and
  `the_endpoint_dialog_names_the_identity_and_no_credential`.
- `crates/fleet-agent/src/supervisor/mod.rs`:
  `an_orphaned_supervisor_directory_is_not_reaped_at_startup` (clause 25). The archive tests of
  [PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md) in
  `crates/fleet-util/src/archive.rs`, `a_tree_keeps_the_modes_the_archive_carried` among them,
  cover the modes of clause 23.
- `crates/fleet-agent/src/config_init.rs` and `crates/fleet-agent/src/service/enrol.rs`
  (clause 28):
  - `what_an_endpoint_install_answered_is_handed_on` and
    `answers_without_a_ca_hand_on_their_endpoint_and_no_ca`: an `--endpoint` install and the
    questionnaire hand on the endpoint and the CA they answered. They hand on no CA where none was
    answered, and nothing for a kept file.
  - `a_run_that_answered_enrols_with_its_answers_whatever_the_file_says_by_now`: a run answers an
    endpoint and a CA. The test rewrites `supervisor.toml` and the CA copy before the enrolment
    connects. The enrolment connects to the answered endpoint with the answered trust.
  - `a_run_that_answered_nothing_enrols_with_what_the_file_says`: without answers the enrolment
    takes both from the file.
  - `an_endpoint_named_over_a_kept_file_that_names_another_refuses_the_enrolment`: `--endpoint`
    over a kept file that names another endpoint is refused, naming both.

**Not mechanically decidable:** that every supported agent works under the account and under root
with these units. The kinds' end-to-end tests run without systemd. The manual checklist in
`README.md` carries a step for each wrapped kind under the packaged account. Only installing a
release exercises what `dpkg`, `rpm` and Windows Installer do on a real host with the maintainer
scripts and the MSI tables. That covers the upgrade fall-through, the rpm `%posttrans` ordering,
the dialog sequence and the permanence of the `UpgradeCode`. No CI job installs the packages.
