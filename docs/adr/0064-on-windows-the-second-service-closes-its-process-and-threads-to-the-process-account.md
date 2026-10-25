# ADR-0064: On Windows the second service closes its process and its threads to the process account, and the Client holds the peer's process while it checks it

- **Status:** 🟡 proposed
- **Date:** 2026-10-25
- **Deciders:** Markus Brigl
- **Applies to:** the security of the second service's process and threads on Windows in `crates/fleet-agent/src/supervisor/channel/windows.rs`, `crates/fleet-agent/src/win32.rs` and the service's entry in `crates/fleet-agent/src/main.rs` and `crates/fleet-agent/src/service/`, the Client's check of the pipe's server process in `crates/fleet-agent/src/supervisor/channel/windows.rs`, the Windows service smoke in `crates/fleet-agent/tests/service_smoke.rs`, and every document that says what the separation by a process account holds against a Managed Process on Windows
- **Supersedes:** [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md) clause 4

## Context

Where a process account is registered, every Managed Process runs under it, and so does the second
service that starts them
([ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md)). Clause 4 of ADR-0059 has the Client speak only to the process its service
manager runs as the second service, and closes that process to the Managed Processes where the
platform allows: on Linux it marks itself not dumpable, and on macOS `task_for_pid` refuses a
caller of the same user. On Windows the process keeps the default security its account gives it.
That default lets every process of the same account open it with the rights the account holds,
among them `PROCESS_VM_READ`, `PROCESS_VM_WRITE`, `PROCESS_CREATE_THREAD` and `PROCESS_DUP_HANDLE`.
A Managed Process can therefore read what the Client sends the second service, the Supervisor
Endpoint's token among it, run code in it, or duplicate its pipe handle and answer the Client in
its place. ADR-0059 names this under *Out of scope*, together with a process id reused on Windows,
and names closing the Windows process and pinning the peer's process as follow-ups.

Windows checks every `OpenProcess` and `OpenThread` against the object's DACL. A caller without
`SeDebugPrivilege`, which the process account does not hold (ADR-0059 clause 9), gets only what the
DACL grants. A process's owner keeps `READ_CONTROL` and `WRITE_DAC` implicitly unless an
`OWNER RIGHTS` entry says otherwise. A thread is an object of its own, with a DACL of its own, and
`THREAD_SET_CONTEXT` on one is enough to run code in its process. A new process or thread whose
creator names no security descriptor gets one from the creator's token, so changing the token's
default DACL would reach every program the second service starts.

The specification's quality goal Q-1 asks that the Fleet be secure by default.

## Decision

We will have the second service on Windows give its process and every one of its threads a DACL
that leaves its own account only the right to ask about and wait for it, and have the Client hold
the peer's process by a handle while it checks it.

1. **Restates ADR-0059 clause 4. The channel admits the Client's account alone, the Client speaks to
   the second service alone, and the channel ends with the Client.** On Linux and macOS the second
   service listens on a Unix socket, on Windows on a named pipe. Every connection is checked by the
   peer's credentials (`SO_PEERCRED`, `getpeereid`, the pipe client's token) and refused unless its
   user is the Client's account. One connection is served at a time. When it closes, the second
   service stops every program it started, by its process group on Unix and by its job on Windows,
   so a Client that restarts finds no process it did not start.
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
     process in between. On Windows the Client opens the process `GetNamedPipeServerProcessId`
     names with `SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` before it asks the SCM, holds that
     handle until it has decided, and requires that the process has not ended once the SCM has
     answered: while the handle is open the id cannot pass to another process. A process it cannot
     open is one that cannot be read. The account is still read as the bullet above says, from the
     peer's uid or the pipe's owner. A connection whose process cannot be read, or that the service manager
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
     to a caller of the same user without the debugging entitlement. On Windows it gives its process
     and every one of its threads a protected DACL as the first thing each does, before the program
     reads its configuration or sets up its log (below).
   - **On Windows the process account may only ask about and wait for the second service.** The
     process carries `D:P(A;;0x101000;;;<process account>)(A;;0x101000;;;<Client>)(A;;RC;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)`:
     `SYNCHRONIZE` and `PROCESS_QUERY_LIMITED_INFORMATION` for the process account and for the
     Client's account, `READ_CONTROL` alone for its owner (`OWNER RIGHTS`), so the owner holds no
     implicit `WRITE_DAC`, and full access for `LocalSystem` and the Administrators. Each thread
     carries `D:P(A;;0x100800;;;<process account>)(A;;RC;;;OW)(A;;GA;;;SY)(A;;GA;;;BA)`,
     `SYNCHRONIZE` and `THREAD_QUERY_LIMITED_INFORMATION` for the process account. So a Managed
     Process, which holds no `SeDebugPrivilege`, can neither read nor write the second service's
     memory, start or steer a thread in it, take its handles, suspend it nor end it. The DACLs are
     set through `SetSecurityInfo` on the process's and each thread's own handle: the main thread
     sets the process's and its own and, from a snapshot, those of the threads that exist by then;
     the thread the SCM starts for the service and every thread of the runtime set their own as
     their first act. Threads the system starts in the process, such as those of its thread pool,
     are set from a snapshot of every thread, taken again once a second while the second service
     serves. Before it listens, the second service sets them so once more, then reads back the DACL
     of its process and of every thread it has, compared by what they say as ADR-0059 clause 6
     compares an ACL, and does not listen, ending with an error, while one differs. `GA` stands for
     the access it maps to on a process and on a thread, `0x1fffff`, which is what is written, so
     that what is read back compares equal. The token's default DACL is left as it is, so the
     programs it starts, which are created with the security their own account gives them, are not
     touched.
   - **Each program runs in a job of its own.** On Windows the second service starts every program
     suspended, assigns it to a new job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and only
     then lets it run, so everything it starts is in that job too. At the end of each connection,
     and so before it reports itself stopped, the second service ends every job it holds and
     waits, up to five seconds, until none holds a process.

