# ADR-0062: On Windows a first registration runs the Client under its virtual account, and `LocalSystem` only where an operator names it

- **Status:** 🟡 proposed
- **Date:** 2026-10-26
- **Deciders:** Markus Brigl
- **Applies to:** the account a Windows `service install` chooses and accepts in `crates/fleet-agent/src/service/run_as.rs` and sets in `crates/fleet-agent/src/service/windows_config.rs`, what the install prints about it, and every document that states which account the Windows service runs as
- **Supersedes:** [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md) clause 10

## Context

On Windows the Client's service runs as `LocalSystem` unless an operator names another account
with `--run-as` ([ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)
clause 10). The MSI names none, so every MSI install runs as `LocalSystem`.
[ADR-0042](0042-the-linux-packages-run-the-client-under-its-own-account.md) moved the Linux
packages to an account of their own and left Windows out of scope; it names "a virtual-account
default in the MSI" and "an explicit return to `LocalSystem` on Windows" as follow-ups.

Where no process account is registered (ADR-0059 clause 1), every Managed Process runs as the
Client. What a Server-delivered configuration can make a Managed Process do — an exporter that
writes a file, a `command` kind's program, a configuration language that runs commands — it then
does as `LocalSystem` on every Windows host of the fleet, and that includes rewriting
`supervisor.toml` and with it the keys that bind the Server (ADR-0017). The process account
narrows this, but it is opt-in, needs a second service, and is refused while the Client itself
runs as `LocalSystem` (ADR-0059 clause 1). A fleet that runs no second service has the Client's
account as the only bound.

The mechanism is already there: `--run-as` accepts the service's own virtual account
`NT SERVICE\<PRODUCT_NAME>`, the install hands both roots to it, and a re-registration keeps the
account the SCM holds. What is missing is the default, and a way to name `LocalSystem`, which the
passwordless-form check refuses today.

This serves Q-1 (*secure by default*) of [`docs/SPECIFICATION.md`](../SPECIFICATION.md) and keeps
G-11 (*the Client runs and updates itself as an OS service, on every platform*): the self-update
stages into the layout root, which the hand-over gives the account.

## Decision

We will register a first Windows install under the service's own virtual account
`NT SERVICE\<PRODUCT_NAME>`, keep whatever account the SCM already holds on a re-registration,
`LocalSystem` included, and accept `LocalSystem` only where `--run-as` names it.

