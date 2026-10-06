# ADR-0017: The Client updates itself — its own Agent, a staged version, a restart it does not issue, and a consent that stands unless it is withdrawn

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

Goal 10 says the Server updates *"an agent's binary — the Collector's, **and the Client's own**"*, and
goal 11 asks for a Client that *"can replace its own binary in place — a self-update that survives the
service restart and is rolled back on failure"*. Package delivery to Managed Processes is decided in
[ADR-0015](0015-package-delivery-for-managed-processes.md).

Most of the machinery already exists, and two earlier decisions built deliberately toward this one.
[ADR-0010](0010-client-os-service-and-installation-layout.md) put the Client in a **versioned side-by-side layout** —
a directory per version under `<root>/versions/`, a `current` symlink (Unix) or junction
(Windows) that the service is registered against, and a per-version `manifest.toml` carrying the full
version string and the binary's SHA-256, described there as *"what a future self-update verifies
against"*. It also chose `RestartPolicy::OnFailure` explicitly so that *"a future updater [can] stop
the service, switch `current`, and start it"*, and it carries `heal_current` for a pointer
switch that was interrupted. ADR-0015 contributes the verified download (content hash always, Ed25519
when a key is configured), [ADR-0015](0015-package-delivery-for-managed-processes.md) the archive handling, and
[ADR-0016](0016-a-package-is-a-versioned-set.md) the per-Agent offer. What is missing is not plumbing.

Three problems are genuinely new, and none of them is solved by reusing ADR-0015.

**There is no Agent to offer the package to.** Without a self-Agent, a Client with `[[supervisor]]`
blocks presents *only* its Supervisor Agents. So on precisely the hosts that matter — the ones
actually managing something — nothing on the wire represents the Client itself. Nor can a Supervisor
Agent carry the Client's package on the side: the Baseline knows *"normally only one top-level
package, which implements the primary functionality of the Agent"*, that one is the Managed Process's
binary, and an `Addon` is the thing ADR-0015 refuses because a Supervisor has no way to apply one.

**The health gate cannot be where it is for a Managed Process.** ADR-0015 gates an install on *"the
process I started survived `apply_grace_secs`"*, judged by the Supervisor that started it. Here the
process that installs the package is the process that has to die for the install to take effect.
Nothing is left to watch, and nothing is left to roll back — the rollback in ADR-0015 works precisely
because the Supervisor outlives its Managed Process.

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

A hazard this creates rather than inherits: under ADR-0016 a package with an empty Selector reaches
**every** Agent that accepts packages. The moment the Client is an Agent that accepts packages, a
fleet-wide Collector package would be offered to the Client itself — and installed over it. That is a
way to brick a fleet, and it has to be closed by this decision, not documented as a caveat. A consent
with no name attached would let the first fleet-wide artifact an operator uploads be written over the
Client and take the host out of reach. The name is what makes the consent specific.

The grant is real, and so is its alternative. A default of "absent means no" is the wrong way round
for a fleet. The absent section is the common state, and it makes the Client the one program in the
fleet that has to be patched by hand on every host — which is the work fleet management exists to
end. An un-updatable agent is a security position too, and a worse one, because a Client that cannot
be updated cannot be *fixed*. The asymmetry is a large grant against a fleet-wide patching gap, not
against a small convenience. An opt-in default is also unreachable on the path most hosts take: an
installer that collects only the endpoint gives a host no way to consent, at install time or ever,
unless someone knows that a section exists, what it is called, and that its absence is what silences
it.

## Decision

We will make the Client its own Agent and update it by staging a new version beside the running one,
proving the new binary before committing to it, and letting the service manager perform the restart.
The consent to be updated **stands by default, narrowed to the Client's own Agent type**, and every
install path can withdraw it.

### The update

1. **The Client is always its own Agent.** The self-Agent exists *alongside* Supervisor Agents rather
   than only instead of them, with its own `instance_uid`, its own health, and its own
   `service.version` — the Client's baked ADR-0009 version. This is worth having for its own sake:
   a Client with supervisors would otherwise be invisible in the fleet, and nobody could ask which
   Client version a host runs.

