# ADR-0036: On Windows the second service closes its process and its threads to the process account, and the Client holds the peer's process while it checks it

- **Status:** 🟢 accepted
- **Date:** 2026-10-27
- **Deciders:** Markus Brigl
- **Applies to:** the security of the second service's process and threads on Windows in `crates/fleet-agent/src/supervisor/channel/windows.rs`, `crates/fleet-agent/src/win32.rs` and the service's entry in `crates/fleet-agent/src/main.rs` and `crates/fleet-agent/src/service/`, the Client's check of the pipe's server process in `crates/fleet-agent/src/supervisor/channel/windows.rs`, the Windows service smoke in `crates/fleet-agent/tests/service_smoke.rs`, and every document that says what the separation by a process account holds against a Managed Process on Windows

## Context

Where a process account is registered, every Managed Process runs under it, and so does the second
service that starts them ([ADR-0037](0037-the-accounts-the-service-runs-under.md)).
Clause 19 of ADR-0020 has the Client speak only to the process its service manager runs as the
second service, and closes that process to the Managed Processes where the platform allows: on
Linux it marks itself not dumpable, and on macOS `task_for_pid` refuses a caller of the same user.
On Windows the process keeps the default security its account gives it.
That default lets every process of the same account open it with the rights the account holds,
among them `PROCESS_VM_READ`, `PROCESS_VM_WRITE`, `PROCESS_CREATE_THREAD` and `PROCESS_DUP_HANDLE`.
A Managed Process can therefore read what the Client sends the second service, the Supervisor
Endpoint's token among it, run code in it, or duplicate its pipe handle and answer the Client in
its place. ADR-0020 names this under *Out of scope*, together with a process id reused on Windows,
and names closing the Windows process and pinning the peer's process as follow-ups.

Windows checks every `OpenProcess` and `OpenThread` against the object's DACL. A caller without
`SeDebugPrivilege`, which the process account does not hold (ADR-0037 clause 7), gets only what the
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

1. **[ADR-0037](0037-the-accounts-the-service-runs-under.md) clause 19 restates ADR-0020 clause 19
   for this decision: the channel admits the Client's account alone, the Client speaks to the
   second service alone, and the channel ends with the Client.**

**Out of scope:** the process itself from its creation until its program's first act; a thread the
runtime creates, in the instant between its creation and its own first act; a thread the system
starts in the process, until the next snapshot, at most a second later; a broker outside the job, a
WMI process creation or a scheduled task, through which a Managed Process outlives the job (ADR-0037
clause 19); a process id reused on macOS, or on Windows before the Client opens the process the pipe
names; what the Managed Processes can do to one another under the one account.

## Alternatives considered

- **Change the token's default DACL instead of each thread's.** Every later thread would be covered
  from its creation, but every Managed Process would inherit the DACL and be closed to its own
  account too, which may break an agent that inspects its own processes. Starting each program
  under a token of its own instead brings back the token handling of `CreateProcessAsUserW` that
  ADR-0037 kept out of the second service. Rejected.
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
- This repository: [ADR-0037](0037-the-accounts-the-service-runs-under.md) clauses 17, 19 and 21;
  `crates/fleet-agent/src/supervisor/channel/windows.rs`; `docs/HARDENING.md`.

## Consequences

- Positive: on Windows a Managed Process can no longer read or write the second service's memory,
  run code in it or take its pipe handle, and a process id passed on while the Client checks the
  peer no longer passes. Windows reaches what Linux already holds.
- Negative / trade-offs: more Windows code that only CI exercises; a thread the runtime starts is
  open for the instant before it sets its own DACL; a process id freed and reused between
  `GetNamedPipeServerProcessId` and the Client's open of that process still passes, which a
  Managed Process can no longer bring about by ending the second service; an administrator's tool
  that opens the second service needs to run elevated.
- Follow-ups: pinning the peer's process on macOS.

## Enforcement

Tests that will carry `Verifies: ADR-0036`:

- On Windows, in `crates/fleet-agent/src/`: the process and thread DACLs are the ones of
  [ADR-0037](0037-the-accounts-the-service-runs-under.md) clause 19 and read back as such; a
  second service whose process or a thread carries another DACL does not listen; the Client
  refuses a peer whose process it cannot open, and one whose process has ended once the SCM has
  answered.
- The Windows service smoke: a task scheduled under `NT AUTHORITY\LocalService`, the smoke's
  process account, opens the second service's process with `PROCESS_VM_READ`,
  `PROCESS_DUP_HANDLE` and `PROCESS_CREATE_THREAD`, and one of its threads with
  `THREAD_SET_CONTEXT`, and is refused `ERROR_ACCESS_DENIED` each time, while a Managed Process
  still runs.