1. **Restates ADR-0059 clause 10. The system service runs under the account its registration names,
   and both roots belong to it, except what the process account keeps there.** `service install
   --run-as <account>` names the account. It applies at system scope only and conflicts with
   `--user`. On Linux and macOS, `--run-as root` is how an operator chooses root. It is written as
   `User=root` or `UserName` `root`, so a unit records that root was chosen. On Windows,
   `--run-as LocalSystem` or `--run-as "NT AUTHORITY\SYSTEM"` is how an operator chooses
   `LocalSystem`. Without the flag, the account is taken in this order:
   - **A re-registration keeps the account the registration names.** That is `User=` in the system
     unit `/etc/systemd/system/<PRODUCT_NAME>.service`, `UserName` in
     `/Library/LaunchDaemons/<PRODUCT_NAME>.plist`. The install reads the file the Client wrote,
     not a drop-in and not the loaded state of the service manager. On Windows it reads the logon
     account the SCM holds for the service (`QueryServiceConfig`) and keeps it, `LocalSystem`
     included, so a host registered before this decision stays where it is. The second service's
     account is read and kept the same way (ADR-0059 clause 1). A service the SCM holds already is
     reconfigured in place: its program, arguments and start type are rewritten, its logon
     account is set only by the `sc config obj=` step of `windows_config`, and what ADR-0059 clause 9 and
     `windows_config` set follows as on a first registration.
   - **A unit or plist that names no account, on a data root that a non-root account owns,** was
     rewritten by a re-registration that dropped its account. The install takes the owner of the
     data root and prints that it did, naming the data root. Every other registration that names no
     account stays root.
   - **A first registration**, one where no unit, plist or SCM entry exists, runs as root under
     systemd and launchd and as the service's own virtual account `NT SERVICE\<PRODUCT_NAME>`
     under the SCM. The Linux packages name their account themselves (ADR-0042 clause 5).

   The install prints the account and where it came from: `--run-as`, the unit, the plist, the
   SCM's entry, the owner of the data root, or the default. Where the account is `LocalSystem`,
   named or kept, it also warns that every Managed Process the Client starts itself runs as
   `LocalSystem`, and names `--run-as "NT SERVICE\<PRODUCT_NAME>"` as the way to the virtual
   account.
   - **The service runs as the account.** Under systemd this is `User=` in the unit the Client
     writes (ADR-0059 clause 9). Under launchd it is `UserName`, through `service-manager`'s `username`. On
     Windows it is an `sc config obj=` step in `windows_config`, on every registration, the
     default included. No *Log on as a service* grant is performed: the default policy grants it
     to `NT SERVICE\ALL SERVICES`, built-in accounts carry it, and a gMSA receives it from its
     domain. A host hardened against the default grant must restore it.
   - **Windows accepts only passwordless forms:** the service's own virtual account `NT
     SERVICE\<PRODUCT_NAME>` (recommended, and the default), a gMSA (`name$`),
     `NT AUTHORITY\LocalService` / `NetworkService`, or `LocalSystem` / `NT AUTHORITY\SYSTEM`.
     There is no password parameter, because it would stand in the process list and the installer
     log. A form that needs a password is refused with a message naming the passwordless forms.
   - **The account must exist** on Linux and macOS, whether it was named or kept. An account that
     does not exist is refused before anything is written, and the install fails closed. For a kept
     account, the message names the account and the file or directory it came from. It shows the
     `useradd --system` line that creates the account, and says that `--run-as <account>` or
     `--run-as root` names another. The binary never creates an account; that is the packages' job
     (ADR-0042 clause 5).
   - **The hand-over covers both roots, on every registration that runs under an account.** That
     includes a kept account and root. After laying out and registering, the install hands the
     account the data root (`supervisor.toml`, `state/`), because the service reads and rewrites
     them. It also hands over the layout root (`versions/`, `current`), because the self-update
     that stages into it is the service itself. On Unix this is an ownership change. On Windows it
     is a grant of Modify to the account, inherited by what each root holds (`(OI)(CI)`) and set
     explicitly on every entry that exists; `LocalSystem` is granted nothing, since it keeps full
     control of every root (ADR-0059 clause 16). Where a process account is registered the grant
     also carries `WRITE_DAC`, so the Client can grant the process account its share of what the
     install laid out (ADR-0059 clause 6); it never carries `WRITE_OWNER`. Where the roots coincide this is
     one operation. Where they split, an install that hands over only one has not succeeded.
   - **Where a process account is registered, the hand-over leaves its data alone.** An entry the
     process account owns is skipped together with everything beneath it, and an entry in the
     process account's group keeps that group, a directory its mode too, so a re-registration —
     every package upgrade — takes from no Managed Process the data it wrote or reads.
   - **The hand-over never follows a link, and neither does any other privileged step** (ADR-0054
     clause 2). The account owns the trees that root walks, and ADR-0042 clause 5 makes that walk
     part of every package upgrade. So each directory is opened without following a link, and its
     owner is changed through that open handle. Its entries are read from the handle, and a
     symbolic link is re-owned as a link. A regular file with more than one hard link is skipped
     and named in a warning (ADR-0054 clause 5). On Windows the hand-over walks as ADR-0059 clause 6 says,
     with the backup, restore and take-ownership privileges held. Re-running `install` with
     another `--run-as` re-owns the same directories, and what the former account was granted is
     withdrawn from it (ADR-0059 clause 16).

2. **The install grants the account no group and no privilege.** A Managed Process that needs more
   than the virtual account holds — the Security event log through *Event Log Readers*,
   performance counters through *Performance Monitor Users* — gets it from the operator, who adds
   `NT SERVICE\<PRODUCT_NAME>` to that group, or runs the host as `LocalSystem` with `--run-as`.
   The manual shows the `net localgroup` line for each.

**Out of scope:** macOS, whose first registration stays root: it has no virtual account, and an
account there would have to be created by the package, which ADR-0042 leaves to packaging. The
MSI's command lines, which name no account and so take the default of clause 1. Moving a host that
runs as `LocalSystem` today; an operator does that with `--run-as`. Job Objects for the Client's
own Managed Processes.

## Alternatives considered

- **Move every `LocalSystem` host on its next upgrade.** The fleet would be hardened sooner, but a
  Managed Process that needs `LocalSystem` — a full inventory, a service it installs, the Security
  log — would stop working on an MSI upgrade without anyone having chosen that, and the
  registration would have to tell a chosen `LocalSystem` from the old default, which the SCM does
  not record. ADR-0042 decided the same for Linux root hosts. Rejected.
- **`NT AUTHORITY\LocalService` or `NetworkService` as the default.** Both are shared by many
  services, so a group or ACL granted to the Client is granted to all of them, and a file one of
  them writes is the Client's. The virtual account is a SID of this service alone. Rejected.
- **An MSI property that names the account** (`RUNAS=`). Every later MSI command line, an upgrade
  included, would have to leave it out, or it would override what an operator chose with
  `--run-as` since. The binary's default reaches the `.zip` install too. Rejected.
- **The install adds the account to *Event Log Readers* and *Performance Monitor Users*.** It
  would grant every host rights only some need, and an install that changes group membership must
  withdraw it when the account changes, as ADR-0059 clause 16 does for ACLs. Linux leaves
  `SupplementaryGroups=` to a drop-in (ADR-0042). Rejected; clause 2.
- **Keep `LocalSystem` and rely on the process account.** It needs the second service, is opt-in,
  and is refused under a `LocalSystem` Client. A host without it would keep `LocalSystem` as its
  bound. Rejected.

## Sources / Prior art

- Microsoft, *LocalSystem Account* (Win32 services documentation): the account holds extensive
  privileges on the local computer and acts as the computer on the network.
- Microsoft, *Service Accounts* (Windows security documentation), the section on virtual accounts:
  a managed local account per service, named `NT SERVICE\<service name>`, with no password to
  manage, that acts as the computer account on the network.
- Microsoft, the built-in local groups *Event Log Readers* and *Performance Monitor Users*.
- The OpenTelemetry Collector's and Telegraf's Windows services, which register as `LocalSystem`
  unless their installer is told otherwise: the common default this decision departs from.
- Recalled, not re-read for this record: the Microsoft pages and the two projects' defaults
  above, cited for their shape, not for detail.
- This repository: [ADR-0019](0019-the-client-supervisor-installed-service-releases-and-installers.md)
  clause 13, [ADR-0042](0042-the-linux-packages-run-the-client-under-its-own-account.md) (*Out of
  scope* and *Follow-ups*), [ADR-0054](0054-no-privileged-install-step-follows-a-link-the-account-placed.md),
  [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)
  clauses 1, 10 and 16, `docs/HARDENING.md`, `packaging/windows/supervisor.wxs`.

## Consequences

- Positive (Q-1): a fresh Windows host no longer hands a Server-delivered configuration
  `LocalSystem`. Without a process account, the virtual account is the bound of what a Managed
  Process can do: it cannot read another service's files, write outside what the hand-over and the
  default ACLs give it, or hold `SeDebugPrivilege` or `SeTcbPrivilege`.
- Positive: a fresh host can register a process account (ADR-0059 clause 1) without naming the
  Client's account first.
- Positive: `LocalSystem` is a recorded choice, warned about at every install, as root is on
  Linux.
- Negative / trade-offs: a Managed Process that needs `LocalSystem` or a group fails on a fresh
  host until the operator grants it or names `LocalSystem`. A kind such as a full inventory or an
  agent that installs a Windows service is one of these.
- Negative / trade-offs: an MSI uninstall followed by a fresh install is a first registration, so a
  host that ran as `LocalSystem` comes back under the virtual account.
- Negative / trade-offs: hosts registered before this decision stay `LocalSystem`; the warning is
  what moves them.
- Follow-ups: an account of its own for the macOS Client; the groups each Supervisor kind needs on
  Windows, stated in its documentation; Job Objects for the Managed Processes the Client starts
  itself.

## Enforcement

Tests that will carry `Verifies: ADR-0062`:

- In `crates/fleet-agent/src/service/run_as.rs`, on every platform: with no `--run-as` and no SCM
  entry the Windows choice is `NT SERVICE\<PRODUCT_NAME>` from the default; an SCM entry naming
  `LocalSystem` is kept as the registration's; `LocalSystem` and `NT AUTHORITY\SYSTEM` pass the
  passwordless-form check; the message for a `LocalSystem` account, named or kept, carries the
  warning and names the virtual account.
- In `crates/fleet-agent/tests/service_smoke.rs`, on Windows in CI: a first install without
  `--run-as` registers the virtual account with the SCM and grants it both roots; a re-install
  without the flag keeps it; `--run-as LocalSystem` registers `LocalSystem` and withdraws the
  virtual account's grants; a re-install without the flag keeps `LocalSystem`.