**Out of scope:** the process itself from its creation until its program's first act; a thread
the runtime creates, in the instant between its creation and its own first act; a thread the system
starts in the process, until the next snapshot, at most a second later; a broker outside the job, a WMI process creation or a scheduled task, through which a
Managed Process outlives the job (ADR-0059 clause 4); a process id reused on macOS; what the Managed
Processes can do to one another under the one account.

## Alternatives considered

- **Change the token's default DACL instead of each thread's.** Every later thread would be covered
  from its creation, but every Managed Process would inherit the DACL and be closed to its own
  account too, which may break an agent that inspects its own processes. Starting each program
  under a token of its own instead brings back the token handling of `CreateProcessAsUserW` that
  ADR-0059 kept out of the second service. Rejected.
- **Leave `PROCESS_TERMINATE` to the process account.** A Managed Process could end the second
  service, which the service manager restarts; that is a denial and no takeover, and the shared
  account allows other denials anyway. Not taken: the rule is the narrowest grant that still lets
  the Client check the process.
- **Set the DACL only when the second service starts to listen.** Robust in operation, but not fail
  closed, and the memory read before that holds the configuration. Rejected.
- **A protected process (PPL).** It needs a signing level the product does not have. Rejected.
- **Only a unit test of the DACL.** It would not show that a process of the same account is
  refused. Rejected in favour of the smoke below.

## Sources / Prior art

- Microsoft Learn, *Process Security and Access Rights*: the default security descriptor comes
  from the creator's token, `OpenProcess` is checked against the process's DACL, `SeDebugPrivilege`
  overrides it, and `PROCESS_DUP_HANDLE` alone yields a handle with full access; *Thread Security
  and Access Rights*.
- Microsoft Learn, *SID Strings*: `OW` (`OWNER RIGHTS`), `SY`, `BA`.
- Recalled, not re-read for this record: that an object's owner holds `READ_CONTROL` and
  `WRITE_DAC` implicitly unless an `OWNER RIGHTS` entry names what it holds, and that
  `THREAD_SET_CONTEXT` is enough to run code in a thread's process. The smoke under *Enforcement*
  decides both on a real host.
- Microsoft Learn, `SetSecurityInfo`, `GetNamedPipeServerProcessId`, `OpenProcess`,
  `CreateToolhelp32Snapshot`.
- This repository: [ADR-0059](0059-managed-processes-run-under-a-process-account-started-by-a-second-service.md) clauses 2, 4 and 6;
  `crates/fleet-agent/src/supervisor/channel/windows.rs`; `docs/HARDENING.md`.

## Consequences

- Positive: on Windows a Managed Process can no longer read or write the second service's memory,
  run code in it or take its pipe handle, and a reused process id no longer passes the Client's
  check. Windows reaches what Linux already holds.
- Negative / trade-offs: more Windows code that only CI exercises; a thread the runtime starts is
  open for the instant before it sets its own DACL; an administrator's tool that opens the second
  service needs to run elevated.
- Follow-ups: pinning the peer's process on macOS.

## Enforcement

Tests that will carry `Verifies: ADR-0064`:

- On Windows, in `crates/fleet-agent/src/`: the process and thread DACLs are the ones of clause 1 and
  read back as such; a second service whose process or a thread carries another DACL does not
  listen; the Client refuses a peer whose process it cannot open, and one whose process has ended
  once the SCM has answered.
- The Windows service smoke: a task scheduled under `NT AUTHORITY\LocalService`, the smoke's
  process account, opens the second service's process with `PROCESS_VM_READ`,
  `PROCESS_DUP_HANDLE` and `PROCESS_CREATE_THREAD`, and one of its threads with
  `THREAD_SET_CONTEXT`, and is refused `ERROR_ACCESS_DENIED` each time, while a Managed Process
  still runs.
