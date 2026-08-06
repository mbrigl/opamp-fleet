# ADR-0020: The Client updates itself — its own Agent, a staged version, and a restart it does not issue

- **Status:** 🟡 proposed
- **Date:** 2026-08-06
- **Deciders:** Markus Brigl

## Context

Goal 10 says the Server updates *"an agent's binary — the Collector's, **and the Client's own**"*, and
goal 11 asks for a Client that *"can replace its own binary in place — a self-update that survives the
service restart and is rolled back on failure"*. Package delivery to Managed Processes has been in
place since [ADR-0015](0015-package-delivery-for-managed-processes.md); the Client's own half is the
last row of that work still marked future in [`CONFORMANCE.md`](../CONFORMANCE.md).

Most of the machinery already exists, and two earlier decisions built deliberately toward this one.
[ADR-0010](0010-client-os-service-and-cli.md) put the Client in a **versioned side-by-side layout** —
`<root>/versions/opamp-client-<version>-<hash>/client`, a `current` symlink (Unix) or junction
(Windows) that the service is registered against, and a per-version `manifest.toml` carrying the full
version string and the binary's SHA-256, described there as *"what a future self-update verifies
against"*. It also chose `RestartPolicy::OnFailure` explicitly so that *"a future updater [can] stop
the service, switch `current`, and start it"*, and it already carries `heal_current` for a pointer
switch that was interrupted. ADR-0015 contributes the verified download (content hash always, Ed25519
when a key is configured), [ADR-0018](0018-packages-imported-from-a-url.md) the archive handling, and
[ADR-0017](0017-selector-targeted-packages.md) the per-Agent offer. What is missing is not plumbing.

Three problems are genuinely new, and none of them is solved by reusing ADR-0015.

**There is no Agent to offer the package to.** A Client with `[[supervisor]]` blocks presents *only*
its Supervisor Agents; the self-Agent exists only in the degenerate case of a Client with no
supervisors at all (`build_engine`). So on precisely the hosts that matter — the ones actually
managing something — nothing on the wire represents the Client itself. Nor can a Supervisor Agent
carry the Client's package on the side: the Baseline knows *"normally only one top-level package,
which implements the primary functionality of the Agent"*, that one is the Managed Process's binary,
and an `Addon` is the thing ADR-0015 refuses because a Supervisor has no way to apply one.

**The health gate cannot be where it is today.** ADR-0015 gates an install on *"the process I started
survived `apply_grace_secs`"*, judged by the Supervisor that started it. Here the process that
installs the package is the process that has to die for the install to take effect. Nothing is left
to watch, and nothing is left to roll back — the rollback in ADR-0015 works precisely because the
Supervisor outlives its Managed Process.

**The restart cannot be self-issued.** Calling `systemctl restart` on your own unit from inside it
synchronously waits on a job ordered against the job you are part of, which deadlocks. Spawning a
helper to do it does not escape the problem either: a child inherits the unit's cgroup and systemd's
default `KillMode=control-group` kills it along with the service it was supposed to restart. Escaping
that needs `systemd-run --scope` — a Linux-only mechanism with no launchd or SCM equivalent.

One thing is *not* a problem, and only because ADR-0010 saw it coming: Windows locks a running `.exe`,
so an in-place overwrite is impossible there. Side-by-side version directories make the question moot
on all three platforms — the new binary is never written over the old one.

There is no upstream answer to copy. The OpenTelemetry `opampsupervisor` updates the Collector it
supervises and says nothing about updating itself; its specification has no self-update section and no
package configuration of its own.

Finally, a hazard this creates rather than inherits. Under ADR-0017 a package with an empty Selector
reaches **every** Agent that accepts packages. The moment the Client is an Agent that accepts
packages, a fleet-wide Collector package would be offered to the Client itself — and installed over
it. That is a way to brick a fleet, and it has to be closed by this decision, not documented as a
caveat.

## Decision

We will make the Client its own Agent and update it by staging a new version beside the running one,
proving the new binary before committing to it, and letting the service manager perform the restart.

1. **The Client is always its own Agent.** The self-Agent exists *alongside* Supervisor Agents rather
   than only instead of them, with its own `instance_uid`, its own health, and its own
   `service.version` — the Client's baked ADR-0009 version. This is worth having for its own sake:
   today a Client with supervisors is invisible in the fleet, and nobody can ask which Client version
   a host runs.