2. **Self-update names its package.** The `[self_update]` section carries `package = "<name>"`. The
   self-Agent declares `AcceptsPackages` while the consent stands (clauses 8 and 9), and **refuses
   any offered top-level package whose name is not the configured one**, reporting `InstallFailed`
   with that reason. This is what closes the fleet-wide-package hazard: consenting to be updated is
   not the same as consenting to receive whatever the fleet is receiving. A Server able to replace
   the binary that manages every other binary on the host is a larger grant than one able to replace
   a Collector, and the name is what keeps it to the one package that could be this Client.

3. **An install is a staged version, never an overwrite.** The verified artifact is unpacked
   (ADR-0015) into a new version directory under `<root>/versions/`, its `manifest.toml` written, and
   the binary marked executable — the same layout `service install` already produces. The version
   directory and the program in it are named as ADR-0022 clause 9 says. The running binary is never
   touched.

4. **The new binary is proved before the pointer moves.** The staged binary is executed as a child
   with a self-check subcommand that only this Client answers, and must report the version its
   manifest claims; how the two versions are compared is ADR-0009 clause 12. A binary that cannot
   exec — wrong architecture, truncated artifact, missing loader — is the one failure class no
   post-restart mechanism can catch, because a binary that never runs never notices anything. It is
   also what distinguishes the Client's own binary from some other program that was offered under the
   configured name. A failed probe fails the install with the previous version still current and
   still running.

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
     restart automatically"`. So the `RestartPolicy::OnFailure` ADR-0010 asks for is not in effect on
     Windows through that backend, and no amount of exiting with the right code would bring the
     Client back. The installer has to configure the recovery actions itself, through
     `Service::update_failure_actions` in the `windows-service` crate that is already a Windows-target
     dependency.
   - **SCM ignores a clean non-zero exit unless told not to.** Even with recovery actions configured,
     they run when a service dies *without* reporting `SERVICE_STOPPED`, or reports it with a non-zero
     exit code **only if** `fFailureActionsOnNonCrashFailures` is true — false by default. So
     `Service::set_failure_actions_on_non_crash_failures(true)` is required as well. Elastic Agent
     shipped without this flag and got exactly the silence it implies.
   - **The shim must not always report success.** `service/windows.rs` must not build every status
     with `ServiceExitCode::Win32(0)`, or even a deliberate failure exit reaches the SCM as a clean
     stop. The run's exit code has to reach the final `set_service_status` instead of being
     hard-coded.

   None of the three is a workaround for Windows being different; together they are the Windows
   spelling of the sentence the other two managers already say. What must **not** happen is the
   alternative that suggests itself — crashing on purpose so the SCM sees an unexpected termination —
   because that throws away the graceful shutdown, the `agent_disconnect`, and the orderly stopping of
   Managed Processes, and it would make every self-update look like a fault in the event log.

6. **The new version commits itself, or rolls itself back.** Before the pointer moves, an
   `update-marker` is written into the state directory recording the previous version directory, the
   new one, the offered package hash, and an attempt counter; it also carries the trace context
   ADR-0023 clause 33 requires. On start:
   - a Client that finds no marker starts normally;
   - a Client that finds one increments the attempt counter, and **commits** once it has stayed up for
     the apply grace and reached the Server — deleting the marker and reporting `Installed` at the new
     version;
   - a Client that finds a marker whose attempts exceed a small bound repoints `current` at the
     recorded previous directory and exits, so the manager brings the old version back, which finds
     the marker, reports `InstallFailed` with the recorded reason, and deletes it.

   This covers the "starts but does not stay up" class with the same restart loop that would otherwise
   be the problem, and needs no process outside the unit. The update in flight is resolved before the
   configuration (ADR-0037 clause 1), so a version that cannot read this host's configuration counts
   as a failed attempt too.

7. **The outcome is reported after the restart, by whichever version is running.** `Installing` goes
   out before the exit; the terminal status necessarily comes from a different process than the one
   that started the install. Either way the offered `all_packages_hash` is echoed once terminal, which
   is what stops the Server re-offering — the same rule ADR-0015 follows, and the reason a refused or
   failed self-update does not become a loop. Whether an offer is already installed is decided by the
   version this process runs (ADR-0035 clause 7).

