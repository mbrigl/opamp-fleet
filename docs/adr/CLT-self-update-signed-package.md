# CLT-self-update-signed-package: The Client updates itself from a signed package of its own Agent type

- **Status:** 🟢 accepted
- **Date:** 2026-10-11
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/update/ (mod.rs and installer.rs), the Client Agent in crates/fleet-agent/src/supervisor/, the `self_update` key of `[packages]` and its parsing in crates/fleet-agent/src/config.rs, the refusal of a `[self_update]` section, the self-update flags of `service install`, the questionnaire's handling of the consent in crates/fleet-agent/src/config_init.rs, the MSI `SELFUPDATE` property, and the startup notice on a missing `[packages] verification_key`

## Context

The [specification](../SPECIFICATION.md) puts security before convenience (Strategy *Security
before convenience*, Q-1 *Secure by default*). No package is installed without a valid signature,
the Client's own included. The Server updates an agent's software, the Collector's and the
Client's own (G-10). The Client replaces its own binary in place, survives the service restart and
rolls back on failure (G-11).

Other ADRs decide the machinery around a self-update:

- the versioned side-by-side layout with its `current` pointer, the per-version `manifest.toml`
  and the restart-on-failure registration
  ([CLT-installed-service-and-state §17](CLT-installed-service-and-state.md),
  [CLT-accounts-service-runs-under §6](CLT-accounts-service-runs-under.md));
- the verified download and the archive handling of package delivery
  ([PKG-signed-packages-and-delivery §51](PKG-signed-packages-and-delivery.md) to
  [PKG-signed-packages-and-delivery §57](PKG-signed-packages-and-delivery.md));
- the per-Agent offer
  ([SRV-deployments-and-pinned-rollouts §7](SRV-deployments-and-pinned-rollouts.md)).

Four problems are specific to replacing the program that does the replacing.

- **Something on the wire has to represent the Client.** A Supervisor Agent cannot carry the
  Client's package beside its own. The Baseline knows *"normally only one top-level package, which
  implements the primary functionality of the Agent"*, and that one is the Managed Process's
  binary.
- **The health gate cannot be where package delivery puts it.** The Supervisor that outlives a
  Managed Process judges it. Here the process that installs the package has to exit for the install
  to take effect, so nothing is left to watch or roll back.
- **The restart cannot be self-issued.** `systemctl restart` on a unit from inside it waits on a
  job ordered against its own job, and deadlocks. A helper child inherits the unit's cgroup and dies
  with it under `KillMode=control-group`. Escaping that needs `systemd-run --scope`, which has no
  launchd or SCM equivalent.
- **The consent has to be both present and narrow.** A Client the fleet cannot update has to be
  patched by hand on every host, the work fleet management exists to end. An agent that cannot be
  updated cannot be *fixed*. But replacing the binary that manages every other binary on the host
  is the largest grant a Server holds. An offer meant for another program must never be written
  over the Client.

Three properties of the consent follow from the last problem:

- **It is one decision.** A questionnaire that asked for the verification key and then whether the
  Server may update the Client would split it over two questions. The second answer has no effect
  without the first: a "yes" with no key updates nothing, and with a key a "yes" is what the
  default already says.
- **Only one package name can carry this Client:** the Agent type its Client Agent reports. A
  Server offers a package under the Agent type it is built for
  ([PKG-signed-packages-and-delivery §9](PKG-signed-packages-and-delivery.md)). Any other name
  points at another program's package or at none. A configurable name is therefore only a way to
  misconfigure the narrowing.
- **It belongs beside what it depends on.** The key that decides whether it takes effect sits in
  `[packages]`. A consent in a section of its own makes a reader join two sections to learn whether
  this Client updates itself.

Windows locks a running `.exe`. Side-by-side version directories make that moot on every platform,
because the new binary is never written over the old one. There is no upstream answer to copy: the
OpenTelemetry `opampsupervisor` updates the Collector and has no self-update section.

## Decision

