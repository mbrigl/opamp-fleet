# ADR-0061: The Client speaks only to the process its service manager runs as the second service

- **Status:** 🟡 proposed
- **Date:** 2026-10-24
- **Deciders:** Markus Brigl
- **Applies to:** the Client's check of the second service's end of the channel in `crates/fleet-agent/src/supervisor/channel.rs` and `crates/fleet-agent/src/supervisor/channel/windows.rs`, the second service's own process attributes in `crates/fleet-agent/src/supervisor/channel.rs`, the query of the second service's process from systemd, launchd and the SCM in `crates/fleet-agent/src/service/manager.rs` and `crates/fleet-agent/src/service/windows_config.rs`, and every document that says what the separation by a process account holds against a Managed Process
- **Supersedes:** [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md) clause 4

## Context

Where a process account is registered, the Client starts every Managed Process through a second
service that runs under that account
([ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)).
Clause 4 of ADR-0059 has each end of the channel check the other's account: the second service
admits the Client's account alone, and the Client accepts a socket whose peer, or a pipe whose
owner, is the process account.

Every Managed Process runs under that same account. On Linux and macOS the socket lies in a
directory the process account owns (ADR-0059 clause 9), so a Managed Process can remove the second
service's socket and bind its own at the name; on Windows it can create an instance of the pipe,
which the pipe's security descriptor grants the process account. To be there when the Client
connects again it must outlive the Client's last connection, which ends every program the second
service started: on Unix by leaving its process group, on Windows through a broker outside its job —
both of which ADR-0059 names. At the Client's next connection — after a restart of the Client or of
the second service — the Client finds a peer of the right account and accepts it (`Channel::open`
checks the peer's uid, or the pipe's owner). The impostor then receives what the Client sends: each
program to start, its arguments and environment, and the Supervisor Endpoint's token. It gains no
account it does not already run under, but what it starts no longer ends with the Client's
connection, which clause 4 promises, and what it reports of a program's start and exit to the Client
— and through the Client's health to the Server — is its own word. ADR-0059 and `docs/HARDENING.md`
name this for Windows alone.

The account cannot tell the two apart; the process can. Each service manager knows the process
it runs as the second service — systemd its `MainPID`, launchd the job's `pid`, the SCM the
service's process id — and each platform reports the process at the other end of a local
connection: `SO_PEERCRED` and, from Linux 6.5, `SO_PEERPIDFD` on Linux, `LOCAL_PEERPID` on macOS,
`GetNamedPipeServerProcessId` on Windows.

A process id alone has two limits. It can be reused: a process that listened and then ended leaves
its id behind, and a later process — the restarted second service among them — can be given it;
polkit's check of a caller by process id failed this way (CVE-2013-4288) and moved to the process's
start time and to pidfds. And a Managed Process under the same account can reach into the real
second service itself instead of replacing its socket: on Linux by `ptrace`, `/proc/<pid>/mem` or
`pidfd_getfd`, where Yama is absent or `kernel.yama.ptrace_scope` is 0, which permit it for a
process that is not a descendant; on Windows by `OpenProcess` with the rights the process's default
security grants its own account. A process that marks itself not dumpable (`PR_SET_DUMPABLE` 0) is
closed to all three on Linux for any caller without `CAP_SYS_PTRACE`, whatever Yama says. On macOS
`task_for_pid` refuses a caller of the same user without the debugging entitlement.

The specification's quality goal Q-1 asks that the Fleet be secure by default.

## Decision

