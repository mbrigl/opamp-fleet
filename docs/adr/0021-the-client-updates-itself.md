# ADR-0021: The Client updates itself — as its own Agent, by a consent that stands, through a staged version and a restart it does not issue

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl
- **Applies to:** crates/client/src/selfupdate.rs, the Client's own Agent in crates/client/src/supervisor/, the `[self_update]` section, the self-update flags of `service install`, the MSI `SELFUPDATE` property

## Context

The specification asks the Server to update *"an agent's binary — the Collector's, **and the
Client's own**"*, and the Client to *"replace its own binary in place — a self-update that survives
the service restart and is rolled back on failure"*. Most of the machinery exists: the versioned
side-by-side layout with its `current` pointer, per-version `manifest.toml` and restart-on-failure
registration ([ADR-0014](0014-the-client-as-an-installed-service.md)), the verified download and the
archive handling of package delivery ([ADR-0019](0019-package-delivery-on-the-agent.md)), and the
per-Agent offer ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md)). Four problems are specific
to replacing the program that does the replacing.

- **Something on the wire has to represent the Client.** A Supervisor Agent cannot carry the
  Client's package beside its own: the Baseline knows *"normally only one top-level package, which
  implements the primary functionality of the Agent"*, and that one is the Managed Process's binary.
- **The health gate cannot be where package delivery puts it.** A Managed Process is judged by the
  Supervisor that outlives it. Here the process that installs the package is the one that has to
  exit for the install to take effect; nothing is left to watch or roll back.
- **The restart cannot be self-issued.** `systemctl restart` on your own unit from inside it waits on
  a job ordered against the job you are part of and deadlocks; a helper child inherits the unit's
  cgroup and dies with it under `KillMode=control-group`; escaping that needs `systemd-run --scope`,
  which has no launchd or SCM equivalent.
- **The consent has to be both present and narrow.** A Client the fleet cannot update has to be
  patched by hand on every host, which is the work fleet management exists to end — and an
  un-updatable agent cannot be *fixed*. But replacing the binary that manages every other binary on
  the host is the largest grant a Server holds, and an offer meant for another program must never be
  written over the Client.

Windows locks a running `.exe`; side-by-side version directories make that moot on every platform,
because the new binary is never written over the old one. There is no upstream answer to copy: the
OpenTelemetry `opampsupervisor` updates the Collector and has no self-update section.

## Decision

We will make the Client its own Agent, let it take one named package by a consent that stands
unless it is withdrawn, and update it by staging a new version beside the running one, proving the
new binary before committing to it, and letting the service manager perform the restart.

1. **The Client is always its own Agent.** It exists *alongside* the Supervisor Agents, not only
   when there are none, with its own `instance_uid`, health and `service.version` (the baked version,
   [ADR-0013](0013-versions.md)) and the Agent type `supervisor`
   ([ADR-0023](0023-releases-installers-and-the-name-supervisor.md) clause 1). A Client that
   supervises something is visible in its own fleet, and nobody has to ask which Client version a
   host runs.

2. **The consent stands by default and is narrowed to one package name.** `[self_update]` has two
   keys, both optional, and unknown keys are refused:

   | key | default | meaning |
   |---|---|---|
   | `enabled` | `true` | `false` withdraws the consent |
   | `package` | the Client's own Agent type, `supervisor` | the only package name this Agent installs |

   An absent section is consent. While the consent stands, the Client's own Agent declares
   `AcceptsPackages` and `ReportsPackageStatuses` and **refuses any offered top-level package whose
   name is not the configured one**, reporting `InstallFailed` with that reason and installing
   nothing. With `enabled = false` it declares no package capability, and no offer can reach it.
   The default is not a wildcard: the name a Server offers a package under is the Agent type it is
   built for ([ADR-0030](0030-packages-and-deployments.md)), so the default is the one package that
   could legitimately be this Client. **An empty `package` with the consent standing fails at load**,
   naming the key: the name is the whole of the narrowing, and an empty one would widen it to
   whatever the Server offers next.

