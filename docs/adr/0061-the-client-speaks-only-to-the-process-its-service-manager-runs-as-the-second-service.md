# ADR-0061: The Client speaks only to the process its service manager runs as the second service

- **Status:** 🟡 proposed
- **Date:** 2026-10-24
- **Deciders:** Markus Brigl
- **Applies to:** the Client's check of the second service's end of the channel in `crates/fleet-agent/src/supervisor/channel.rs` and `crates/fleet-agent/src/supervisor/channel/windows.rs`, the query of the second service's process from systemd, launchd and the SCM in `crates/fleet-agent/src/service/manager.rs` and `crates/fleet-agent/src/service/windows_config.rs`, and every document that says what the separation by a process account holds against a Managed Process
- **Supersedes:** [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md) clause 4

## Context

Where a process account is registered, the Client starts every Managed Process through a second
service that runs under that account
([ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)).
Clause 4 of ADR-0059 has each end of the channel check the other's account: the second service
admits the Client's account alone, and the Client accepts a socket whose peer, or a pipe whose
owner, is the process account.

Every Managed Process runs under that same account. On Linux and macOS the socket lies in a
directory the process account owns (ADR-0059 clause 9), so a Managed Process can remove the
second service's socket and bind its own at the name; on Windows it can create an instance of the
pipe, which the pipe's security descriptor grants the process account. At the Client's next
connection — after a restart of the Client or of the second service — the Client finds a peer of
the right account and accepts it (`Channel::open` checks the peer's uid, or the pipe's owner).
The impostor then receives what the Client sends: each program to start, its arguments and
environment, and the Supervisor Endpoint's token. It gains no account it does not already run
under, but what it starts no longer ends with the Client's connection, which clause 4 promises,
and what it reports of a program's start and exit to the Client — and through the Client's health
to the Server — is its own word. ADR-0059 names this for Windows as a case of the Managed
Processes sharing one account; `docs/HARDENING.md` names it for every platform.