2. **Self-update is opt-in, and names its package.** A `[self_update]` section in `client.toml`
   enables it and carries `package = "<name>"`. The self-Agent declares `AcceptsPackages` only while
   that section is present, and **refuses any offered top-level package whose name is not the
   configured one**, reporting `InstallFailed` with that reason. This is what closes the
   fleet-wide-package hazard: consenting to be updated is not the same as consenting to receive
   whatever the fleet is receiving. A Server able to replace the binary that manages every other
   binary on the host is a larger grant than one able to replace a Collector, and it is made
   deliberately, per Client.

3. **An install is a staged version, never an overwrite.** The verified artifact is unpacked
   (ADR-0018) into `<root>/versions/opamp-client-<version>-<hash>/`, its `manifest.toml` written, and
   the binary marked executable — the same layout `client service install` already produces. The
   running binary is never touched.

4. **The new binary is proved before the pointer moves.** The staged binary is executed as a child
   with a self-check subcommand that only this Client answers, and must report the version its
   manifest claims. A binary that cannot exec — wrong architecture, truncated artifact, missing
   loader — is the one failure class no post-restart mechanism can catch, because a binary that never
   runs never notices anything. It is also what distinguishes the Client's own binary from some other
   program that was offered under the configured name. A failed probe fails the install with the
   previous version still current and still running.

5. **The restart is the service manager's, triggered by a deliberate exit.** Once `current` points at
   the new directory, the Client reports `Installing`, shuts down gracefully — Managed Processes
   stopped, `agent_disconnect` sent — and exits with a distinguished non-zero code. The installed
   unit's `OnFailure` policy (ADR-0010) brings it back after its delay, now through the switched
   pointer. No unit manipulates itself, and nothing has to survive the unit stopping.

   **This mechanism is not free on all three platforms, and pretending otherwise would ship a feature
   that silently works on two of them.** "Restart on a non-zero exit" is native on systemd and
   launchd and is *off by default* on Windows:

   | | What makes the restart happen | Already true? |
   |---|---|---|
   | **Linux** (systemd) | `Restart=on-failure` — a non-zero exit is a failure | Yes: what `RestartPolicy::OnFailure` installs |
   | **macOS** (launchd) | `KeepAlive { SuccessfulExit: false }` — restart unless the job exited 0 | Yes: the same policy maps to this |
   | **Windows** (SCM) | Recovery actions, **plus** `SERVICE_CONFIG_FAILURE_ACTIONS_FLAG` | **No — three gaps** |

   The Windows gaps are specific, and all three must be closed:

   - **Nothing configures a restart at all.** `service-manager`'s Windows backend is the `sc.exe`
     wrapper, and its `install` *discards* the restart policy — it matches on it only to log
     `"sc.exe does not support automatic restart policies through 'sc create'; service '…' will not
     restart automatically"`. So the `RestartPolicy::OnFailure` ADR-0010 asked for has never been in
     effect on Windows, and no amount of exiting with the right code would have brought the Client
     back. The installer has to configure the recovery actions itself, through
     `Service::update_failure_actions` in the `windows-service` crate that is already a Windows-target
     dependency.
   - **SCM ignores a clean non-zero exit unless told not to.** Even with recovery actions configured,
     they run when a service dies *without* reporting `SERVICE_STOPPED`, or reports it with a non-zero
     exit code **only if** `fFailureActionsOnNonCrashFailures` is true — false by default. So
     `Service::set_failure_actions_on_non_crash_failures(true)` is required as well. Elastic Agent
     shipped without this flag and got exactly the silence it implies.
   - **The shim always reports success.** `service/windows.rs` builds every status with
     `ServiceExitCode::Win32(0)`, so even a deliberate failure exit reaches the SCM as a clean stop.
     The run's exit code has to reach the final `set_service_status` instead of being hard-coded.

   None of the three is a workaround for Windows being different; together they are the Windows
   spelling of the sentence the other two managers already say. What must **not** happen is the
   alternative that suggests itself — crashing on purpose so the SCM sees an unexpected termination —
   because that throws away the graceful shutdown, the `agent_disconnect`, and the orderly stopping of
   Managed Processes, and it would make every self-update look like a fault in the event log.