We will have the Client present a Client Agent that takes only the package named after its own
Agent type, signed, by a consent that stands unless it is withdrawn, and update it by staging a new
version beside the running one, proving the new binary before committing to it, and letting the
service manager perform the restart.

1. **The Client always presents a Client Agent.**
   - The Client Agent exists *alongside* the Supervisor Agents, not only when there are none.
   - It has its own `instance_uid`, health and `service.version`. The version is the baked one
     ([OPS-cargo-decides-product-version §5](OPS-cargo-decides-product-version.md)).
   - Its Agent type is `supervisor`
     ([CLT-installed-service-and-state §4](CLT-installed-service-and-state.md)).

   A Client that supervises something is thus visible in its own fleet. Nobody has to ask which
   Client version a host runs.

2. **The consent is `[packages] self_update`, it stands by default, and the one package it admits
   is the Client's own Agent type.** The key sits beside `verification_key`, which decides whether
   it takes effect:

   ```toml
   [packages]
   self_update = true
   verification_key = "<hex-encoded Ed25519 public key>"
   ```

   - `self_update` defaults to `true`. `false` withdraws the consent.
   - An absent key is consent, and so is an absent `[packages]`.
   - **The package name is not configurable.** It is the Agent type compiled into the binary,
     `supervisor` (clause 1), the constant the Client Agent reports as `service.name`.
   - A Server therefore offers this Client's package under the name the Client reports. That name
     is the same in every variant build, whatever its product name. One published Package serves
     them all ([CLT-installed-service-and-state §3](CLT-installed-service-and-state.md)).
   - While the consent stands and `verification_key` is set, the Client Agent declares
     `AcceptsPackages` and `ReportsPackageStatuses`.
   - The Client Agent then **refuses any offered top-level package whose name is not its Agent
     type**. It reports `InstallFailed` with that reason and installs nothing.
   - With `self_update = false`, the Client Agent declares no package capability, and no offer can
     reach it. The Supervisor Agents still take theirs.
   - **With the consent standing but no verification key, the Client Agent declares no package
     capability either.** The Client says so at startup, naming the missing
     `[packages] verification_key`. The consent is kept and takes effect once the key is set.
   - The key alone thus decides whether a consenting Client updates itself.

   **A `[self_update]` section fails the load**, whatever it holds. The message names
   `[packages] self_update` as the key that replaces it. Ignoring the section would turn a written
   `enabled = false` into consent the moment a key is set. Honouring it would keep two spellings of
   one decision alive for good.

3. **Every install path can withdraw the consent, none asks for a name, and the answer is written
   into the configuration.**
   - `service install --no-self-update` withdraws it on the non-interactive `--endpoint` path
     ([SEC-bootstrap-certificates-and-enrolment §17](SEC-bootstrap-certificates-and-enrolment.md)).
   - **The interactive questionnaire asks nothing about the consent.** It writes the consent. The
     verification key it asks for just before decides whether the consent takes effect (clause 2).
   - The questionnaire accepts an empty key. The install then says that the Client installs no
     package, its own update included, until a key is set.
   - The MSI shows a checkbox, checked by default, on its endpoint dialog. The public property
     `SELFUPDATE` (default `1`) backs it.
   - `SELFUPDATE=0` or an empty value withdraws the consent. `msiexec /qn … SELFUPDATE=0` is how
     Intune or Group Policy declines.
   - Whatever path wrote it, the first configuration carries `[packages] self_update = true` or
     `= false`. The decision is thus visible and editable on the host.
   - No installer holds state of its own.

4. **An offer is checked before anything is staged.**
   - The offered version is Server-controlled and names a directory. It must parse as a version.
   - The directory it derives must be a direct child of `versions/`.
   - **The Client refuses an offer older than the running version.** The Ed25519 signature covers
     the artifact's bytes and carries no ordering. An old release therefore stays validly signed
     forever, and a compromised Server would use it to send a known-vulnerable build back.
   - An offer of the release already running is not a failure and installs nothing
     ([SRV-deployments-and-pinned-rollouts §18](SRV-deployments-and-pinned-rollouts.md)).
   - A Client that does not run from a versioned layout refuses the offer with a message naming
     `supervisor service install`. A `cargo run` build and a loose binary are such Clients.