We will have the Client accept the second service's end of the channel only where its process is
the one the service manager runs as the second service, a process that, where the platform
allows, the Managed Processes cannot reach into.

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
   - **Each end checks the other's account.** The Client opens the pipe at the identification
     level (`SECURITY_IDENTIFICATION`), so the second service may identify it and act as nobody,
     and accepts the pipe only where its owner is the process account; on Unix it accepts a peer
     whose uid is the process account's. The second service reads the client's token by
     impersonating it at that level, and refuses a client that is not the Client's account. It
     refuses to run as `LocalSystem`, to serve `LocalSystem`, or to serve its own account.
   - **The Client checks the process too.** Before it writes anything on a new connection, the
     Client reads the process at the other end — on Linux `SO_PEERPIDFD`, and `SO_PEERCRED` only
     where the kernel does not know that option (`ENOPROTOOPT`), any other error being a process
     that cannot be read; on macOS `LOCAL_PEERPID`, on Windows
     `GetNamedPipeServerProcessId` — and then asks the service manager for the process it runs as
     the second service: systemd the unit's `MainPID`, launchd the job's `pid`, the SCM the
     service's process id from `QueryServiceStatusEx`. It accepts the connection only where both
     name the same process. With a pidfd it also requires that the process the pidfd refers to is
     still running once the service manager has answered, so the id cannot have passed to another
     process in between. The account is still read as the bullet above says, from the peer's uid
     or the pipe's owner. A connection whose process cannot be read, or that the service manager
     names no process for, is refused as one whose process differs.
   - **A refused peer learns nothing.** The Client closes a connection it refuses before it sends
     the handshake or any request. It logs an error naming the socket or pipe and the peer's
     process id, and treats the second service as away: it tries again with the backoff it uses
     for a second service that is not running, each time on a new connection checked anew, and the
     Supervisors report the second service unreachable as they do then (ADR-0059 clause 3).
   - **The second service closes its process to the Managed Processes where it can.** On Linux it
     marks itself not dumpable (`PR_SET_DUMPABLE` 0) as the first thing its program does, before it
     reads its configuration or sets up its log, and does not listen while `/proc/self/status`
     names a tracer (`TracerPid` other than 0), so from then on no process of its own account
     without `CAP_SYS_PTRACE` can attach to it, read or write its memory, or take its file
     descriptors, whatever `kernel.yama.ptrace_scope` says; the programs it starts are dumpable as
     usual, since `execve` resets the mark. On macOS the platform already refuses `task_for_pid`
     to a caller of the same user without the debugging entitlement. On Windows the process keeps
     the default security its account gives it.
   - **Each program runs in a job of its own.** On Windows the second service starts every program
     suspended, assigns it to a new job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and only
     then lets it run, so everything it starts is in that job too. At the end of each connection,
     and so before it reports itself stopped, the second service ends every job it holds and
     waits, up to five seconds, until none holds a process.

**Out of scope:** on Windows, a Managed Process that opens the second service's process with the
rights its own account holds on it — to duplicate its pipe handle or run code in it — which no check
of the peer can tell; on Linux where Yama is absent or `ptrace_scope` is 0, a Managed Process that
attaches to the second service between its `execve` and the mark — after killing it so that its
service manager starts it again — and that, as a tracer, can also make the second service read a
`TracerPid` of 0; a process id reused on Linux before 6.5, on macOS and on Windows, where the
platform gives the Client no handle that pins the peer's process; the second service noticing that
its socket was replaced; a socket the service manager creates and owns (socket activation); what the
Managed Processes can do to one another under the one account beyond taking the second service's
place, which ADR-0059 leaves to them; a peer that is the second service's process but runs a build
other than the Client's, which the handshake already refuses (ADR-0059 clause 5).

## Alternatives considered

- **A socket the service manager owns.** systemd (`ListenStream=` in a socket unit) and launchd
  (`Sockets`) can create the socket as root in a directory the process account cannot write, and
  hand it to the second service, so a Managed Process cannot replace it. It closes the replacement
  on Linux and macOS without asking for a process, but not a Managed Process reaching into the
  second service, and Windows has no such mechanism for a named pipe, so the Client would need the
  process check there anyway; two mechanisms, and a change to the units, the plist, the socket's
  place and the command line. Not taken.
- **Close the Windows process too.** A security descriptor on the second service's process that
  denies its own account, through `OWNER RIGHTS`, the rights to write its memory, create threads
  in it and duplicate its handles, and a handle the Client holds on the peer's process to pin its
  id. More Windows code that only CI exercises, for a case of low severity. Not taken now; it stays
  a follow-up.
- **The second service checks that its socket is still its own** and binds it again where it was
  replaced. It narrows the window but does not close it — the impostor can replace the socket
  again between two checks — and does nothing on Windows. Rejected; the Client's check closes the
replacement.
- **A secret only the Client and the second service share** — a token the second service proves
  at the handshake. The second service would have to keep it where its own Managed Processes,
  running under its account, cannot read it. On Linux a process that is not dumpable would keep it
  so, but the process check needs nothing kept, and on Windows the secret would be as open as the
  process. Rejected.
- **Report a refused peer to the Server too.** The Server would see it in the fleet view. The
  Client's health already shows the second service unreachable, and the log names the impostor;
  a new health detail is a protocol matter of its own. Not taken now.
- **Keep the replacement as a residual.** The separation holds everywhere else; leaving this case
  means a Managed Process can lie about what runs and keep what it starts alive past the Client's
  connection. Rejected; what remains is named under *Out of scope*.

## Sources / Prior art

- `unix(7)`, `SO_PEERCRED`: the peer's credentials as they were when it called `connect(2)` or
  `listen(2)`; `SO_PEERPIDFD` (Linux 6.5): a pidfd for that process. `pidfd_open(2)`,
  `pidfd_send_signal(2)`: a pidfd refers to one process and never to a later holder of its id.