6. **The new version commits itself, or rolls itself back.** Before the pointer moves, an
   `update-marker` is written into the state directory recording the previous version directory, the
   new one, the offered package hash, and an attempt counter. On start:
   - a Client that finds no marker does what it does today;
   - a Client that finds one increments the attempt counter, and **commits** once it has stayed up for
     the apply grace and reached the Server — deleting the marker and reporting `Installed` at the new
     version;
   - a Client that finds a marker whose attempts exceed a small bound repoints `current` at the
     recorded previous directory and exits, so the manager brings the old version back, which finds
     the marker, reports `InstallFailed` with the recorded reason, and deletes it.

   This covers the "starts but does not stay up" class with the same restart loop that would otherwise
   be the problem, and needs no process outside the unit.

7. **The outcome is reported after the restart, by whichever version is running.** `Installing` goes
   out before the exit; the terminal status necessarily comes from a different process than the one
   that started the install. Either way the offered `all_packages_hash` is echoed once terminal, which
   is what stops the Server re-offering — the same rule ADR-0015 already follows, and the reason a
   refused or failed self-update does not become a loop.

A downgrade needs no separate treatment: it is the ordinary offer naming an older artifact, exactly as
in [ADR-0019](0019-one-step-back.md), and the same rollback path applies to it.

## Alternatives considered

- **A separate watcher process, as Elastic Agent does.** The closest prior art, and the source of the
  marker file in point 6: Elastic writes an `.update-marker` recording the previous version and hash,
  spawns `elastic-agent watch` after the upgrade, and flips the symlink back if the new version does
  not check in. Rejected in that shape because a watcher spawned from a systemd service dies with it
  under the default `KillMode=control-group`, and escaping the cgroup is a Linux-only manoeuvre with
  nothing equivalent on launchd or SCM — three platform-specific escapes to write and maintain. The
  marker survives on its own; splitting the observation across the restart into "prove before, count
  after" gets the same coverage from one process.
- **Overwrite the binary in place, as ADR-0015 does for a Managed Process.** Rejected: Windows locks a
  running `.exe`, and the whole point of ADR-0010's side-by-side layout was to not need this. It would
  also throw away the thing that makes rollback cheap — the previous version still sitting on disk.
- **Leave self-update to the OS package manager (apt, MSI, Homebrew).** Genuinely how much
  infrastructure is updated, and it keeps the Client out of the business of rewriting itself.
  Rejected because goals 10 and 11 put this in the protocol deliberately: a fleet that reaches its
  agents only through OpAMP should not need a second, per-platform distribution channel to update the
  thing that speaks OpAMP.
- **Give the Client's package to an existing Supervisor Agent.** Rejected: an Agent has one top-level
  package and it is the Managed Process's binary, and an `Addon` is precisely what a Supervisor cannot
  apply. It would also make the self-update of a host depend on it happening to supervise something.
- **A dedicated package type or a reserved package name in the protocol.** Rejected as forbidden by
  the specification's non-goal *"Forking or extending the protocol"* — the name matching in point 2 is
  Client-side policy over an ordinary package, not a new protocol meaning.
- **Roll back on post-update health rather than on "did it come back".** Tempting and much larger: it
  needs a definition of "degraded" that is not "the process exited", a window to judge it over, and
  something to stop a fleet oscillating between two versions. Rejected as its own decision, for the
  same reason ADR-0019 rejected it.
- **Always present the self-Agent, but let it accept any package.** Rejected: this is the
  brick-the-fleet path described in the context. The configured package name is cheap and the failure
  it prevents is not recoverable remotely.

## Sources / Prior art

- [Elastic Agent upgrade documentation](https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md)
  — the update marker, the watcher, and the symlink flip; already the source of ADR-0010's
  `<component>-<version>-<hash>` directory scheme, and the model this decision follows in substance
  while dropping the separate process.