5. **An install is a staged version, never an overwrite, of a signed artifact.**
   - The Client verifies the artifact like any other package. Its SHA-256 must equal the offered
     content hash.
   - **Its Ed25519 signature must verify against `[packages] verification_key`.**
   - An artifact offered without a signature, or whose signature does not verify, fails the install
     with `InstallFailed` naming the reason. Nothing is staged.
   - The Client unpacks the verified artifact as package delivery unpacks one
     ([PKG-signed-packages-and-delivery §53](PKG-signed-packages-and-delivery.md)), into
     `<layout root>/versions/supervisor-<MAJOR.MINOR.PATCH>-<hash>/`.
   - The Client makes the binary executable. `manifest.toml` records the version and the binary's
     SHA-256. This is the same layout `service install` produces.
   - The Client never touches the running binary.

6. **The new binary is proved before the pointer moves.**
   - The Client runs the staged binary as `supervisor self-check`. Only this program answers it,
     with `supervisor self-check ok version=<version>`.
   - The probe must exit successfully and print that line.
   - The line must report the offered release. The Client compares it without build metadata but
     with the pre-release, so it never accepts a `-dev` build as the release it heads for
     ([OPS-cargo-decides-product-version §10](OPS-cargo-decides-product-version.md)).
   - A binary that cannot exec is the one failure no mechanism after the restart can catch. Wrong
     architecture, a truncated artifact and a missing loader are such failures.
   - A program that runs but answers differently is not this Client.
   - A failed probe fails the install. The previous version stays current and running.

7. **A marker is written before the pointer moves.**
   - The marker is `update-marker.json` in the state directory. It lies outside `versions/` so
     that it survives the switch.
   - It records the previous version directory, the new one, the offered version, the offered
     package hash, an attempt counter and the install's trace
     ([CLT-what-an-agent-reports §21](CLT-what-an-agent-reports.md)).
   - The Client then repoints `current`.
   - A crash between the two leaves a marker naming a switch that did not happen. The next start
     resolves it.

8. **The restart is the service manager's, triggered by a deliberate exit.**
   - Once `current` points at the new directory, the Client reports `Installing` and shuts down
     gracefully. It stops the Managed Processes and sends `agent_disconnect`.
   - It then exits with the distinguished code **10**.
   - No unit manipulates itself, and nothing has to survive the unit stopping.
   - Each manager must be told to restart on a non-zero exit:

   | | What makes the restart happen |
   |---|---|
   | Linux (systemd) | `Restart=on-failure` |
   | macOS (launchd) | `KeepAlive { SuccessfulExit = false }` |
   | Windows (SCM) | a `Restart` recovery action, **and** `fFailureActionsOnNonCrashFailures` set, **and** the run's exit code reported in the final `SERVICE_STOPPED` status as a service-specific code |

   On Windows all three are required:
   - the `sc.exe` backend of `service-manager` registers no recovery actions;
   - the SCM ignores a clean non-zero stop unless the non-crash flag is set;
   - a status of `Win32(0)` is a clean stop, whatever happened.

   The registration that sets them is
   [CLT-installed-service-and-state §17](CLT-installed-service-and-state.md)'s. **The Client must
   never crash on purpose to restart.** A crash throws away the graceful shutdown and the
   `agent_disconnect`, and makes every self-update look like a fault.

9. **The new version commits itself, or the host rolls back.** Before anything else runs, a
   starting Client resolves the marker:
   - **No marker:** an ordinary start.
   - **A marker, and this process is not the version it names:** the update did not take effect,
     and the Client reports it failed. The Client judges by the version the binary reports *and*
     the directory it runs from. A pointer alone says what was pointed at, not what started.
   - **A marker, attempts above 3:** the Client repoints `current` at the recorded previous
     directory and exits with code 10. The manager brings the previous version back.
   - **Otherwise:** the Client counts the attempt, and the run proceeds on probation. **The first
     reply from the Server commits it.** The Client removes the marker and owes `Installed`. The
     bar is "it reached the Server", not "it survived a clock".

   A configuration the new version cannot read is a failed attempt like any other. The Client
   resolves the marker before the error ends the run. It takes the state directory from
   `--state-dir`, else from the file's own `state_dir`, else the default. One attempt is too few,
   because a loaded host loses a start to unrelated causes. Many attempts would leave a fleet
   crash-looping for minutes.