3. **Every install path can answer the consent, and the answer is written into the configuration.**
   `service install --no-self-update` withdraws it and `--self-update-package <NAME>` names another
   package (the two conflict); both ride the non-interactive `--endpoint` path
   ([ADR-0023](0023-releases-installers-and-the-name-supervisor.md) clause 18). The interactive
   questionnaire asks it last and defaults to yes. The MSI shows a checked-by-default checkbox on its
   endpoint dialog backed by the public property `SELFUPDATE` (default `1`); `SELFUPDATE=0` or an
   empty value withdraws it, so `msiexec /qn … SELFUPDATE=0` is how Intune or Group Policy declines.
   Whatever was answered, the first configuration carries a `[self_update]` section — `package = …`
   or `enabled = false` — so the decision is visible and editable on the host; no installer holds
   state of its own.

4. **An offer is checked before anything is staged.** The offered version is Server-controlled and
   names a directory, so it must parse as a version, and the directory it derives must be a direct
   child of `versions/`. **An offer older than the running version is refused**: the Ed25519
   signature covers the artifact's bytes and carries no ordering, so an old release stays validly
   signed forever and is how a compromised Server would push a known-vulnerable build back. An offer
   of the release already running is not a failure and installs nothing
   ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clause 15). A Client that does not run
   from a versioned layout (a `cargo run` build, a loose binary) refuses with a message naming
   `supervisor service install`.

5. **An install is a staged version, never an overwrite.** The verified artifact is unpacked as
   ([ADR-0019](0019-package-delivery-on-the-agent.md)) into
   `<layout root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/`, the binary is made executable and
   `manifest.toml` records the version and the binary's SHA-256 — the same layout
   `service install` produces. The running binary is never touched.

6. **The new binary is proved before the pointer moves.** The staged binary is run as
   `supervisor self-check`, which only this program answers, with
   `supervisor self-check ok version=<version>`. It must exit successfully, print that line, and
   report the offered release — compared without build metadata but with the pre-release, so a
   `-dev` build is never accepted as the release it heads for ([ADR-0013](0013-versions.md)). A
   binary that cannot exec — wrong architecture, truncated artifact, missing loader — is the one
   failure no post-restart mechanism can catch, and a program that runs but answers differently is
   not this Client. A failed probe fails the install with the previous version still current and
   running.

7. **A marker is written before the pointer moves.** `update-marker.json` in the state directory —
   outside `versions/`, so it survives the switch — records the previous version directory, the new
   one, the offered version, the offered package hash, an attempt counter and the install's trace
   ([ADR-0025](0025-own-telemetry.md)). Then `current` is repointed. A crash between the two leaves a
   marker naming a switch that did not happen, which the next start resolves.

8. **The restart is the service manager's, triggered by a deliberate exit.** Once `current` points at
   the new directory, the Client reports `Installing`, shuts down gracefully — Managed Processes
   stopped, `agent_disconnect` sent — and exits with the distinguished code **10**. No unit
   manipulates itself and nothing has to survive the unit stopping. "Restart on a non-zero exit" is
   what each manager has to be told:

   | | What makes the restart happen |
   |---|---|
   | Linux (systemd) | `Restart=on-failure` |
   | macOS (launchd) | `KeepAlive { SuccessfulExit = false }` |
   | Windows (SCM) | a `Restart` recovery action, **and** `fFailureActionsOnNonCrashFailures` set, **and** the run's exit code reported in the final `SERVICE_STOPPED` status as a service-specific code |

   On Windows all three are required: the `sc.exe` backend of `service-manager` registers no recovery
   actions, the SCM ignores a clean non-zero stop unless the non-crash flag is set, and a status of
   `Win32(0)` is a clean stop whatever happened. The registration that sets them is
   [ADR-0014](0014-the-client-as-an-installed-service.md)'s. **Crashing on purpose is never the
   mechanism**: it throws away the graceful shutdown and the `agent_disconnect`, and makes every
   self-update look like a fault.

