# ADR-0029: The Linux service executes from `/opt`, and the system service may run under an operator-named account that owns the instance's files

- **Status:** 🟢 accepted
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl

## Context

ADR-0010 roots the default system-scope install at the platform data directory —
`/var/lib/opamp-fleet/client/<instance>` on Linux — and that one root holds everything: the
`versions/` tree, the `current` pointer the service is registered against, the default `state/`
directory, and the default `client.toml`. ADR-0021 builds the `.deb`/`.rpm` flow on
top of it: `%post` runs plain `service install`, and `/usr/bin/opamp-fleet-client` is a symlink
through that root's `current` pointer.

On the distributions the `.rpm` exists for, the service this registers can never start:

- Fedora and RHEL run SELinux with the targeted policy in **enforcing** mode by default, and
  openSUSE Leap 16 / SLES 16 have switched from AppArmor to exactly that (SELinux targeted,
  enforcing on a fresh install). This is no longer one distribution family's quirk but the
  default posture of the entire rpm world.
- Files created under `/var/lib` carry the SELinux type `var_lib_t`. systemd (`init_t`) may only
  `execve` types the policy marks as service entrypoints — `bin_t`, `usr_t`, and friends, through
  which a third-party service transitions into `unconfined_service_t`. `var_lib_t` is not such a
  type, for anybody: data directories are deliberately not executable by the init domain.
- The failure is deferred and silent at install time: the package installs, `service install`
  stages the layout and registers the unit, `systemctl enable` succeeds — and the first
  `systemctl start` dies with `status=203/EXEC` (Permission denied), an AVC denial in the audit
  log the unit's own journal never explains. The same binary runs fine from an interactive shell
  (`unconfined_t` may execute nearly anything), which makes the diagnosis actively misleading.
- The blind spot is on record: the service smoke test excludes "hosts with SELinux or AppArmor in
  the way" from coverage (`crates/client/tests/service_smoke.rs`), so no automated or scripted
  check ever met an enforcing host.

Two forces constrain the fix:

- **The layout is rewritten at runtime.** The self-update (ADR-0018) stages new version
  directories while the service runs, so any fix must hold for files the *daemon* creates later,
  not only for what the package laid down once. A one-time `chcon` dies at the next staging or
  the next filesystem relabel; a persistent file-context rule needs `semanage` from
  `policycoreutils-python-utils`, which is not guaranteed to be installed and would become a
  runtime dependency of an application that otherwise needs none.
- **The unit's data paths are load-bearing on managed hosts.** The instance identity, the
  credential, and `client.toml` live under the `/var/lib` root on every host already in the
  fleet. A fix that moves *them* forces a fleet-wide state migration inside a package upgrade —
  a copy across a possible filesystem boundary, torn if the upgrade is interrupted, on hosts
  whose whole point is to be managed unattended.

ADR-0010 registers the Client as a system service under each platform's default identity: a
systemd system unit and a launchd `LaunchDaemon` run as **root**, the Windows service as
**`LocalSystem`**. The install hard-codes that choice — `username: None` in
[`manager.rs`](../../crates/client/src/service/manager.rs) — and offers no way to name a
different account. The `--user` flag is not that way: it targets the *invoking* user's own
service manager (systemd `--user` / a LaunchAgent), exists as a development opt-in, and is
refused on Windows because the SCM has no user-scope services.

The operator requirement points 5 to 9 answer is least privilege: run the fleet client — and with
it every Managed Process its Supervisors spawn — under a dedicated account instead of
root/`LocalSystem`, **with the instance's configuration and state directories owned by that
same account**, so the service can read its `client.toml`, write its state, and nothing else on
the host.

Forces:

- **The plumbing half-exists.** `service-manager`'s `ServiceInstallCtx` has a `username` field,
  honoured as systemd `User=` and launchd `UserName` — but *not* wired on Windows. The project
  already has the precedent for finishing what the crate omits there:
  [`windows_config`](../../crates/client/src/service/windows_config.rs) performs a second SCM
  step for the recovery actions the crate silently drops.
- **Windows service accounts normally need a password** (`sc config obj= password=`), and
  ADR-0021 already refused credentials on a command line — they stand in the process list and
  the installer log. Windows' own answer to exactly this is the **virtual account**
  (`NT SERVICE\<service name>`): per-service, provisioned and password-managed by the OS,
  nothing to create and nothing to store. gMSAs and the built-in `LocalService`/
  `NetworkService` are equally passwordless.