- `prctl(2)`, `PR_SET_DUMPABLE`, and `ptrace(2)`, *Ptrace access mode checking*: a process that is
  not dumpable is denied to a caller without `CAP_SYS_PTRACE` in the user namespace, which governs
  `PTRACE_ATTACH`, `/proc/<pid>/mem` and `pidfd_getfd(2)`; the mark is reset by `execve(2)`. The
  Yama documentation (`Documentation/admin-guide/LSM/Yama.rst`) for `ptrace_scope`.
- Apple, `<sys/un.h>`: `LOCAL_PEERPID` at `SOL_LOCAL`; `task_for_pid` and the
  `com.apple.security.cs.debugger` entitlement.
- Microsoft Learn, `GetNamedPipeServerProcessId`, `QueryServiceStatusEx`
  (`SC_STATUS_PROCESS_INFO`, `SERVICE_STATUS_PROCESS.dwProcessId`), and *Process Security and
  Access Rights*.
- `systemd.service(5)` (the main process of a service) and `org.freedesktop.systemd1(5)`
  (`MainPID`); `launchctl(1)` (`print`, whose output it does not promise as an interface).
- polkit's `unix-process` subject and CVE-2013-4288: a check of a caller by process id alone is
  open to the id's reuse, which polkit answered with the process's start time and later pidfds.
- This repository: [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)
  clauses 3, 4, 5 and 9; `crates/fleet-agent/src/supervisor/channel.rs` (`Channel::open`);
  `docs/HARDENING.md`.

## Consequences

- Positive: a Managed Process can no longer take the second service's place by replacing its
  socket or making an instance of its pipe, so the Client does not tell it a program, an
  environment or the Supervisor Endpoint's token, and the programs the Client knows of end with
  its connection, as ADR-0059 clause 4 promises. On Linux, the Managed Processes can no longer
  read the second service's memory either, which holds what the Client sends it.
- Negative / trade-offs: on Windows a Managed Process can still take the second service's place
  by opening its process, and on Linux before 6.5, on macOS and on Windows by waiting for a process
  id to come round again; both are named, not closed. Every new connection asks the service manager
  once — a `systemctl` or `launchctl` run, or an SCM query — which adds a few milliseconds at the
  Client's start and after the second service restarts. On Linux that needs systemd's system bus,
  and on macOS it reads `launchctl print`, whose output is not a promised interface; where either
  fails, every connection is refused and no Managed Process starts until it answers again, which
  is logged. The second service writes no core dump on Linux. A Managed Process can still remove
  or replace the socket, and while it does the Client finds the second service away; that is a
  denial of what the process account's own programs run, which the shared account already allows.
- Follow-ups: closing the second service's process on Windows and pinning the peer's process
  there and on macOS; a socket the service manager owns on Linux and macOS; a health detail that
  names a refused peer.

## Enforcement

Tests that will carry `Verifies: ADR-0061`:

- With a fake service manager that names one process as the second service, the Client accepts a
  peer of that process and the process account, and refuses one of the process account whose
  process differs, one whose process cannot be read, and one the service manager names no process
  for — each before it writes anything, each treated as a second service that is away, and each
  retried on a new connection.
- On Linux, which CI runs with a kernel that has `SO_PEERPIDFD` and where the test fails rather
  than passes without it: a peer whose listening process has ended, while another process holds
  the listening socket, is refused although the service manager names a process of the same id;
  an error of `SO_PEERPIDFD` other than `ENOPROTOOPT` refuses the peer and falls back to nothing.
- On Linux the second service, once listening, reports itself not dumpable
  (`PR_GET_DUMPABLE`), and the test, as its parent and without `CAP_SYS_PTRACE`, so that Yama
  permits it, is refused `PTRACE_ATTACH`, `/proc/<pid>/mem` and `pidfd_getfd`; a program it starts
  is dumpable. The second service does not listen while it is traced.
- The service smoke test: on Linux and macOS a Managed Process the second service started leaves
  its process group and replaces the socket, and the Client, restarted, refuses it, writes it
  nothing, logs it, and starts no Managed Process until the second service is restarted. On
  Windows, with `NT AUTHORITY\LocalService` as the process account and the second service
  stopped, a task scheduled under that account creates the pipe; the Client, restarted, refuses
  it as a peer the SCM names no process for, writes it nothing and logs it, and once the task has
  ended and the second service runs again, reaches the second service. Which instance of a pipe a
  client opens while the second service holds one too is not determined, so the case of a
  differing process is decided on Windows by the test with a fake service manager.