10. **The outcome is reported after the restart, by whichever version runs.**
    - `Installing` is the last thing the installing process says.
    - The next process reports the terminal status, through `update-outcome.json` in the state
      directory. It removes the file once it has reported it.
    - Either way, the Client echoes the offered `all_packages_hash` once the status is terminal.
      That stops the Server from offering it again.
    - Package delivery follows the same rule. It is the reason a refused or failed self-update does
      not become a loop.

**Out of scope:**

- rolling back on health after the update rather than on "did it come back";
- a Server-driven downgrade, which the Client refuses (clause 4) and the Server never offers
  ([SRV-deployments-and-pinned-rollouts §21](SRV-deployments-and-pinned-rollouts.md));
- a privileged updater helper for a low-privileged service account;
- closing the non-atomic Windows pointer switch, which needs a different Windows layout.

## Alternatives considered

- **A separate watcher process, as Elastic Agent has** (the source of the marker file). A watcher
  spawned from a systemd service dies with it under `KillMode=control-group`, and escaping the
  cgroup is Linux-only. "Prove before, count after" gets the same coverage from one process.
- **Overwrite the binary in place.** Windows locks a running `.exe`. The previous version on disk
  is what makes rollback a pointer move.
- **Leave self-update to the OS package manager.** A fleet that reaches its agents only through
  OpAMP then needs a second, per-platform channel to update the thing that speaks OpAMP.
- **Give the Client's package to a Supervisor Agent.** Its one top-level package is the Managed
  Process's binary, and a host's self-update would depend on it supervising something.
- **A dedicated package type or reserved name in the protocol.** The specification's Non-Goal
  *Forking or extending the protocol* rules it out. The name match is Client-side policy.
- **Roll back on health after the update.** It needs a definition of "degraded" that is not "it
  exited", a window to judge it over, and a guard against a fleet oscillating between two
  versions.
- **Self-update on the content hash alone.** The hash comes from the same Server that offers the
  package. It proves only that the download is what that Server meant. A compromised Server would
  install a binary of its choosing on every consenting host, the largest grant a Server holds. The
  signature binds the bytes to a key the operator keeps off the Server.
- **Consent with no name.** The first artifact meant for another program would be written over the
  Client, and that failure is not recoverable remotely.
- **A configurable package name, defaulting to the Agent type.** The only name that can
  legitimately carry this Client is its Agent type. Any other names another program's package or
  none, so the freedom can only misconfigure the narrowing.
- **The name from the product name.** Every variant build would need a Package of its own, where
  one serves them all. [CLT-installed-service-and-state §3](CLT-installed-service-and-state.md)
  keeps the product name off the Agent type for that reason.
- **The name from the running file's name.** Renaming or linking the binary would redirect the
  self-update to another package.
- **Ask for the consent in the questionnaire.** The question comes after the verification key, and
  its answer does nothing without one, so the operator answers one decision twice. An operator who
  must decline keeps `--no-self-update`, the MSI checkbox and the file.
- **The consent in a `[self_update]` section of its own.** It names one decision whose effect
  hangs on a key in another section. This was the Client's earlier spelling; clause 2 refuses it.
- **The questionnaire writes `self_update = false` when the key is left empty.** A key added later
  would then need a second edit before it takes effect, and the file would hold two answers that
  can disagree.
- **The key alone, with no withdrawal.** A host whose Client is updated through another channel
  (the OS package manager, a golden image) needs a way to decline the self-update that does not
  also cost it every other package.
- **Ignore a leftover `[self_update]` section with a notice.** A host that withdrew the consent
  with `enabled = false` would update itself once a key is set, without anyone having said yes.