Which versions are offered to the Client at all, a downgrade included, is ADR-0035's rule (clause
11). Whatever version is offered, the staging, the probe and the rollback above apply to it.

### The consent

8. **An absent `[self_update]` section is consent**, under the Client's own Agent type
   ([ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) clause 6), which is what a Set
   carrying this Client is keyed by anyway
   ([ADR-0016](0016-a-package-is-a-versioned-set.md)); `package` defaults to that
   type (ADR-0022 clause 7). The default is therefore not a wildcard — it is the one package that
   could legitimately be this Client, and an offer under any other name is refused and reported as
   clause 2 says.

9. **The withdrawal is written down**, as `enabled = false`. A Client the fleet cannot update says so
   in its own configuration rather than saying nothing at all, which is the state that was
   indistinguishable from an oversight.

10. **An empty package name with the consent standing fails at load**, naming the key. The name is the
    whole of the narrowing, and an empty one would widen the consent to whatever the Server offers
    next — a failure to catch at startup, not at the first offer.

11. **Every install path can answer it.** `service install --no-self-update` is the non-interactive
    withdrawal, beside `--endpoint`; `--self-update-package <NAME>` names a different package for a
    deployment whose Set is named differently; the questionnaire of
    [ADR-0020](0020-installing-the-client-and-native-installers.md) asks and defaults to yes;
    the MSI has a checked-by-default checkbox on its endpoint dialog and a public `SELFUPDATE`
    property, so `msiexec /qn … SELFUPDATE=0` withdraws it the way Intune and Group Policy will.

12. **The answer lands in the configuration file either way.** The installer's job is to write the
    first configuration ([ADR-0020](0020-installing-the-client-and-native-installers.md)), not
    to hold state of its own, so what the checkbox decided is visible and editable on the host
    afterwards.

## Alternatives considered

- **A separate watcher process, as Elastic Agent does.** The closest prior art, and the source of the
  marker file in clause 6: Elastic writes an `.update-marker` recording the previous version and hash,
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
  the specification's non-goal *"Forking or extending the protocol"* — the name matching in clause 2
  is Client-side policy over an ordinary package, not a new protocol meaning.
- **Roll back on post-update health rather than on "did it come back".** Tempting and much larger: it
  needs a definition of "degraded" that is not "the process exited", a window to judge it over, and
  something to stop a fleet oscillating between two versions. Rejected as its own decision.
- **Always present the self-Agent, but let it accept any package** — or default to consent with no
  name at all. Rejected outright: this is the brick-the-fleet path described in the context, since
  ADR-0016's empty Selector reaches every Agent and the first fleet-wide artifact would be written
  over the Client. The configured package name is cheap, the failure it prevents is not recoverable
  remotely, and the name is not ceremony.
- **Keep the default at "no" and only make the MSI able to say "yes".** The minimal fix. Rejected
  because it leaves the fleet-wide default at the state that makes a host un-updatable and silent
  about it: every host installed by a script that does not know the flag stays that way.
- **A third state: "ask the Server whether it has one for us".** No such negotiation exists in the
  Baseline, and inventing one to avoid writing a boolean would be a protocol extension for a
  configuration question.
- **Leave the config default alone and have the *installers* write `[self_update]` explicitly.**
  Tempting: existing hosts keep their behaviour exactly, and only new installs are updatable.
  Rejected because it splits the meaning of the same file — a configuration without the section
  would mean "no" on an upgraded host and nothing at all on a fresh one — and because it leaves every
  already-installed Client un-updatable. The cost is recorded under Consequences instead.

## Sources / Prior art

- [Elastic Agent upgrade documentation](https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md)
  — the update marker, the watcher, and the symlink flip; already the source of ADR-0010's
  `<component>-<version>-<hash>` directory scheme, and the model this decision follows in substance
  while dropping the separate process.
