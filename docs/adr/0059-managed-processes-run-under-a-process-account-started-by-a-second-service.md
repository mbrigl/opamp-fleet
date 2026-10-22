# ADR-0059: Managed Processes run under a process account of their own, started by a second service that runs under it

- **Status:** 🟡 proposed
- **Date:** 2026-10-20
- **Deciders:** Markus Brigl
- **Applies to:** the `--process-account` flag of `service install` and the second service it registers in `crates/fleet-agent/src/main.rs`, `crates/fleet-agent/src/service/manager.rs`, `crates/fleet-agent/src/service/run_as.rs`, `crates/fleet-agent/src/service/windows_config.rs` and `crates/fleet-agent/src/service/windows_rights.rs`, every place a Supervisor starts a program in `crates/fleet-agent/src/supervisor/process.rs`, the channel between the Client and the second service, the modes and ownership of the Supervisors' directories in `crates/fleet-agent/src/storage.rs`, and every document that tells an operator which account a Managed Process runs as
- **Supersedes:** [ADR-0019](0019-the-client-supervisor-installed-service-releases-and-installers.md) clauses 3 and 9, [ADR-0042](0042-the-linux-packages-run-the-client-under-its-own-account.md) clauses 2 and 6, [ADR-0026](0026-packages-signed-deployments-offered-downloads-and-verified-delivery.md) clause 60

## Context

[ADR-0042](0042-the-linux-packages-run-the-client-under-its-own-account.md) runs the Client as a
system service under an account of its own. "The same account runs every Managed Process its
Supervisors start" (ADR-0042, Context). Every Managed Process can therefore read everything the
Client can: the whole `state_dir` with the Client's private key `client-key.pem`
(`crates/fleet-agent/src/tls.rs:20`), the `config/` of every other Supervisor, and the Client's
own environment through `/proc`. It can also rewrite its own `program/` and so outlast the next
package. With the Client's key a process can present itself to the Server for any `instance_uid`
of its host ([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)).
A Managed Process is code the Supervisor does not vouch for
([ADR-0041](0041-a-managed-process-states-only-its-own-service-attributes.md), Context), and a
Configuration's language can run commands as it: a Telegraf `inputs.exec`, an Icinga
`CheckCommand`, a GLPI deploy task.

A Supervisor starts programs in three places
(`crates/fleet-agent/src/supervisor/process.rs`): the Managed Process (`:868`), the version probe
(`:944`) and the preflight of a staged package (`:1043`), which runs a package before it is
trusted. All three run as the Client today.

Separating the accounts needs something that may start a process under another account. The
sources below show:

- On Linux, `CAP_SETUID` and `CAP_SETGID` permit "arbitrary manipulations" of a process's user and
  group IDs, uid and gid 0 included (capabilities(7)). A Client that holds them is root in all but
  name, and SafeSetID still allows arbitrary `setgroups()`. A process can signal another account's
  process only with `CAP_KILL` (kill(2)), so the process-group stop of
  [ADR-0017](0017-supervisor-mode-kinds-directories-and-what-the-server-may-change.md) breaks
  without it.
- On macOS a non-root process cannot change its user ID (setuid(2)), and launchd's `UserName`
  applies only in the system domain (launchd.plist(5)).
- On Windows a service can start a child under a restricted version of its own token without
  `SeAssignPrimaryTokenPrivilege` (CreateProcessAsUserW), but whether the supported agents still
  reach the network and RPC under such a token is unverified. A second service under its own
  virtual account needs no password. Every Managed Process today also inherits
  `SeImpersonatePrivilege` from a `LocalService`-like account.

## Decision

We will run every Managed Process, and every program a Supervisor starts, under one process
account that the operator names, through a second service that runs under that account and starts,
signals and stops those programs on the Client's request.