- **Honour a leftover `enabled = false` and ignore the rest.** No host would stop loading, but the
  old spelling would stay a second way to say the same thing, and the loader would carry the
  migration for good.

## Sources / Prior art

- [Elastic Agent upgrade
  documentation](https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md)
  — the update marker, the watcher and the symlink flip.
- [OpAMP Supervisor
  specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — Collector executable updates, no self-update section, no consent switch.
- [OpAMP specification
  `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — one top-level package per Agent; `PackageAvailable` as an upgrade or downgrade offer.
- [systemd-devel on self-restart
  deadlocks](https://lists.freedesktop.org/archives/systemd-devel/2015-February/027966.html).
- [Microsoft: replacing an in-use
  file](https://learn.microsoft.com/en-us/sysinternals/downloads/pendmoves).
-
  [`SERVICE_FAILURE_ACTIONS_FLAG`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_failure_actions_flag)
  — failure actions on a non-zero `SERVICE_STOPPED` only with `fFailureActionsOnNonCrashFailures`,
  false by default; and
  [Elastic Agent shipping without it](https://discuss.elastic.co/t/elastic-agent-service-installed-on-windows-without-noncrash-failure-flag/374043).
- [`launchd.plist(5)`](https://www.manpagez.com/man/5/launchd.plist/) — `KeepAlive` with
  `SuccessfulExit = false`.
- [`windows-service`
  crate](https://docs.rs/windows-service/0.8.0/windows_service/service/struct.Service.html)
  — `update_failure_actions`, `set_failure_actions_on_non_crash_failures`.
- `service-manager` 0.11, `src/sc.rs`, read directly: the Windows `install` only logs a warning
  about the restart policy and configures no failure actions.

## Consequences

- Positive: the fleet patches its own agent, and the Client is visible in its own fleet (version,
  health, connection) on every host. The verification key answers the consent once, and the answer
  stays visible.
- Positive: the narrowing cannot be misconfigured. The package a Server offers this Client is the
  Agent type the fleet view shows for it.
- Positive: a compromised Server cannot install a binary of its own making over the Client. It can
  offer only what the holder of the signing key has signed.
- Negative / trade-offs: an operator who wants to decline the self-update interactively edits the
  file after the install. The questionnaire offers no "no".
- Negative / trade-offs: **a configuration that carries a `[self_update]` section fails to load.**
  A host that receives a release by self-update with such a file cannot read it. It counts failed
  attempts and rolls back to the previous version (clause 9), and the Server sees the update fail.
  Each such file needs the section replaced by `[packages] self_update` by hand before the release
  reaches that host. A host upgraded through its OS package manager does not start until then.
- Negative / trade-offs: **a reduced build cannot be kept apart by package name.** A build that
  leaves kinds out carries the same Agent type as the full one. A Package is identified by type and
  version alone, so the two cannot both be published at one version. A host running the reduced
  build takes the full one at its next self-update. Such hosts withdraw the consent
  (`self_update = false`) and are updated through their own channel.
- Negative / trade-offs: every Client is an extra Agent, not opt-in. Fleet counts and anything
  aimed at "every Agent" grow by one row per host.
- Negative / trade-offs: an operator who never chose is in the larger position. The package name,
  the signature and the explicit act that releases a package
  ([SRV-deployments-and-pinned-rollouts §9](SRV-deployments-and-pinned-rollouts.md)) bound it. But
  a compromised Server reaches further than with the consent off: it can roll a consenting host
  forward to any signed release.
- Negative / trade-offs: a host with the consent but no verification key does not update itself
  until the key is set. The startup notice is the only sign of it on the host. The Server sees a
  Client Agent that takes no packages.
- Negative / trade-offs: between `Installing` and the new version connecting, a Client that never
  comes back looks like a host that went down. The marker recovers the host; the Server learns
  nothing until it does. The attempt bound trades a crash loop against a premature rollback of a
  version slow to start.
- Negative / trade-offs: **the pointer switch is atomic on Unix and not on Windows.** A junction
  cannot be renamed over `current`; the Client removes and recreates it. The still-running process
  recovers from a failed recreate: it points the junction back and fails the install. A process
  killed inside that window of milliseconds leaves a service with no program to start. Accepted
  and named.
- Follow-ups: deferring a self-update while a Managed Process is mid-install, so two swaps never
  overlap on one host.

## Enforcement

- [`tests/self_update_e2e.rs`](../../crates/fleet-agent/tests/self_update_e2e.rs), with the real
  Server and Client binaries:
  - `the_client_installs_a_version_of_itself_and_reports_it_installed`;
  - `managed_processes_stop_cleanly_on_the_self_update_restart`;
  - `a_set_at_the_running_version_reaches_nobody`.
- [`update/installer.rs`](../../crates/fleet-agent/src/update/installer.rs):
  - `install_refuses_a_downgrade`;
  - `install_refuses_a_version_that_would_escape_the_layout`;
  - the `the_probe_refuses_*` tests and `the_probe_ignores_the_commit_a_build_came_from`;
  - `taking_over_needs_the_binary_to_be_the_version_the_marker_names`;
  - `the_old_version_finding_a_marker_reports_the_update_as_failed`;
  - `committing_replaces_the_marker_with_an_installed_outcome`.
- [`service/runtime.rs`](../../crates/fleet-agent/src/service/runtime.rs):
  `an_unreadable_configuration_resolves_the_update_in_flight` (clause 9).
- The consent (clauses 2, 3):
  - `self_update_consent_stands_by_default_and_is_narrowed_to_the_agent_type` and
    `a_self_update_section_fails_the_load_naming_the_packages_key`
    ([`config.rs`](../../crates/fleet-agent/src/config.rs));
  - `the_self_agent_refuses_a_package_not_named_after_its_agent_type`
    ([`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs));
  - `install_carries_the_self_update_answer_without_a_terminal`
    ([`cli.rs`](../../crates/fleet-agent/src/cli.rs));
  - `declined_sections_are_absent_rather_than_empty_and_the_consent_is_always_written`
    ([`config_init.rs`](../../crates/fleet-agent/src/config_init.rs));
  - `the_self_update_answer_rides_both_register_actions` and
    `the_withdrawal_condition_reads_both_spellings_of_off`
    ([`tests/msi_exe_command.rs`](../../crates/fleet-agent/tests/msi_exe_command.rs)).