- [OpAMP Supervisor specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — checked as the behavioural oracle this project usually follows: it covers Collector executable
  updates and has no self-update section at all, so there is no upstream shape to match here.
- Elastic Agent and the OpenTelemetry `opampsupervisor`, read for the consent: neither ships an
  agent-updates-itself consent switch, because neither updates its own binary from the control
  plane at all. There is no upstream default to follow.
- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"normally only one top-level package, which implements the primary functionality of the Agent"*,
  and `PackageAvailable` as an offer to *"install a new package or initiate an upgrade or downgrade"*.
- [systemd-devel on self-restart deadlocks](https://lists.freedesktop.org/archives/systemd-devel/2015-February/027966.html)
  — synchronously waiting on a job ordered against your own job deadlocks, which is why clause 5
  does not issue the restart.
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
  failure actions. The cross-platform abstraction is not one here, which is why clause 5 has a
  per-platform table instead of a single sentence.
- The incident behind the default: a Windows Client reporting `capabilities` without
  `AcceptsPackages` (8) or `ReportsPackageStatuses` (16), with `package_assignments` set and
  `package_statuses` absent — an assigned package that could never be offered, and no diagnostic
  anywhere in the path.
- [ADR-0020](0020-installing-the-client-and-native-installers.md) — the questionnaire that
  asks for the consent.
- [ADR-0020](0020-installing-the-client-and-native-installers.md),
  [ADR-0020](0020-installing-the-client-and-native-installers.md) — the installers that carry the
  answer, and the precedent for a public MSI property an administrator can set.

## Consequences

- Positive: goal 11 closes, and the layout ADR-0010 built — versions, pointer, manifest hash,
  `heal_current` — is used for what it was designed for.
- Positive: the Client is visible in its own fleet. Its version, health, and connection show up like
  any other Agent, on every host, including one that supervises something.
- Positive: a fleet can patch its own agent. The Client stops being the one program on the host that
  an operator has to reach by hand.
- Positive: the answer is askable and visible — a checkbox at install time, a key in the file
  afterwards, and a flag for every scripted path.
- Positive: closing the Windows gaps in clause 5 fixes a **defect wider than self-update**. Without
  them a Windows Client is not restarted after *any* failure — not a crash, not a panic, not an error
  exit — because the `sc.exe` backend silently discards the restart policy and only logs a warning.
  ADR-0010 states the Client restarts on failure; the gaps closed are what make that true on Windows,
  and what make clause 5 a mechanism rather than a wish.
- Negative / trade-offs: **every Client is an extra Agent.** Fleet counts grow, the fleet view
  grows a row per Client, and any Selector written as "everything" means something wider than it
  would otherwise. This is the most disruptive part of the decision and it is not opt-in — the Agent
  exists whether or not the consent stands, because a Client invisible in its own fleet is the wrong
  default.
- **Negative / trade-offs: a host whose configuration has no `[self_update]` section accepts
  self-update offers.** That includes every host where the questionnaire's answer was once "no"
  without the section being written. It is a real widening applied without asking, and the only
  honest mitigation is that it is loud: the CHANGELOG names it, and the withdrawal is one line. An
  operator who wants the narrower position writes `enabled = false`.
- Negative / trade-offs: an operator who never chose either way is in the larger of the two
  positions. The narrowing to the package name is what bounds it, and the Server still only offers
  what an explicit rollout act assigned ([ADR-0030](0030-a-rollout-is-an-explicit-act.md)) — but a
  compromised Server reaches further than it would under an opt-in.
- Negative / trade-offs: a self-update is reported across a process boundary, so there is a window in
  which the Server has seen `Installing` and will not see anything further until the new version
  connects. A Client that never comes back is indistinguishable, from the Server, from one whose host
  went down — the marker makes the *host* recover, but the Server learns nothing until it does.
- Negative / trade-offs: the attempt bound in clause 6 trades a crash-looping new version against a
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
  mid-package-install, so two swaps do not overlap on one host. Whether the self-Agent should
  report a distinguishing attribute the fleet view surfaces, so an operator can filter Clients from
  the agents they manage without knowing the naming convention. And the silence that made a
  missing consent expensive: an Agent assigned a package it can never be offered — because it
  declares no capability, or because the Set holds no entry for its platform — is skipped without a
  word by `rollout_package` and shown as *assigned* in the fleet view. That deserves its own change.