The account cannot tell the two apart; the process can. Each service manager knows the process
it runs as the second service — systemd its `MainPID`, launchd the job's `pid`, the SCM the
service's process id — and each platform reports the process at the other end of a local
connection: `SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS, `GetNamedPipeServerProcessId` on
Windows. A peer whose process is the one the service manager runs as the second service is the
second service; any other is not, whatever its account.

The specification's quality goal Q-1 asks that the Fleet be secure by default.

## Decision

We will have the Client accept the second service's end of the channel only where its process is
the one the service manager runs as the second service, besides being the process account's.

1. **Restates ADR-0059 clause 4. The channel admits the Client's account alone, the Client speaks
   to the second service alone, and the channel ends with the Client.** On Linux and macOS the
   second service listens on a Unix socket, on Windows on a named pipe. Every connection is
   checked by the peer's credentials (`SO_PEERCRED`, `getpeereid`, the pipe client's token) and
   refused unless its user is the Client's account. One connection is served at a time. When it
   closes, the second service stops every program it started, by its process group on Unix and by
   its job on Windows, so a Client that restarts finds no process it did not start.
   - **The pipe is the process account's, and the Client reaches it for reading and writing data
     alone.** It is `\\.\pipe\<PRODUCT_NAME>-agents`, created with a security descriptor that
     names the process account as its owner and carries a protected DACL of two entries: full
     access for the process account, which creates the pipe's instances, and for the Client's
     account read and `FILE_WRITE_DATA` alone (`0x12008b`). The Client is granted neither
     `FILE_GENERIC_WRITE` nor `FILE_APPEND_DATA`, which is `FILE_CREATE_PIPE_INSTANCE` on a pipe,
     so it cannot serve the pipe itself. Nobody else is granted anything.
   - **The pipe cannot be taken over.** Its first instance is created as the first of its name
     (`FILE_FLAG_FIRST_PIPE_INSTANCE`), so a pipe someone else made under the name first fails
     the second service's start, and every instance rejects remote clients
     (`PIPE_REJECT_REMOTE_CLIENTS`).
   - **Each end checks the other.** The Client opens the pipe at the identification level
     (`SECURITY_IDENTIFICATION`), so the second service may identify it and act as nobody, and
     accepts the pipe only where its owner is the process account. The second service reads the
     client's token by impersonating it at that level, and refuses a client that is not the
     Client's account. It refuses to run as `LocalSystem`, to serve `LocalSystem`, or to serve its
     own account.
   - **The Client checks the process too.** Before it writes anything on a new connection, the
     Client reads the process at the other end — `SO_PEERCRED` on Linux, `LOCAL_PEERPID` on macOS,
     `GetNamedPipeServerProcessId` on Windows — and then asks the service manager for the process
     it runs as the second service: systemd the unit's `MainPID`, launchd the job's `pid`, the SCM
     the service's process id from `QueryServiceStatusEx`. It accepts the connection only where
     both name the same process and that process is the process account's. A connection whose
     process cannot be read, or that the service manager names no process for, is refused as one
     whose process differs.
   - **A refused peer learns nothing.** The Client closes a connection it refuses before it sends
     the handshake or any request. It logs an error naming the socket or pipe, the peer's process
     id and, where it can read it, its executable, and treats the second service as away: it tries
     again with the backoff it uses for a second service that is not running, and the Supervisors
     report the second service unreachable as they do then (ADR-0059 clause 3). A peer whose
     process differs is never retried on the same connection.
   - **Each program runs in a job of its own.** On Windows the second service starts every program
     suspended, assigns it to a new job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and only
     then lets it run, so everything it starts is in that job too. At the end of each connection,
     and so before it reports itself stopped, the second service ends every job it holds and
     waits, up to five seconds, until none holds a process.

**Out of scope:** the second service noticing that its socket was replaced; a socket the service
manager creates and owns (socket activation); what the Managed Processes can do to one another
under the one account beyond taking the second service's place, which ADR-0059 leaves to them; a
peer that is the second service's process but runs a build other than the Client's, which the
handshake already refuses (ADR-0059 clause 5).

## Alternatives considered

- **A socket the service manager owns.** systemd (`ListenStream=` in a socket unit) and launchd
  (`Sockets`) can create the socket as root in a directory the process account cannot write, and
  hand it to the second service, so a Managed Process cannot replace it. It closes the case on
  Linux and macOS without asking for a process, but Windows has no such mechanism for a named
  pipe, so the Client would need the process check there anyway; two mechanisms, and a change to
  the units, the plist, the socket's place and the command line. Not taken; the process check
  alone covers all three platforms.
- **Both.** The socket the service manager owns and the process check together. The process check
  already refuses every impostor, so the second mechanism would add cost and no protection the
  decision lacks. Rejected.
- **The second service checks that its socket is still its own** and binds it again where it was
  replaced. It narrows the window but does not close it — the impostor can replace the socket
  again between two checks — and does nothing on Windows. Rejected; the Client's check closes it.
- **A secret only the Client and the second service share** — a token the second service proves
  at the handshake. The second service would have to keep it where its own Managed Processes,
  running under its account, cannot read it, which the account does not allow. Rejected.
- **Report a refused peer to the Server too.** The Server would see it in the fleet view. The
  Client's health already shows the second service unreachable, and the log names the impostor;
  a new health detail is a protocol matter of its own. Not taken now.
- **Keep it as a residual.** The separation holds everywhere else; leaving this case means a
  Managed Process can lie about what runs and keep what it starts alive past the Client's
  connection. Rejected.

## Sources / Prior art

- `unix(7)`, `SO_PEERCRED`: the peer's pid, uid and gid as they were when it called `connect(2)` or
  `listen(2)`.
- Apple, `<sys/un.h>`: `LOCAL_PEERPID`, the pid of the peer of a connected local socket
  (`getsockopt(2)` at `SOL_LOCAL`).
- Microsoft Learn, `GetNamedPipeServerProcessId` and `QueryServiceStatusEx`
  (`SC_STATUS_PROCESS_INFO`, `SERVICE_STATUS_PROCESS.dwProcessId`).
- `systemd.exec(5)` and `systemctl(1)` (`show --property=MainPID`); `launchctl(1)` (`print`).
- polkit and D-Bus, which authorize a caller by its process as well as its uid (`polkit(8)`,
  *Unix process* subjects), for the same reason: a uid alone does not say which program asks.
- This repository: [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)
  clauses 3, 4, 5 and 9; `crates/fleet-agent/src/supervisor/channel.rs` (`Channel::open`);
  `docs/HARDENING.md`.

## Consequences

- Positive: a Managed Process can no longer take the second service's place: the Client never
  tells it a program, an environment or the Supervisor Endpoint's token, and every program the
  Client knows of ends with its connection, as ADR-0059 clause 4 promises.
- Negative / trade-offs: every new connection asks the service manager once — a `systemctl` or
  `launchctl` run, or an SCM query — which adds a few milliseconds to a connection that is made at
  the Client's start and after the second service restarts. A Managed Process can still remove or
  replace the socket, and while it does the Client finds the second service away and the
  Managed Processes do not start; that is a denial of what the process account's own programs
  run, which the shared account already allows by stopping the second service's processes.
- Follow-ups: a socket the service manager owns on Linux and macOS, should a Managed Process's
  denial of the second service need to be closed too; a health detail that names a refused peer.

## Enforcement

Tests that will carry `Verifies: ADR-0061`:

- With a fake service manager that names one process as the second service, the Client accepts a
  peer of that process and the process account, and refuses one of the process account whose
  process differs, one whose process cannot be read, and one the service manager names no process
  for — each before it writes anything, and each treated as a second service that is away.
- The service smoke test, on Linux, macOS and Windows: a program running under the process account
  replaces the socket, or creates an instance of the pipe, and the Client, restarted, refuses it,
  writes it nothing, logs it, and starts no Managed Process until the second service serves the
  name again.