9. **The new version commits itself, or the host rolls back.** Before anything else runs, a starting
   Client resolves the marker:
   - **no marker** — an ordinary start;
   - **a marker, and this process is not the version it names** — judged by the version the binary
     reports *and* the directory it runs from, since a pointer alone says what was pointed at, not
     what started — the update did not take effect and is reported failed;
   - **a marker, attempts above 3** — `current` is repointed at the recorded previous directory and
     the process exits with code 10, so the manager brings the previous version back;
   - **otherwise** — the attempt is counted and the run proceeds on probation. **The first reply from
     the Server commits it**: the marker is removed and `Installed` is owed. The bar is "it reached
     the Server", not "it survived a clock".

   A configuration the new version cannot read is a failed attempt like any other: the marker is
   resolved before the error ends the run, with the state directory taken from `--state-dir`, else
   from the file's own `state_dir`, else the default. One attempt is too few (a loaded host loses a
   start to unrelated causes); many would leave a fleet crash-looping for minutes.

10. **The outcome is reported after the restart, by whichever version runs.** `Installing` is the
    last thing the installing process says. The terminal status comes from the next process, through
    `update-outcome.json` in the state directory, and is removed once reported. Either way the
    offered `all_packages_hash` is echoed once the status is terminal, which stops the Server
    re-offering — the same rule package delivery follows, and the reason a refused or failed
    self-update does not become a loop.

**Out of scope:** rolling back on post-update health rather than on "did it come back"; a
Server-driven downgrade, which the Client refuses (clause 4) and the Server never offers
([ADR-0027](0027-rollout-and-what-reaches-an-agent.md)); a privileged updater helper for a
low-privileged service account; closing the non-atomic Windows pointer switch, which would need a
different Windows layout.

## Alternatives considered

- **A separate watcher process, as Elastic Agent does** (the source of the marker file). A watcher
  spawned from a systemd service dies with it under `KillMode=control-group`, and escaping the cgroup
  is Linux-only. "Prove before, count after" gets the same coverage from one process.
- **Overwrite the binary in place.** Windows locks a running `.exe`, and the previous version on
  disk is what makes rollback a pointer move.
- **Leave self-update to the OS package manager.** A fleet that reaches its agents only through
  OpAMP should not need a second, per-platform channel to update the thing that speaks OpAMP.
- **Give the Client's package to a Supervisor Agent.** Its one top-level package is the Managed
  Process's binary, and a host's self-update would depend on it supervising something.
- **A dedicated package type or reserved name in the protocol.** Forbidden by the specification's
  non-goal *"Forking or extending the protocol"*; the name match is Client-side policy.
- **Roll back on post-update health.** Needs a definition of "degraded" that is not "it exited", a
  window to judge it over, and a guard against a fleet oscillating between two versions.
- **Consent with no name.** The first artifact meant for another program would be written over the
  Client, and that failure is not recoverable remotely.
- **Consent off unless explicitly given**, or off in the loader with the installers writing the
  section. The first leaves every host whose install path did not ask un-updatable and silent about
  it; the second makes one file mean "no" on one host and "yes" on another.
- **A third state, "ask the Server whether it has one for us".** No such negotiation exists in the
  Baseline; inventing one would extend the protocol for a configuration question.

## Sources / Prior art

- [Elastic Agent upgrade documentation](https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md)
  — the update marker, the watcher and the symlink flip.