1. **The process account is named at registration, and kept.** `service install --process-account
   <account>` names it, at system scope only; it conflicts with `--user`. A re-registration without
   the flag keeps the account the second service's registration names (clause 2), read the way
   ADR-0042 clause 2 reads the Client's. Without the flag and without such a registration, there is
   no process account and no second service, and the Client starts its programs itself, as before.
   - **The operator creates the account.** On Linux and macOS it must exist, and an install naming
     one that does not is refused before anything is written, with a message that shows the
     `useradd --system` line, and on macOS the `sysadminctl -roleAccount` line, that creates it. The
     binary never creates an account (ADR-0042 clause 2).
   - **Windows accepts only passwordless forms**, as ADR-0042 clause 2 does: the second service's
     own virtual account `NT SERVICE\<PRODUCT_NAME>-agents` (recommended), a gMSA, or
     `NT AUTHORITY\LocalService` / `NetworkService`.
   - **It is never root and never the Client's account.** `--process-account root`,
     `LocalSystem` or the account the Client runs as is refused, naming `--run-as root` as the way
     to run everything as root. A host whose agents need root runs as root as a whole (ADR-0042
     clause 2), with no second service.
   - **It is dropped on request.** `service install --no-process-account` stops and deregisters the
     second service, and the Client starts its programs itself again. It conflicts with
     `--process-account`, and a refusal that a kept process account causes names it.

2. **Restates ADR-0019 clause 3. The service is `PRODUCT_NAME`, with no suffix, on every platform,
   and the second service is `<PRODUCT_NAME>-agents`.** The label is a single token — no
   qualifier, no organization — so `service-manager` renders the same string for systemd, launchd
   and the SCM, and the name never contains a dot. The second service exists only where a process
   account is named (clause 1); its label is the product's with the suffix `-agents`. The `PATH`
   symlink is `/usr/bin/<PRODUCT_NAME>`, so two variants never collide on one `PATH` entry. On
   Windows the SCM additionally carries each service's display name and a Description that says
   what the program does.
   - **It is the same program.** The second service runs `supervisor run --agents` from the same
     layout and `current` (ADR-0019 clause 9), under the process account. The package still
     delivers one file (ADR-0042 clause 3).
   - **The two are registered, started, stopped and deregistered together.** `service install`,
     `start`, `stop` and `uninstall` act on both, the second service first on start and last on
     stop. Its unit carries
     the directives ADR-0042 clause 7 sets for its account; its plist names the account in
     `UserName`. On Windows its required privileges are limited to `SeChangeNotifyPrivilege`, so no
     Managed Process holds `SeImpersonatePrivilege`, and the Client's service depends on it.
   - **The Client runs without it, and neither waits for the other.** Under systemd the Client's
     unit wants the second service and is ordered after it (`Wants=`, `After=`), never `Requires=`:
     while the second service is down the Client stays reported to the Server, its Supervisors say
     that their programs do not start, and they start again once it is back. A Supervisor whose
     start finds the second service unreachable is not held for failing to start; it tries again
     with backoff. The Client connects again whenever the second service has restarted. A
     program the second service stops because it is stopping itself has not exited on its own,
     and counts for no restart policy.
   - **What arrives while the second service is away waits for it.** An offered package is staged
     and kept, never swapped in: it is swapped in, and its apply grace runs, once the second service
     is reached again, so the previous program stays in place and a rollback only ever goes back to
     a program that ran. A delivered Configuration is written as today, and its acknowledgement is
     given by the first start of an apply or of the retry that reaches the second service and
     survives the apply grace; a restart command's start acknowledges nothing and swaps nothing in
     (ADR-0017 clause 8), and the retry follows it at once. A Supervisor held after a package that
     failed leaves no Configuration waiting: it is reported `FAILED`. Until
     then the package stays `Installing` and the Configuration `APPLYING`, each naming the second
     service as the reason. A newer offer or Configuration replaces the waiting one, and a Client
     that stops while one waits leaves its files as the record names them.
   - **A self-update restarts whichever end is behind.** The self-update switches `current` and
     restarts the Client as before. A second service that meets a Client of another build while
     `current` no longer names its own version exits, so that its service manager starts it from
     `current`; a Client that meets a second service of another build while `current` no longer
     names its own version exits for the same restart. A peer of another build is otherwise
     refused (clause 5), and only a peer of the admitted account can cause either exit (clause 4).