- **The updater is the daemon itself.** ADR-0018's self-update runs *inside the service*: it
  stages a new version directory and swings `current`. Whatever account the service runs as
  must therefore be able to write the executable layout — a root-owned layout under a non-root
  service would end self-update for exactly these installs, against Goals 10/11.
- **Ownership is two operations, not one.** On Unix "belongs to the account" is `chown`; on
  Windows the layout lives under `%ProgramData%` and the equivalent is an ACL grant on the
  instance's directories.
- ADR-0010 demands that an install which cannot succeed "fail with a clear message" before
  anything is written; an account that does not exist is such a case.

## Decision

We will split the **Linux system-scope defaults**: the executable layout — `versions/` and the
`current` pointer — moves to **`/opt/opamp-fleet/client/<instance>/`**, while the instance's data
— `client.toml` and `state/` — stays at **`/var/lib/opamp-fleet/client/<instance>/`**.

1. **Why `/opt` starts.** Its default file context is `usr_t`, an entrypoint type through which
   systemd transitions a third-party service into `unconfined_service_t` — the mechanism the
   targeted policy provides precisely so vendor software outside the distribution's packages can
   run enforcing. Files the self-update stages later inherit the directory's label, so staging
   keeps working with no SELinux tooling, no policy module, and no new dependency. FHS 3.0
   sanctions the shape: `/opt` is for add-on application software, and an add-on package's
   variable data belongs under `/var` — the split is the standard-conformant layout, not a
   compromise. It is also the field-proven one: Elastic Agent, whose versioned-directory scheme
   ADR-0010 adopted, installs to `/opt/Elastic/Agent` on Linux.
2. **Only the defaults split, and only here.** `--root` keeps its meaning unchanged — everything
   under the one directory the operator names, whose labeling is then the operator's business
   (documented in the manual). macOS, Windows, and the Linux user scope are untouched: no
   enforcing policy stops them, and a `systemd --user` service runs in the user's own domain.
3. **The upgrade on a managed host is a re-registration, not a migration.** The unit's `--config`
   and `--state-dir` arguments keep their `/var/lib` paths; only `ExecStart` changes. `%post`'s
   `service install` stages into `/opt`, rewrites the unit, and the restart the scriptlet already
   performs picks it up — identity, credential, and state never move.