- The signature:
  - clause 2: `without_a_verification_key_no_agent_takes_packages`
    ([`supervisor/mod.rs`](../../crates/fleet-agent/src/supervisor/mod.rs)). The consent stands,
    and the Client Agent declares no package capability.
  - clause 5: the self-update downloads and verifies through the path every package takes.
    `signature_policy_is_enforced` ([`packages.rs`](../../crates/fleet-agent/src/packages.rs)) and
    `an_unsigned_offer_or_an_unkeyed_client_fetches_nothing`
    ([`tests/packages_download.rs`](../../crates/fleet-agent/tests/packages_download.rs)) test it.
    End to end, this test covers it:
    `a_self_update_whose_signature_does_not_verify_is_refused_and_the_client_keeps_running`
    ([`tests/self_update_e2e.rs`](../../crates/fleet-agent/tests/self_update_e2e.rs)).
- The real service manager's restart (clause 8):
  `the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop` in
  [`tests/service_smoke.rs`](../../crates/fleet-agent/tests/service_smoke.rs). It runs weekly on
  Windows via [`service-smoke.yml`](../../.github/workflows/service-smoke.yml).

**Not mechanically decidable:** these rest on a timing window, a log line or a terminal no test
reaches.

- No test can hit the window of the non-atomic Windows pointer switch on purpose.
- The Client names the missing `[packages] verification_key` at startup (clause 2). That is a log
  line in [`service/runtime.rs`](../../crates/fleet-agent/src/service/runtime.rs) that no test
  reads.
- The questionnaire asks nothing about the consent (clause 3). That is a review question on `ask`
  in [`config_init.rs`](../../crates/fleet-agent/src/config_init.rs). `ask` reads a terminal that
  `cargo test` does not have.