3. **Every program a Supervisor starts is started by the second service.** Where a process account
   is registered, the Managed Process, the version probe and the preflight of a staged package run
   under the process account, through the second service. The Client starts none of them itself.

4. **The channel admits the Client's account alone, and ends with it.** On Linux and macOS the
   second service listens on a Unix socket, on Windows on a named pipe whose DACL grants the
   Client's account and nobody else. Every connection is checked by the peer's credentials
   (`SO_PEERCRED`, `getpeereid`, the pipe client's token) and refused unless its user is the
   Client's account. One connection is served at a time. When it closes, the second service stops
   every program it started, by its process group on Unix and by a job object with
   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` on Windows, so a Client that restarts finds no process it
   did not start, as today.

5. **The requests are what the Supervisor does today, and both ends are one build.** The Client
   asks the second service to start a program with its arguments, environment and working
   directory, to signal or stop one, and to report its exit, and to remove what the process account
   wrote under a Supervisor's directory when that Supervisor is purged; the second service streams
   each program's output back. The programs run with the second service's own umask, never one
   widened for the Client's sake. The messages are private to the build: both ends run from `current`, and
   a peer that states another version is refused.

6. **The process account reads what it runs and writes only its own data.** The process account's
   primary group is made a supplementary group of the Client's account by the operator, and the
   install refuses to register where it is not, or where the process account's primary group is
   group 0 or the Client's own primary group, where it has a member other than the Client's account
   or is the primary group of an account other than the process account, or where the process
   account is a member of the Client's primary group: the group is the process account's own.
   Every file and directory the Client keeps under `state_dir` that the table does not name — its
   logs among them — stays the Client's alone. Then:

   | Path | Owner : group | Mode |
   |---|---|---|
   | `state_dir`, each Supervisor's directory | Client : the process account's group | `0710` |
   | the Client's key, certificates, `remote-config.pb`, `instance-uid`, package state and staging | Client | `0600` / `0700`, as today |
   | `program/` and its files | Client : the process account's group | `0750`, files without group or other write |
   | the preflight's staging directory | Client : the process account's group | `0750` |
   | `config/` and its files | Client : the process account's group | `0750` / `0640` |
   | a kind's data directories | Client : the process account's group | `2770` |

   On Windows the same split is made by ACL grants to the process account's SID instead of the
   group.

7. **A program inherits nothing of the Client's.** Its environment is the second service's own
   with `HOME`, `USER` and `LOGNAME` set to the process account's, then the block's `env` laid over
   it, then the Supervisor Endpoint's token, last
   ([ADR-0015](0015-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)
   clause 16).

8. **Restates ADR-0019 clause 9. The executable layout is versioned side by side behind a `current`
   pointer, and the registered command line names the second service where there is one.**

   ```text
   <layout root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/supervisor[.exe]
   <layout root>/versions/supervisor-…/manifest.toml   # full version string, binary SHA-256
   <layout root>/current -> versions/supervisor-…/      # symlink on Unix, junction on Windows
   ```

   The directory name is the bare base and the commit short-hash of
   [ADR-0012](0012-versions-resolved-in-the-internal-crate.md)'s version, never the pre-release: the release `1.2.3` and a
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
   registered command line is `run --service --config <abs> --state-dir <abs>`, followed, where a
   process account is registered, by `--agents-socket <abs> --process-uid <uid>` (clause 2).

9. **Restates ADR-0042 clause 6. The Client writes its systemd system units itself, and
   `service-manager` installs them.**
   `manager::install` passes `contents: Some(<unit>)` at system scope under systemd.
   `service-manager` still writes the file (mode `0644`), enables it, which reloads the manager's
   configuration, and uninstalls it. The unit holds:
   - `[Unit]`: `Description=<PRODUCT_NAME>`, the same as before. The display name stays where
     ADR-0019 clause 2 puts prose. Where a process account is registered, also
     `Wants=<PRODUCT_NAME>-agents.service` and `After=<PRODUCT_NAME>-agents.service` (clause 2).
   - `[Service]`: `ExecStart=` with the program and every argument in double quotes. A `\` and a
     `"` are escaped, a `%` is written `%%` and a `$` is written `$$`, so any absolute path reaches
     the program as it was given. Then `Restart=on-failure`, `RestartSec=` set to the restart delay
     the SCM's recovery actions use (ADR-0019 clause 11), `User=` set to the account of ADR-0042
     clause 2, and the directives of ADR-0042 clause 7.
   - `[Install]`: `WantedBy=multi-user.target`.

   The user scope and the launchd plist stay as `service-manager` renders them. `service install`
   never writes, reads or removes anything under `/etc/systemd/system/<PRODUCT_NAME>.service.d/`.
   Drop-ins there belong to the operator, outrank the unit, and survive every re-registration.

   The second service's unit is written the same way: `User=` names the process account,
   `ExecStart=` runs `run --agents <socket> --client-uid <uid>`, `RuntimeDirectory=` holds the
   socket, `TimeoutStopSec=` exceeds the time the second service takes to stop what it started,
   and the directives are those of ADR-0042 clause 7 for its account.

10. **Restates ADR-0042 clause 2. The system service runs under the account its registration
   names, and both roots belong to it, except what the process account keeps there.** `service install --run-as <account>` names the account. It
   applies at system scope only and conflicts with `--user`. On Linux and macOS, `--run-as root` is
   how an operator chooses root. It is written as `User=root` or `UserName` `root`, so a unit
   records that root was chosen. Without the flag, the account is taken in this order:
   - **A re-registration keeps the account the registration names.** That is `User=` in the system
     unit `/etc/systemd/system/<PRODUCT_NAME>.service`, `UserName` in
     `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`. The install reads the file the Client wrote, not
     a drop-in and not the loaded state of the service manager. On Windows the install does not ask
     the SCM: a re-registration without `--run-as` sets no logon account, so the SCM keeps the one
     it holds (`windows_config`), and the install names no account.
   - **A unit or plist that names no account, on a data root that a non-root account owns,** was
     rewritten by a re-registration that dropped its account. The install takes the owner of the
     data root and prints that it did, naming the data root. Every other registration that names no
     account stays root.
   - **A first registration**, one where no unit, plist or SCM entry exists, runs as root under
     systemd and launchd and as `LocalSystem` under the SCM. The Linux packages name their account
     themselves (ADR-0042 clause 5).

   On Linux and macOS the install prints the account and where it came from: `--run-as`, the unit,
   the plist, the owner of the data root, or the default.
   - **The service runs as the account.** Under systemd this is `User=` in the unit the Client
     writes (clause 9). Under launchd it is `UserName`, through `service-manager`'s `username`. On
     Windows it is an `sc config obj=` step in `windows_config`. No *Log on as a service* grant is
     performed: the default policy grants it to `NT SERVICE\ALL SERVICES`, built-in accounts carry
     it, and a gMSA receives it from its domain. A host hardened against the default grant must
     restore it.
   - **Windows accepts only passwordless forms:** the service's own virtual account
     `NT SERVICE\<PRODUCT_NAME>` (recommended), a gMSA (`name$`), or
     `NT AUTHORITY\LocalService` / `NetworkService`. There is no password parameter, because it
     would stand in the process list and the installer log. A form that needs a password is refused
     with a message naming the passwordless forms.
   - **The account must exist** on Linux and macOS, whether it was named or kept. An account that
     does not exist is refused before anything is written, and the install fails closed. For a kept
     account, the message names the account and the file or directory it came from. It shows the
     `useradd --system` line that creates the account, and says that `--run-as <account>` or
     `--run-as root` names another. The binary never creates an account; that is the packages' job
     (ADR-0042 clause 5).
   - **The hand-over covers both roots, on every registration that runs under an account.** That
     includes a kept account and root. After laying out and registering, the install hands the
     account the data root (`supervisor.toml`, `state/`), because the service reads and rewrites
     them. It also hands over the layout root (`versions/`, `current`), because the self-update that
     stages into it is the service itself. On Unix this is an ownership change. On Windows it is an
     inheritable Modify ACL grant. Where the roots coincide this is one operation. Where they split,
     an install that hands over only one has not succeeded.
   - **Where a process account is registered, the hand-over leaves its data alone.** An entry the
     process account owns is skipped together with everything beneath it, and an entry in the
     process account's group keeps that group, a directory its mode too, so a re-registration —
     every package upgrade — takes from no Managed Process the data it wrote or reads.
   - **The hand-over never follows a link.** The account owns the trees that root walks, and
     ADR-0042 clause 5 makes that walk part of every package upgrade. So each directory is opened
     without following a link, and its owner is changed through that open handle. Its entries are
     read from the handle, and a symbolic link is re-owned as a link. A regular file with more than
     one hard link is skipped and named in a warning. Re-running `install` with another `--run-as`
     re-owns the same directories.

11. **Restates ADR-0026 clause 60. Modes come from a `.tar.gz` on Unix, never with write for group
    or others; the program is always made executable.** A tar's member modes are applied, each
    without the group's and others' write bits, on every host, whether or not a process account is
    registered: a member written `0777` or `0666` lands `0755` or `0644`, so no account but the
    Client's can rewrite what a Supervisor runs. A `.7z` or `.zip` carries Windows attributes, so no
    mode is taken from it. The program — the single file or the member at `program_path` — is set
    `0755` regardless, so whether it can run never depends on how the archive was built; an agent
    with further executables beside its program ships as `.tar.gz`.

**Out of scope:** an account per Supervisor; making a process account the default where none is
named; job objects for a host without a process account; what the Managed Processes can read of
one another, which they share by this decision; a compromised Client, which can place any program
under `program/`.

## Alternatives considered

- **Let the Client switch accounts itself.** On Linux it needs `CAP_SETUID`, `CAP_SETGID` and
  `CAP_KILL` in its unit, which permit uid and gid 0, so a compromised Client is root; every child
  must also clear the ambient and inheritable sets, or it inherits them. On macOS the Client must
  be root. On Windows a restricted token is unproven for the agents' network and RPC use. Rejected
  for making the Client root-equivalent.
- **A setuid-root helper bound to the process account.** It is a second file in the package,
  against ADR-0042 clause 3, lives outside the layout the self-update rewrites, and is the classic
  target. Rejected.
- **A root parent that keeps privileges and drops them for the Client and the processes**, as nginx
  and postfix do. It brings back the long-running root process ADR-0042 removed. Rejected.
- **Transient units through systemd's D-Bus API, or `sudo -u`.** The first needs the polkit right
  to manage units, which cannot be narrowed to one account and exists on Linux only; the second
  does not relay `SIGKILL` (sudo(8)). Rejected.
- **An account per Supervisor.** It also separates the Supervisors from one another, but needs one
  service or one privilege per account. Not taken now.
- **Keep the key out of the processes' reach instead** (kernel keyring, TPM, a key helper). It
  protects the key alone, and leaves every other Supervisor's `config/` and the program directories
  writable. Rejected.
- **Create the process account in the packages and by `service install`.** The packages could
  create it as they create the Client's (ADR-0042 clause 5). Not taken: the operator names and
  creates it.

## Sources / Prior art

- [capabilities(7)](https://man7.org/linux/man-pages/man7/capabilities.7.html): `CAP_SETUID`,
  `CAP_SETGID`, `CAP_KILL`, and the ambient set across `execve`.
- [kill(2)](https://man7.org/linux/man-pages/man2/kill.2.html): who may signal whom.
- [SafeSetID](https://docs.kernel.org/admin-guide/LSM/SafeSetID.html): `setgroups()` stays
  unrestricted.
- [systemd.exec(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html):
  `AmbientCapabilities=`, `NoNewPrivileges=`, `ProtectControlGroups=`.
- [sudo(8)](https://www.sudo.ws/docs/man/sudo.man/): `SIGSTOP` and `SIGKILL` are not relayed.
- [nginx `user`](https://nginx.org/en/docs/ngx_core_module.html#user): worker credentials.
- [Elastic Agent, unprivileged mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged)
  and [Datadog's `ddagentuser`](https://docs.datadoghq.com/agent/guide/windows-agent-ddagent-user/):
  how two fleet agents run without root, the latter with a stored password.
- [launchd.plist(5)](https://keith.github.io/xcode-man-pages/launchd.plist.5.html) and
  [setuid(2)](https://keith.github.io/xcode-man-pages/setuid.2.html) on macOS; Apple DTS on
  [role accounts](https://developer.apple.com/forums/thread/692593).
- [CreateProcessAsUserW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessasuserw),
  [Restricted tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens),
  [LocalService](https://learn.microsoft.com/en-us/windows/win32/services/localservice-account),
  [service accounts](https://learn.microsoft.com/en-us/windows-server/identity/ad-ds/manage/understand-service-accounts).
- Recalled, not re-read for this record: that the SCM removes every privilege not listed in a
  service's required privileges, that `SeImpersonatePrivilege` opens paths to `SYSTEM`, and the
  behaviour of job objects.
- `crates/fleet-agent/src/supervisor/process.rs:868`, `:944`, `:1043`: the three places a program
  is started; `:869-873`: the token is set last. `crates/fleet-agent/src/storage.rs:354`, `:368`:
  directories `0700` and files `0600` for the Client. `crates/fleet-agent/src/service/run_as.rs`:
  the account resolution and the hand-over that follows no link.

## Consequences

- Positive: a Managed Process, and anything it starts, can no longer read the Client's key, its
  certificates and package state, or the Client's environment, and can no longer rewrite the
  program it runs. A staged package's preflight runs under the process account too. No process on
  the host holds a privilege it did not have before, and on Windows no Managed Process holds
  `SeImpersonatePrivilege`.
- Negative / trade-offs: a second service and a private protocol between two processes of one
  build. A restart of the second service stops every Managed Process. The Managed Processes still
  share one account, so they read one another's `config/` and environment. Grants that name the
  Client's account for an agent — a sudoers rule for Telegraf's `use_sudo`, the rights Icinga 2
  needs on its directories — must name the process account instead; the manual and the changelog
  say so. The separation is opt-in, so a host gains it only once its operator creates the account
  and names it.
- Follow-ups: an account per Supervisor; making the separation the default; job objects for the
  process tree on a host without a process account.

## Enforcement

Tests that will carry `Verifies: ADR-0059`:

- In `crates/fleet-agent/src/service/`: `--process-account` with `--user`, with `root`,
  `LocalSystem` or the Client's account, and with an account that does not exist is refused before
  anything is written; a re-registration without the flag keeps the account the second service
  names; the rendered unit and plist of the second service carry its account and the directives of
  ADR-0042 clause 7; on Windows its required privileges are `SeChangeNotifyPrivilege` alone.
- In `crates/fleet-agent/src/supervisor/`: with a process account registered, the Managed Process,
  the version probe and the preflight are started through the second service and none by the
  Client; a connection whose peer is not the Client's account is refused; closing the Client's
  connection stops every program the second service started.
- In `crates/fleet-agent/src/storage.rs`: the modes and owners of clause 6, and that the Client's
  key stays `0600` and owned by the Client.
- A Linux integration test in `.github/workflows/service-smoke.yml`, run under `sudo`, in which a
  Managed Process under the process account fails to read `client-key.pem` and to write its
  `program/`.