4. **The packaging follows** (amending ADR-0021's table, whose mechanism stands unchanged): the
   `postinst`/`%posttrans` symlink targets
   `/opt/opamp-fleet/client/default/current/opamp-fleet-client`; `postinst` additionally removes
   the now-orphaned `versions/` and `current` under the old `/var/lib` default root — binaries
   only, one-time, after `service install` has succeeded and nothing references them. `postrm` on
   **remove** deletes the `/opt` instance tree (it holds nothing but staged binaries) and on
   **purge** additionally the `/var/lib` instance directory, exactly ADR-0021's remove/purge
   distinction with the paths redrawn.

We will also add **`--run-as <account>`** to `service install`, system scope only (it conflicts
with `--user`), defaulting to today's behaviour when absent:

5. **The service runs as the account.** Linux: systemd `User=<account>`; macOS: launchd
   `UserName`; both through the `username` field `service-manager` already carries. Windows: an
   `sc config obj=` step in `windows_config` — the same "finish what the crate omits" seam — sets
   the service's logon account. No *Log on as a service* grant is performed: the default security
   policy grants the right to `NT SERVICE\ALL SERVICES`, which covers the virtual account; the
   built-in accounts carry it inherently; and a gMSA receives it from its domain's group policy,
   where its rights are managed anyway. A host hardened to remove the default grant must restore
   it for this service's account — the manual says so.
6. **Windows accepts only passwordless account forms**: the service's own virtual account
   (`NT SERVICE\<service name>`, the recommended form), a gMSA (`name$`), or
   `NT AUTHORITY\LocalService`/`NetworkService`. A password parameter does not exist, for
   ADR-0021's reason. An account form that needs one is refused with a message naming the
   passwordless forms.
7. **The instance's files belong to the account.** After laying out and registering, the install
   hands over ownership: the configuration file, the state directory, *and* the executable
   layout (`versions/` and `current`) — `chown` on Unix, an ACL modify-grant on Windows. Config
   and state because the service must read and write them; the layout because ADR-0018's updater
   is the service itself.
8. **The account must already exist** on Linux and macOS; the install refuses early — before
   anything is written, per ADR-0010 — with a message showing the one-line `useradd --system`
   that creates it. Creating accounts is packaging's business, not the Client binary's
   (follow-up below). On Windows the virtual account exists implicitly with the service.
9. **Everything else is unchanged.** `uninstall` still deletes nothing; re-running `install`
   with a different `--run-as` re-owns the same directories; without the flag the service
   registers exactly as today.

## Alternatives considered

- **A persistent SELinux file context from the scriptlets** (`semanage fcontext -a -t bin_t …` +
  `restorecon -R`) — needs `policycoreutils-python-utils`, which the rpm would have to require or
  guard; a guarded fallback fails silently, which is this bug with extra steps. Label management
  in shell, re-run after every self-update and relabel, to keep executing from a directory the
  policy says should not be executed from.
- **Ship an SELinux policy module in the `.rpm`** — the most packaging-orthodox answer and the
  heaviest: a policy to author, build, and verify across Fedora, RHEL, and the newly-enforcing
  SUSE family, for a Client whose need is fully met by standing in the right directory. Remains
  the natural follow-up if a confined domain is ever wanted (CIS Server Level 2 flags
  `unconfined_service_t` daemons); nothing here forecloses it.
- **`chcon` at staging time** — not persistent across an autorelabel (`/.autorelabel`,
  `restorecon`), and it puts SELinux-specific tooling into the Client's own runtime path on every
  distribution, enforcing or not.
- **Move the whole root — state included — to `/opt`, Elastic-style** — one constant instead of
  two defaults, but it forces the fleet-wide state migration the Context rules out, and parks
  variable data in `/opt` against FHS. Rejected: the split costs one extra default path and
  costs the fleet nothing.
- **The executable layout under `/usr/lib` or `/usr/libexec`** — the labels would work, but the
  layout is application-owned and rewritten at runtime by the self-update, and ADR-0021/0048
  drew the ownership line exactly there: the package manager's hierarchy is the package
  manager's. An application mutating `/usr` at runtime blurs what those ADRs sharpened; `/opt`
  is the FHS home for software a distribution's package manager does not own.
- **Status quo (root/`LocalSystem` only)** — refused; least privilege is the requirement, and
  every comparable fleet agent has grown this knob.
- **systemd `DynamicUser=`/`StateDirectory=`** — Linux-only with no launchd/SCM analogue, and
  its ephemeral UIDs fight an instance whose identity, credential, and state must persist
  across restarts (ADR-0010 bakes absolute paths for exactly that).
- **Arbitrary Windows accounts with a password** — the password stands in the process list and
  installer log; ADR-0021 refused precisely this, and the passwordless forms cover the fleet
  cases. Elastic went the other way (a created local user with a managed password) at the cost
  of password machinery the virtual account gets from the OS for free.
- **Root-owned layout, self-update disabled under `--run-as`** — keeps "the service cannot
  replace its own binary" as a boundary, but breaks Goals 10/11 for exactly the installs this
  flag is for. The specification wins.
- **A privileged updater helper** (small root service that swings `current` on request) — a
  second service, an IPC surface, and a privilege boundary to defend, for one flag. Simplicity
  first; rejected as a present need, conceivable as a future hardening ADR.
- **Creating the account in `service install`** — platform-specific user management inside the
  Client binary (three APIs, three idempotency stories) that packaging does in one `postinst`
  line. Deferred to packaging.

## Sources / Prior art

- systemd `status=203/EXEC` under SELinux — the failure signature, and why the same binary runs
  interactively (`unconfined_t`) but not as a service:
  <https://thomaspowell.com/2026/04/03/the-selinux-203-exec-systemd/>; a real-world case of a
  service binary in a data directory failing exactly this way (GitHub Actions runner):
  <https://github.com/actions/runner/issues/1606>.
- `unconfined_service_t` — the targeted policy's mechanism for third-party services started by
  init from entrypoint-typed files (Dan Walsh, its author):
  <https://danwalsh.livejournal.com/70577.html>; Red Hat's documentation of unconfined process
  domains:
  <https://docs.redhat.com/en/documentation/red_hat_enterprise_linux/7/html/selinux_users_and_administrators_guide/sect-security-enhanced_linux-targeted_policy-unconfined_processes>;
  CIS Server Level 2 flagging unconfined daemons (why a policy module stays a possible follow-up):
  <https://access.redhat.com/solutions/6714611>.
- openSUSE Leap 16.0 / SLES 16 release notes — SELinux targeted policy, enforcing by default,
  replacing AppArmor:
  <https://doc.opensuse.org/release-notes/x86_64/openSUSE/Leap/16.0/html/release-notes-leap-160/index.html>.
- FHS 3.0 — `/opt`: add-on application software packages (§3.13), with variable data placed
  under `/var`: <https://refspecs.linuxfoundation.org/FHS_3.0/fhs/ch03s13.html>.
- Elastic Agent installs to `/opt/Elastic/Agent` on Linux (`--base-path` to override) — the
  same layout lineage ADR-0010 cites, standing where this ADR moves to:
  <https://www.elastic.co/docs/reference/fleet/installation-layout>.
- ADR-0010 (the layout and the default root this amends), ADR-0018 (the self-update that
  rewrites the layout at runtime), ADR-0021 (the native packages and the `PATH` symlink
  and remove/purge cleanup whose paths this redraws).
- [`service-manager` changelog](https://docs.rs/crate/service-manager/latest/source/CHANGELOG.md) —
  `ServiceInstallCtx.username`, honoured for systemd and launchd only; Windows explicitly left
  open.
- [Microsoft: Service User Accounts](https://learn.microsoft.com/en-us/windows/win32/services/service-user-accounts)
  and [virtual accounts](https://docs.delinea.com/online-help/privilege-manager/install/upgrades/virtual-accounts.htm) —
  `NT SERVICE\<name>` accounts are provisioned per service with OS-managed passwords.
- [Microsoft: `sc.exe config`](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/sc-config) /
  [`ChangeServiceConfig`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-changeserviceconfiga) —
  setting the logon account does not itself grant the *Log on as a service* right; the default
  security policy's grant to `NT SERVICE\ALL SERVICES` is what covers virtual accounts.
- [Elastic Agent: unprivileged mode](https://www.elastic.co/docs/reference/fleet/elastic-agent-unprivileged) —
  the versioned-layout precedent from ADR-0010, installed by root but *running* as a dedicated
  `elastic-agent-user` that owns the agent's files, upgrades included.
- [OpenTelemetry Collector Linux packages](https://opentelemetry.io/docs/collector/install/binary/linux/) —
  the `.deb`/`.rpm` create a dedicated `otelcol` system user and run the unit with `User=`.

## Consequences

- Positive: the `.rpm` produces a service that starts on Fedora, RHEL, and openSUSE Leap 16 /
  SLES 16 with SELinux enforcing — out of the box, with no new dependency, no policy module, and
  no labeling step to keep alive. Self-update staging inherits the working label by
  construction.
- Positive: hosts already in the fleet upgrade seamlessly — the unit's data paths do not change,
  so identity, credential, configuration, and state stay exactly where they are; only
  `ExecStart` and the `PATH` symlink move.
- Positive: the Client and everything its Supervisors spawn drop root/`LocalSystem` on operator
  demand; the configuration and state directories belong to the account that uses them (the
  requirement); self-update keeps working because the layout moved with them; the Windows story
  needs no password anywhere.
- Negative / trade-offs: the default install spans two directories instead of one — the manual
  and every path a support engineer greps for must name both. A `--root` install on an enforcing
  host still fails if the operator roots it somewhere unexecutable; that is now a documented
  property of choosing a root, not a default anybody gets. A host mounting `/opt` `noexec`
  breaks — rarer than enforcing SELinux by orders of magnitude, and loud rather than silent.
- Negative: upgraded hosts keep an orphaned `versions/` + `current` under `/var/lib` until the
  first packaged upgrade's `postinst` cleans them; manual (`.7z`) installs at the old default
  keep theirs until reinstalled, which is their operators' call (ADR-0010: manual layouts are
  the operator's).
- Negative / trade-offs: the account is a trust boundary — whoever holds it can replace the
  binary in the layout, and the packaging symlink (`/usr/bin/opamp-fleet-client` →
  `current`, ADR-0021 and point 4) means an administrator invoking the CLI executes account-owned
  code; the manual must say so plainly. Managed Processes inherit the account: anything that
  needs ports below 1024 or root-only telemetry sources will fail under it — the operator's
  informed choice, not ours. The Supervisor's spawned processes were never re-privileged, so
  nothing new is needed there.
- Follow-ups: the manual's path table and the README smoke checklist gain the split paths and an
  SELinux-enforcing step; a confined SELinux policy module remains an option for CIS-L2
  environments, by topic only.
- Follow-ups: the `.deb`/`.rpm` packaging grows a `postinst` account creation and a `--run-as`
  wiring (amends the ADR-0021/0048 flow); the MSI can offer the virtual account as a checkbox;
  the manual's `service install` section documents the flag, the ownership handover, and the
  trust boundary; a future hardening ADR may revisit the privileged-updater split if the
  binary-replacement trade-off proves unacceptable in the field.
