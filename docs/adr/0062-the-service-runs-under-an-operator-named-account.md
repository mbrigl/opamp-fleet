# ADR-0062: The system service may run under an operator-named account — and the instance's files belong to that account

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

ADR-0010 registers the Client as a system service under each platform's default identity: a
systemd system unit and a launchd `LaunchDaemon` run as **root**, the Windows service as
**`LocalSystem`**. The install hard-codes that choice — `username: None` in
[`manager.rs`](../../crates/client/src/service/manager.rs) — and offers no way to name a
different account. The `--user` flag is not that way: it targets the *invoking* user's own
service manager (systemd `--user` / a LaunchAgent), exists as a development opt-in, and is
refused on Windows because the SCM has no user-scope services.

The operator requirement this ADR answers is least privilege: run the fleet client — and with
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
  ADR-0046 already refused credentials on a command line — they stand in the process list and
  the installer log. Windows' own answer to exactly this is the **virtual account**
  (`NT SERVICE\<service name>`): per-service, provisioned and password-managed by the OS,
  nothing to create and nothing to store. gMSAs and the built-in `LocalService`/
  `NetworkService` are equally passwordless.
- **The updater is the daemon itself.** ADR-0020's self-update runs *inside the service*: it
  stages a new version directory and swings `current`. Whatever account the service runs as
  must therefore be able to write the executable layout — a root-owned layout under a non-root
  service would end self-update for exactly these installs, against Goals 10/11.
- **Ownership is two operations, not one.** On Unix "belongs to the account" is `chown`; on
  Windows the layout lives under `%ProgramData%` and the equivalent is an ACL grant on the
  instance's directories.
- ADR-0010 demands that an install which cannot succeed "fail with a clear message" before
  anything is written; an account that does not exist is such a case.

## Decision

We will add **`--run-as <account>`** to `service install`, system scope only (it conflicts with
`--user`), defaulting to today's behaviour when absent:

- **The service runs as the account.** Linux: systemd `User=<account>`; macOS: launchd
  `UserName`; both through the `username` field `service-manager` already carries. Windows: an
  `sc config obj=` step in `windows_config` — the same "finish what the crate omits" seam — sets
  the service's logon account. No *Log on as a service* grant is performed: the default security
  policy grants the right to `NT SERVICE\ALL SERVICES`, which covers the virtual account; the
  built-in accounts carry it inherently; and a gMSA receives it from its domain's group policy,
  where its rights are managed anyway. A host hardened to remove the default grant must restore
  it for this service's account — the manual says so.
- **Windows accepts only passwordless account forms**: the service's own virtual account
  (`NT SERVICE\<service name>`, the recommended form), a gMSA (`name$`), or
  `NT AUTHORITY\LocalService`/`NetworkService`. A password parameter does not exist, for
  ADR-0046's reason. An account form that needs one is refused with a message naming the
  passwordless forms.
- **The instance's files belong to the account.** After laying out and registering, the install
  hands over ownership: the configuration file, the state directory, *and* the executable
  layout (`versions/` and `current`) — `chown` on Unix, an ACL modify-grant on Windows. Config
  and state because the service must read and write them; the layout because ADR-0020's updater
  is the service itself.
- **The account must already exist** on Linux and macOS; the install refuses early — before
  anything is written, per ADR-0010 — with a message showing the one-line `useradd --system`
  that creates it. Creating accounts is packaging's business, not the Client binary's
  (follow-up below). On Windows the virtual account exists implicitly with the service.
- **Everything else is unchanged.** `uninstall` still deletes nothing; re-running `install`
  with a different `--run-as` re-owns the same directories; without the flag the service
  registers exactly as today.

## Alternatives considered

- **Status quo (root/`LocalSystem` only)** — refused; least privilege is the requirement, and
  every comparable fleet agent has grown this knob.
- **systemd `DynamicUser=`/`StateDirectory=`** — Linux-only with no launchd/SCM analogue, and
  its ephemeral UIDs fight an instance whose identity, credential, and state must persist
  across restarts (ADR-0010 bakes absolute paths for exactly that).
- **Arbitrary Windows accounts with a password** — the password stands in the process list and
  installer log; ADR-0046 refused precisely this, and the passwordless forms cover the fleet
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

- Positive: the Client and everything its Supervisors spawn drop root/`LocalSystem` on operator
  demand; the configuration and state directories belong to the account that uses them (the
  requirement); self-update keeps working because the layout moved with them; the Windows story
  needs no password anywhere.
- Negative / trade-offs: the account is a trust boundary — whoever holds it can replace the
  binary in the layout, and the packaging symlink (`/usr/bin/opamp-fleet-client` →
  `current`, ADR-0048/0053) means an administrator invoking the CLI executes account-owned
  code; the manual must say so plainly. Managed Processes inherit the account: anything that
  needs ports below 1024 or root-only telemetry sources will fail under it — the operator's
  informed choice, not ours. The Supervisor's spawned processes were never re-privileged, so
  nothing new is needed there.
- Follow-ups: the `.deb`/`.rpm` packaging grows a `postinst` account creation and a `--run-as`
  wiring (amends the ADR-0046/0048 flow); the MSI can offer the virtual account as a checkbox;
  the manual's `service install` section documents the flag, the ownership handover, and the
  trust boundary; a future hardening ADR may revisit the privileged-updater split if the
  binary-replacement trade-off proves unacceptable in the field.