- [OpAMP Supervisor specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — checked as the behavioural oracle this project usually follows: it covers Collector executable
  updates and has no self-update section at all, so there is no upstream shape to match here.
- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"normally only one top-level package, which implements the primary functionality of the Agent"*,
  and `PackageAvailable` as an offer to *"install a new package or initiate an upgrade or downgrade"*.
- [systemd-devel on self-restart deadlocks](https://lists.freedesktop.org/archives/systemd-devel/2015-February/027966.html)
  — synchronously waiting on a job ordered against your own job deadlocks, which is why point 5 does
  not issue the restart.
- [Microsoft: replacing an in-use file](https://learn.microsoft.com/en-us/sysinternals/downloads/pendmoves)
  — the rename-to-delete and `MoveFileEx` dance a running `.exe` would otherwise require, and which
  side-by-side versions make unnecessary.
- [`SERVICE_FAILURE_ACTIONS_FLAG`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_failure_actions_flag)
  — failure actions run on a `SERVICE_STOPPED` with a non-zero `dwWin32ExitCode` *only* when
  `fFailureActionsOnNonCrashFailures` is set; it is false by default. The single fact that makes the
  restart mechanism a per-platform question rather than one behaviour.
- [Elastic: agent installed on Windows without the non-crash failure flag](https://discuss.elastic.co/t/elastic-agent-service-installed-on-windows-without-noncrash-failure-flag/374043)
  — the same prior art that supplied the marker, getting this exact detail wrong in the field.
- [`launchd.plist(5)`](https://www.manpagez.com/man/5/launchd.plist/) — `KeepAlive` with
  `SuccessfulExit = false` restarts a job unless it exited zero, which is macOS's spelling of
  `Restart=on-failure`.
- [`windows-service` crate](https://docs.rs/windows-service/0.8.0/windows_service/service/struct.Service.html)
  — already a Windows-target dependency (ADR-0010) and it exposes both
  `set_failure_actions_on_non_crash_failures` and `update_failure_actions`, so closing the Windows gap
  needs no new dependency.
- `service-manager` 0.11, `src/sc.rs` — read directly rather than taken from its documentation: the
  Windows `install` matches on `ctx.restart_policy` only to emit a warning and never configures
  failure actions. The cross-platform abstraction is not one here, which is why point 5 has a
  per-platform table instead of a single sentence.

## Consequences

- Positive: goal 11 closes, and the layout ADR-0010 built — versions, pointer, manifest hash,
  `heal_current` — is finally used for what it was designed for.
- Positive: the Client becomes visible in its own fleet. Its version, health, and connection show up
  like any other Agent, which today they do not on any host that supervises something.
- Positive: closing the Windows gaps in point 5 fixes a **pre-existing defect wider than
  self-update**. A Windows Client was not restarted after *any* failure — not a crash, not a panic,
  not an error exit — because the `sc.exe` backend silently discards the restart policy and only logs
  a warning. ADR-0010 states the Client restarts on failure; on one of its three platforms that had
  never been true. Goal 11's "on every platform" is what surfaced it, and it is being fixed **ahead**
  of this decision as an ordinary bug fix, so that this decision inherits a working restart rather
  than having to build one. That fix is what makes point 5 a mechanism rather than a wish.
- Negative / trade-offs: **every Client becomes an extra Agent.** Fleet counts change, the fleet view
  grows a row per Client, and any Selector written as "everything" now means something wider than it
  did. This is the most disruptive part of the decision and it is not opt-in — the Agent exists
  whether or not `[self_update]` does, because a Client invisible in its own fleet was already the
  wrong default.
- Negative / trade-offs: a self-update is reported across a process boundary, so there is a window in
  which the Server has seen `Installing` and will not see anything further until the new version
  connects. A Client that never comes back is indistinguishable, from the Server, from one whose host
  went down — the marker makes the *host* recover, but the Server learns nothing until it does.
- Negative / trade-offs: the attempt bound in point 6 trades a crash-looping new version against a
  premature rollback of a version that is merely slow to start on a loaded host. The bound and the
  apply grace are the only tuning, and getting them wrong is visible either as a fleet stuck on a
  broken version or as an update that will not stick.
- Negative / trade-offs: **the pointer switch is atomic on Unix and is not on Windows.** On Unix
  `set_current` renames a staging symlink over `current`, so there is no instant without a pointer. A
  junction cannot be renamed over: ADR-0010 removes it and recreates it, which is why that function
  says callers switch only while the service is stopped. Self-update switches it while running, and
  two things follow. Removing the junction while the process runs is safe in itself — the running
  image is held by handle, and removing a reparse point does not touch its target — but between the
  remove and the `mklink /J` there is a window in which `<root>/current` does not exist. The window is
  entirely inside the installing process's own lifetime, so a *failed* recreate is recoverable: that
  process is still alive, and points the junction back at its own directory and fails the install.
  What is not recoverable is the process being killed inside that window, which leaves a host whose
  service has no program to start. It is milliseconds wide and it is real, and closing it properly
  would mean a different Windows layout than ADR-0010 chose — so this decision accepts it, names it,
  and does not pretend the two platforms behave alike.
- Follow-ups: whether a Client should refuse to self-update while one of its Managed Processes is
  mid-package-install, so two swaps do not overlap on one host. And whether the self-Agent should
  report a distinguishing attribute the fleet view surfaces, so an operator can filter Clients from
  the agents they manage without knowing the naming convention.