- [OpAMP Supervisor specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — Collector executable updates, no self-update section, no consent switch.
- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — one top-level package per Agent; `PackageAvailable` as an upgrade or downgrade offer.
- [systemd-devel on self-restart deadlocks](https://lists.freedesktop.org/archives/systemd-devel/2015-February/027966.html).
- [Microsoft: replacing an in-use file](https://learn.microsoft.com/en-us/sysinternals/downloads/pendmoves).
- [`SERVICE_FAILURE_ACTIONS_FLAG`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_failure_actions_flag)
  — failure actions on a non-zero `SERVICE_STOPPED` only with `fFailureActionsOnNonCrashFailures`,
  false by default; and
  [Elastic Agent shipping without it](https://discuss.elastic.co/t/elastic-agent-service-installed-on-windows-without-noncrash-failure-flag/374043).
- [`launchd.plist(5)`](https://www.manpagez.com/man/5/launchd.plist/) — `KeepAlive` with
  `SuccessfulExit = false`.
- [`windows-service` crate](https://docs.rs/windows-service/0.8.0/windows_service/service/struct.Service.html)
  — `update_failure_actions`, `set_failure_actions_on_non_crash_failures`.
- `service-manager` 0.11, `src/sc.rs`, read directly: the Windows `install` only logs a warning
  about the restart policy and configures no failure actions.

## Consequences

- Positive: the fleet patches its own agent, and the Client is visible in its own fleet — version,
  health, connection — on every host. The consent is askable at install time and visible after.
- Negative / trade-offs: every Client is an extra Agent, not opt-in; fleet counts and anything aimed
  at "every Agent" grow by one row per host.
- Negative / trade-offs: an operator who never chose is in the larger position. The package name
  and the explicit act that releases a package
  ([ADR-0027](0027-rollout-and-what-reaches-an-agent.md)) bound it, but a compromised Server reaches
  further than with the consent off.
- Negative / trade-offs: between `Installing` and the new version connecting, a Client that never
  comes back looks like a host that went down; the marker recovers the host, the Server learns
  nothing until it does. The attempt bound trades a crash loop against a premature rollback of a
  version slow to start.
- Negative / trade-offs: **the pointer switch is atomic on Unix and not on Windows.** A junction
  cannot be renamed over `current`; it is removed and recreated. A failed recreate is recoverable by
  the still-running process, which points it back and fails the install; a process killed inside
  that milliseconds-wide window leaves a service with no program to start. Accepted and named.
- Follow-ups: deferring a self-update while a Managed Process is mid-install, so two swaps never
  overlap on one host.

## Enforcement

- [`tests/self_update_e2e.rs`](../../crates/client/tests/self_update_e2e.rs), with the real Server
  and Client binaries: `the_client_installs_a_version_of_itself_and_reports_it_installed`,
  `managed_processes_stop_cleanly_on_the_self_update_restart`,
  `a_set_at_the_running_version_reaches_nobody`,
  `a_package_under_another_name_is_refused_and_the_client_keeps_running`.
- [`selfupdate.rs`](../../crates/client/src/selfupdate.rs): `install_refuses_a_downgrade`,
  `install_refuses_a_version_that_would_escape_the_layout`, the `the_probe_refuses_*` tests,
  `the_probe_ignores_the_commit_a_build_came_from`,
  `taking_over_needs_the_binary_to_be_the_version_the_marker_names`,
  `the_old_version_finding_a_marker_reports_the_update_as_failed`,
  `committing_replaces_the_marker_with_an_installed_outcome`; and
  `an_unreadable_configuration_resolves_the_update_in_flight` in
  [`service/runtime.rs`](../../crates/client/src/service/runtime.rs).
- The consent: `self_update_consent_stands_by_default_and_is_narrowed_to_a_package_name` and
  `an_empty_self_update_package_is_refused_while_the_consent_stands`
  ([`config.rs`](../../crates/client/src/config.rs)),
  `the_self_agent_refuses_a_package_it_was_not_configured_to_take`
  ([`supervisor/agent.rs`](../../crates/client/src/supervisor/agent.rs)),
  `install_carries_the_self_update_answer_without_a_terminal`
  ([`cli.rs`](../../crates/client/src/cli.rs)),
  `declined_sections_are_absent_rather_than_empty_and_the_consent_is_always_written`
  ([`config_init.rs`](../../crates/client/src/config_init.rs)),
  `the_self_update_answer_rides_both_register_actions` and
  `the_withdrawal_condition_reads_both_spellings_of_off`
  ([`tests/msi_exe_command.rs`](../../crates/client/tests/msi_exe_command.rs)).
- The real service manager's restart: `the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop`
  in [`tests/service_smoke.rs`](../../crates/client/tests/service_smoke.rs), weekly on Windows via
  [`service-smoke.yml`](../../.github/workflows/service-smoke.yml).

**Not mechanically decidable:** the non-atomic Windows pointer switch is a window no test can hit
on purpose.
