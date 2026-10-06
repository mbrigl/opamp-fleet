# ADR-0010: Supervisor Mode is a hexagonal core with compiled-in kinds, and a kind is the authority on its own agent

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/supervisor/ (core, ports, process runner, endpoint, every kind), the `[[supervisor]]` and `[supervisors]` sections of supervisor.toml, docs/artifacts/

## Context

The Supervisors that [ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md) binds are the reason the
client side exists: many Managed Processes per Client, each appearing to the Server as its own
Agent, multiplexed by `instance_uid`. A Managed Process takes one of three integration paths
([specification vocabulary](../SPECIFICATION.md)): a Collector carrying the `opampextension`, which
reports its own description, health and effective configuration to the Supervisor Endpoint; a
Collector without it, observed from the outside; and a Foreign Agent under a Custom Supervisor,
whose lifecycle, configuration and health a Plugin translates into OpAMP.

The specification asks for a hexagonal core: the supervision domain written against two Ports, the
Server-facing side speaking OpAMP and the Managed-Process side (lifecycle, configuration, health),
with Plugins as adapters. [ADR-0025](0025-five-crates-a-publishable-communication-layer-and-toml-configuration-axum-without-its-websocket.md) keeps such seams as
modules until a need makes them crates and requires a typo in the hand-edited TOML to fail loudly
at startup. serde cannot combine `#[serde(flatten)]` with `deny_unknown_fields`
(serde-rs/serde#1547), so a block whose kind-specific keys sit beside the common ones cannot be one
strict struct.

The reference `opampsupervisor` runs a local OpAMP server on
`ws://127.0.0.1:{{port}}/v1/opamp` (WebSocket only), passes configuration to the Collector via
`--config`, restarts it on a configuration change, stops it with SIGTERM then a kill after a
timeout, and watchdog-restarts it with exponential backoff. It configures its agent generically —
an absolute `agent.executable` plus `args` — because it supervises a binary somebody else
installed. This Client is in the opposite position: it installs every program it supervises, into
a directory it owns, from an artifact this project packs
([ADR-0032](0032-a-host-can-keep-its-supervisor-set-from-the-server.md)). It is the one end that can know an
agent's layout, and `opamp-package-fetch` already carries per-agent knowledge (service name,
release source, default Configuration names). Writing that knowledge into every host's TOML by
hand — two platform-specific blocks for the GLPI Agent, nine layout keys for Icinga 2 — is a
transcription that goes stale when an artifact moves.

Agents differ in how they are installed, configured, reloaded and removed. Some re-read their
configuration on a signal and lose buffered state when restarted; systemd treats reload as
first-class beside restart (`ExecReload`, `reload-or-restart`). OpAMP itself only ever drives
configuration and packages, and its only process command is restart.

## Decision

We will implement Supervisor Mode as a hexagonal supervision core in the Client crate whose
Managed-Process Port is one closed, channel-based lifecycle vocabulary served by compiled-in kinds,
each Supervisor its own Agent over the Client's one upstream connection with a WebSocket-only
Supervisor Endpoint, and we will make each kind the authority on its own agent, so that a
`[[supervisor]]` key exists only where its value is a decision.

1. **Two Ports, as modules.** The Managed-Process Port is a message pair — `ProcessCommand` in,
   `ProcessEvent` out — plus a `Plugin` factory trait that validates a block's settings
   (`check`, side-effect free) and starts the adapter task (`start`). Channels keep the trait
   object-safe without `async-trait`, make every adapter a plain tokio task, and keep the core
   free of process handles. The Server-facing Port is the engine seam the transports consume —
   build reports, handle a `ServerToAgent`, produce disconnects — over *n* Agents.

2. **A compiled-in registry selected by `type`.** The shipped kinds are `collector` (the
   Collector Supervisor), `command` (the Custom Supervisor for any Foreign Agent), and the wrapped
   kinds `icinga2`, `glpi` and `telegraf`. A new process kind is one module and one registry line
   (`registry()` in `supervisor/mod.rs`); the core stays untouched. An unknown `type` fails startup
   naming the known ones. Plugins are not loaded dynamically.

3. **A block is parsed in two stages.** The core takes the common keys — `type`, `name`,
   `service_name` ([ADR-0012](0012-what-an-agent-reports-about-itself.md)), `endpoint_port`,
   `program_path` ([ADR-0018](0018-signed-package-delivery-from-allowed-sources.md)), `stop_timeout_secs`,
   `apply_grace_secs`, `retain_previous_secs` — and the program key the kind names (`binary` for
   `collector`, `command` for `command`), which [ADR-0032](0032-a-host-can-keep-its-supervisor-set-from-the-server.md)
   resolves. Everything else is the kind's, parsed with `deny_unknown_fields`. `name` follows the
   instance-name grammar of [ADR-0028](0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md), because it is
   a directory name, and is unique across blocks.

4. **Each Supervisor is one Agent; the pool is one connection.** Every Supervisor has its own
   persisted `instance_uid`, `sequence_num` and capability set, and all of them ride the Client's
   single upstream connection, disambiguated by `instance_uid` alone — the n-over-m model of
   [ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md) with m fixed at one. The Client's own Agent
   ([ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)) is always present beside them, with or without
   any `[[supervisor]]` block. The Server needs no Supervisor-specific code.

5. **The Supervisor Endpoint is WebSocket-only on loopback.** Bound for every Supervisor at start
   to `127.0.0.1:<endpoint_port>` (`0`, the default, is an ephemeral port), so a taken port fails
   the start rather than later; accepted with `tokio-tungstenite`, no HTTP framework in the Client.
   It declares `AcceptsStatus` and `AcceptsEffectiveConfig`, enforces the message size limit, and
   folds **content, not identity**: a connecting process's description, health, effective
   configuration and available components become events of the owning Agent, whose
   `instance_uid` stays the Supervisor's.

6. **Process management follows the reference supervisor.** Spawn via `tokio::process`;
   watchdog-restart an unexpectedly exited process with exponential backoff; stop gracefully with
   SIGTERM → wait up to the stop timeout → kill on Unix, `Child::kill` on Windows. On Client
   shutdown the Managed Processes stop first, then each Agent's `agent_disconnect` goes out, within
   the service managers' stop budgets ([ADR-0028](0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)).
   The Collector Supervisor passes each written configuration entry as its own `--config` (never a
   supplementary entry, [ADR-0011](0011-configurations-and-the-rest-api.md)), appends its extra
   `args`, does not start before a configuration exists, and manipulates no YAML.

7. **Configuration status is health-gated.** `RemoteConfigStatus` is `APPLYING` on receipt,
   `APPLIED` only once the Managed Process has been (re)started on the new files and survived the
   apply grace, and `FAILED` with the error otherwise. A process that exits within the grace fails
   the apply and stays supervised.

8. **The lifecycle vocabulary is closed and every operation runs in the kind's adapter.**
   `ProcessCommand` is `ApplyConfig`, `ApplyPackage`, `Restart` (the Server's restart command:
   respawn on the current files, no configuration acknowledgement), `Shutdown` and `Uninstall`;
   `ProcessEvent` carries description, pid, health, effective configuration, available components,
   and the outcomes `ConfigApplied`, `PackageApplied` and `Uninstalled`. The shared `Runner`
   implements the whole vocabulary — spawn and watchdog, bounded stop, restart as the
   configuration apply, swap-and-gate as the package install, stop-only uninstall — and a kind
   overrides only the steps its agent does differently. A kind that overrides nothing is complete.

9. **Reload is an apply strategy, not a command.** `ApplyConfig` is the only configuration
   operation; the adapter applies it by restart (the default) or by a reload its kind knows, with
   systemd's `reload-or-restart` semantics: a reload that cannot be delivered, or a process that
   dies on it, falls back to a restart on the new files. The health gate of clause 7 applies to
   both paths. The mechanism is the kind's alone — `icinga2` and `telegraf` reload on their own
   signal, `collector`, `command` and `glpi` restart — and no block declares one: a signal written
   for a program the kind knows nothing about is the one value whose error is invisible.

10. **Install and update run behind the Plugin, in one contract.** `ApplyPackage` hands the adapter
    a verified artifact and expects a health-gated outcome with rollback on failure
    ([ADR-0018](0018-signed-package-delivery-from-allowed-sources.md)). The swap on `InstallTarget` (one file or a
    tree) is the shared default; a kind whose installation is not a file swap implements the step
    itself inside the same contract. Resolving the program and declaring `AcceptsPackages` stay in
    the core ([ADR-0032](0032-a-host-can-keep-its-supervisor-set-from-the-server.md)), because a capability
    the Server acts on must derive from what the core reads, never from per-kind behaviour.

11. **Uninstall undoes what installing did, and the core purges.** A Supervisor removed from the
    set receives `Uninstall` before its directory is purged
    ([ADR-0032](0032-a-host-can-keep-its-supervisor-set-from-the-server.md)): the adapter stops its process,
    undoes whatever its installs left outside the Supervisor's directory, answers `Uninstalled`
    and exits. The generic uninstall is exactly the bounded graceful stop. An `Err` outcome is
    logged with what could not be undone; it does not hold up the removal. A Supervisor that is
    only changed is stopped, never uninstalled.

12. **The core persists a configuration, the adapter delivers it.** Receiving, validating,
    persisting and status-reporting a remote configuration are OpAMP mechanics, identical for every
    kind, and stay in the core. Everything between the written files and the running process —
    pointing it at them, merging, restart or reload, applying through an API — is the kind's.

13. **Derivable means derived, and decided elsewhere means not written here.** A value this Client
    can compute from the kind, the platform and its own layout is not a block key, and neither is a
    value the fleet is the better place to set. A block carrying such a key fails at startup with a
    message naming the key and what supplies the value — never a silent override. This covers a
    wrapped kind's program key, `program_path`, `service_name`, `endpoint_port` and the three timing
    keys; `command`'s `working_dir` and `reload_signal`; and `[supervisor.attributes]`, since a
    Server label tags one Agent among several ([ADR-0013](0013-the-fleet-record.md)) while the
    Client-wide `[attributes]` describe the host. A configuration a freshly self-updated version
    refuses is that update's failed attempt: a run resolves the update in flight even when the file
    does not load, so the host returns to the version that could read it
    ([ADR-0020](0020-the-client-updates-itself-from-a-signed-package.md)).

14. **A kind states what it knows, and the spawn applies it.** `Plugin::defaults()` returns
    `KindDefaults`, compile-time constants resolved per platform: the program's file name, its
    `program_path` inside a tree, the `service_name` it presents, its timing corrections (clause
    17), and whether a block may pin `endpoint_port`. Beyond that:
    - **Working directory:** a Managed Process starts in the directory its program lives in —
      `program/` for a single file, the tree root for a tree — unless a kind names another; no
      block sets it. Every path the Client hands a process is absolute.
    - **Directories the agent writes into:** a kind lists them in the process specification
      (`ensure_dirs`), and they are created owner-only **before every spawn**, so a directory
      removed under a running fleet returns on the next restart. They lie outside `program/`,
      which a package swap replaces whole. An installation that fails for want of a directory is
      this Client's failure, not the operator's.
    - **Version:** asking the program for its version is the kind's — `collector`, `icinga2` and
      the wrapped kinds do it themselves. `version_args` exists only on `command`, where it is
      also the preflight a staged program is run with before the running one stops.

15. **Each wrapped agent is decided in an ADR of its own.** The rule here is general; what a
    particular kind knows is not, and an upstream that moves a path should touch one document:
    `glpi` and `telegraf` in [ADR-0015](0015-glpi-agent-and-telegraf.md), `icinga2` in
    [ADR-0016](0016-icinga-2.md). `collector` and `command` are decided in clause 16.

16. **What stays in a block, stays for a stated reason.**
    - `collector` is one kind for both distributions and keeps `binary`: `otelcol` and
      `otelcol-contrib` share a lifecycle, a configuration mechanism and a supervision story, and
      which one a host runs is a decision. Its `service_name` falls back to the program's file
      name.
    - `collector` and `command` keep `args` and `env`: a feature gate or a per-host value read as
      `${env:VAR}` is a decision nothing can derive. The wrapped kinds build both whole and take
      neither; an operator who needs to change how a wrapped agent runs does it in that agent's
      configuration, which the fleet delivers.
    - `endpoint_port` is a key of `collector` only — the one kind whose Managed Process connects to
      the Endpoint. It cannot be derived: the port appears in the Collector configuration the fleet
      delivers, which this Client writes byte for byte and never rewrites. A non-zero value on
      another kind is refused; `0` is the default written out and passes. The default stays
      ephemeral so two Collector Supervisors on one host do not collide.
    - `command` keeps `args`, `env` and `version_args`: without them most Foreign Agents never find
      their configuration and report no version, and each fails visibly when wrong.
    - A wrapped kind may keep a key where a decision exists that nobody else can make — `icinga2`
      keeps its enrolment ([ADR-0016](0016-icinga-2.md)).

17. **Timing is a fleet policy, then a kind's correction — never a host's.** `[supervisors]`
    holds `stop_timeout_secs` (default 10) and `apply_grace_secs` (default 3); `[updates]` holds
    `retain_previous_secs` (default one day, [ADR-0018](0018-signed-package-delivery-from-allowed-sources.md)). A
    wrapped kind may correct any of the three where its agent demands it (Icinga 2's shutdown
    drains for up to a minute), and its blocks may state none of them. Only `collector` and
    `command` blocks may override them, because there no kind exists to hold the value.

18. **A Client reports which kinds it carries.** Its own Agent reports one non-identifying
    attribute per compiled-in kind, `supervisor.kind.<kind> = "true"`, so a Selector can aim a
    Supervisor set at Clients that can run it. One key per kind, because a Selector is equality
    over string values and the question is about one member of the set. An operator's own
    attribute of the same key is left as written. `AvailableComponents` is not used for this: the
    message is *Development* in the pinned Baseline
    ([ADR-0009](0009-protocol-baseline-and-conformance.md)) and Selectors do not match it.

19. **A kind binds itself to one artifact's shape.** The derived paths are those of the artifact
    `opamp-package-fetch` builds. A tree packed differently does not fit, and the answer is to
    repack it with the tool, not to reopen a key.

20. **Every wrapped agent has an artifact document, pinned by two tests.** The shape a kind runs
    and the tool packs is written once, in `docs/artifacts/<agent>.md`, for the wrapped kinds only
    (`icinga2`, `glpi-agent`, `telegraf`). `collector` needs none: its artifact is installed as
    published, found by the name in `binary`, and handed configurations it does not inspect, so
    there is no second end to keep in step. Each document has the same eight sections: source
    (repository, tag form, version reading); assets per platform and their naming; integrity
    (checksum form and location); treatment (as published, or which repack); shape in the
    delivered tree; what the Client derives from it, per platform, and why; the Configurations the
    tool uploads and what the agent reads them as; and what can change upstream, with the test
    that goes red. Each document is pinned by **one test per side** — one against the tool's `Plan`
    (asset name, checksum source, action, output name), one against the kind's constants (program
    name, `program_path`, derived directories, arguments), `cfg`-gated per platform — and both name
    the document.

**Out of scope:** dynamic plugin loading; plain-HTTP polling on the Supervisor Endpoint;
injecting the `opampextension` configuration into a Collector's configuration; connection pools
larger than one (Gateway Mode, [ADR-0034](0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)); reporting the
kinds through `AvailableComponents`; a machine-readable artifact manifest shared by tool and kind;
where a Supervisor's directory lies and how its set changes
([ADR-0032](0032-a-host-can-keep-its-supervisor-set-from-the-server.md)).

## Alternatives considered

- **Dynamic plugin loading (shared libraries).** Rust has no stable ABI, so it means a C ABI
  boundary, unsafe code and version-skew handling for a third-party capability nobody needs; the
  registry keeps a new kind to one module.
- **A serde tagged enum instead of a registry and a two-stage parse.** It hard-codes every kind's
  settings into `config.rs` and cannot combine with `deny_unknown_fields`.
- **A `Supervisor` trait with async lifecycle methods.** Async trait objects need `async-trait` or
  hand-rolled boxing, and a core that awaits adapter methods couples itself to adapter timing.
  The channel vocabulary is the uniform interface.
- **axum (or hyper) for the Supervisor Endpoint, or plain-HTTP support on it.** The endpoint serves
  one loopback WebSocket peer, which `tokio-tungstenite` already handles; the reference template is
  `ws://`, so HTTP polling would have no consumer.
- **One upstream connection per Supervisor.** The Server routes by `instance_uid` regardless, and
  n-over-1 is the model Gateway Mode generalises.
- **Injecting the extension configuration à la `opampsupervisor`.** It needs a YAML stack and
  templating in Rust; the operator pins `endpoint_port` on the Collector block and the delivered
  configuration names it.
- **A distinct `Reload` command beside `ApplyConfig`.** Nothing upstream asks for a bare reload, so
  it would have no sender; reload is how an apply is executed.
- **Operator-written lifecycle hooks (`reload_cmd`, `uninstall_cmd`, `reload_signal`) in TOML.**
  Kind knowledge belongs in the kind's module, written once and tested; a wrong hook silently
  misbehaves on a host.
- **Moving program resolution and package consent into the plugins.** A fleet-visible capability
  must derive from configuration the core reads.
- **Keep the derivable keys and fix the documentation, or keep them undocumented.** A template is a
  default that cannot be updated, and two sources of truth for a value the Client computes is how
  a host quietly differs from what the fleet believes.
- **Split `collector` into `otelcol` and `otelcol-contrib`.** It moves the distribution decision
  from `binary` into `type` and costs a second plugin for one lifecycle.
- **A machine-readable manifest per agent, read by tool and Client.** It needs a schema, a parser
  on each side and a policy for a manifest newer than the Client; two tests achieve the coupling.
- **Probing the unpacked tree at runtime, or a per-kind defaults file.** A wrong guess is silent on
  a host, and data nothing tests re-creates the transcription; a compiled-in constant fails a test
  when the artifact moves.
- **One list attribute for the kinds.** A Selector could match it only by spelling the whole list.

## Sources / Prior art

- [`opampsupervisor` specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  and its [embedded extension template](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/templates/opampextension.yaml)
  (`ws://127.0.0.1:{{.SupervisorPort}}/v1/opamp`) — local server, config handling, restart and stop
  behaviour; the generic `agent.executable` shape for a supervisor that does not install its agent.
- [`opampextension` README](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — a client of both transports reporting effective configuration, health and components.
- [serde-rs/serde#1547](https://github.com/serde-rs/serde/issues/1547) — `deny_unknown_fields`
  does not compose with `flatten`.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  `instance_uid` multiplexing; restart as the only process command; `AvailableComponents`.
- [Nomad task driver plugins](https://developer.hashicorp.com/nomad/docs/concepts/plugins/task-drivers)
  — one driver interface over arbitrary process kinds, with stop and destroy as separate steps.
- [systemd service units](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
  — `ExecReload` and `reload-or-restart`.
- [Puppet package type and providers](https://www.puppet.com/docs/puppet/7/types/package.html) —
  a uniform resource whose platform mechanics live in the provider; the manifest states intent.
- Elastic Agent / Fleet integrations (defaults that work unconfigured, paths resolved by the
  agent's own `paths` package) and Datadog Agent integrations (`conf.d/<name>.d/conf.yaml`,
  per-integration defaults) — the tool that owns the installation knows the layout.

## Consequences

- Positive: many Supervisors per Client, a Foreign Agent managed like a Collector, a new kind as
  one module, and an extension-carrying Collector visible through its own reporting — with no
  Server change, on the tested `instance_uid` routing.
- Positive: a wrapped agent's block is `type` and `name`, the same two lines on every platform;
  an artifact whose layout moves is one edit in the kind, reaching every host with the next Client
  version; the platform branches are compile-time constants a test can assert.
- Positive: reload-capable agents keep their state across configuration changes; retiring a
  Supervisor undoes what its installation did; a delivered agent never fails for a missing
  directory; a rollout can aim only at Clients carrying the kind.
- Positive: an upstream change surfaces as a red test on the side it touches, with a document
  saying what the other side owes.
- Negative / trade-offs: losing the one upstream connection affects every Agent riding it, and a
  reconnect resends full state per Agent. An ephemeral Endpoint port cannot serve an
  extension-carrying Collector; the operator pins it.
- Negative / trade-offs: `APPLIED` means "restarted and survived the grace", not "validated"; a
  process that accepts a configuration and fails later surfaces as unhealthy. A reload leaves less
  evidence than a restart, so an adapter that cannot verify its reload says so in its outcome.
- Negative / trade-offs: supporting a new agent well costs a wrapper, a document and two tests;
  `command` keeps "not yet wrapped" usable, but such an agent applies by restart and runs in its
  program's directory. Kind-specific installs carry their own rollback burden.
- Negative / trade-offs: an Agent cannot be tagged per block before its first message; a label
  covers it from the first exchange on. One attribute per kind reads worse than a list.
- Negative / trade-offs: a kind is coupled to one artifact's shape — "bring your own tree" is a
  packaging question, not a configuration one. The documents' prose has no test behind it.
- Follow-ups (by topic): extension-configuration injection; plain-HTTP polling on the Endpoint;
  connection pools and their failure semantics; a verified-reload gate and making reload vs restart
  visible upstream; kinds that install through OS package managers; the kinds in
  `AvailableComponents` once it leaves *Development*; the next wrapped agent (Fluent Bit is the
  documented candidate); a machine-readable artifact manifest if wrapped agents multiply.

## Enforcement

- Registry, defaults and refusals, in `crates/fleet-agent/src/supervisor/mod.rs`:
  `a_wrapped_block_needs_neither_a_program_nor_a_type`,
  `a_wrapped_block_that_restates_a_derived_value_is_refused`, `every_wrapped_kinds_block_is_two_lines`,
  `an_unwrapped_kind_still_says_everything_itself`, `a_client_reports_the_kinds_it_was_compiled_with`,
  `a_wrapped_kind_corrects_the_fleets_timing_and_the_block_says_nothing`,
  `the_fleets_timing_reaches_a_supervisor_that_says_nothing`.
- Two-stage parse and retired keys: `supervisor_blocks_split_common_keys_from_plugin_settings`,
  `a_supervisor_block_needs_type_and_a_valid_name`, `duplicate_supervisor_names_are_rejected`,
  `attributes_describe_the_host_and_a_block_no_longer_tags_one_agent` (`crates/fleet-agent/src/config.rs`);
  `settings_parse_strictly` and `the_retired_keys_are_refused_by_name` in `command.rs`,
  `settings_parse_strictly` in `collector.rs`, the `the_recipes_keys_are_refused_by_name` tests of
  `glpi.rs` and `telegraf.rs`, `a_retired_key_is_refused_by_name_and_says_what_supplies_it_now` in
  `icinga2.rs`; `an_unreadable_configuration_resolves_the_update_in_flight`
  (`crates/fleet-agent/src/service/runtime.rs`).
- Agents over one connection and the Endpoint: `poll_reports_carries_every_agent_with_distinct_identities`,
  `a_reply_reaches_only_the_agent_its_uid_names` (`crates/fleet-agent/src/engine.rs`);
  `extension_reports_are_folded_into_process_events` (`supervisor/endpoint.rs`);
  `a_config_change_reaches_both_supervised_agents_over_one_connection` (`crates/fleet-agent/tests/e2e.rs`).
- Process management and the lifecycle vocabulary, in `crates/fleet-agent/tests/supervisor_process.rs`:
  `an_exiting_process_turns_unhealthy_and_is_restarted`,
  `a_process_surviving_the_apply_grace_is_acknowledged_applied`,
  `a_process_exiting_within_the_grace_fails_the_apply_and_stays_supervised`,
  `a_restart_command_cycles_the_process_without_a_config_ack`,
  `a_declared_reload_applies_without_a_restart`,
  `a_process_that_dies_on_the_reload_signal_is_restarted_instead`,
  `an_uninstall_stops_the_process_answers_and_exits`,
  `the_directories_an_agent_writes_into_are_made_before_it_runs`,
  `a_program_named_by_a_relative_path_still_starts_in_its_own_directory`; and
  `retiring_uninstalls_the_removed_and_only_stops_the_changed` (`engine.rs`),
  `a_collector_supervisor_passes_each_config_entry_as_a_config_flag`,
  `sigterm_stops_the_managed_process_and_the_client_cleanly` (`crates/fleet-agent/tests/supervisor.rs`),
  `a_two_line_telegraf_block_runs_reports_and_applies` and
  `a_two_line_glpi_block_runs_reports_and_applies` (`crates/fleet-agent/tests/wrapped_supervisors.rs`).
- Artifact documents (clause 20): the kind side is `the_defaults_are_the_artifacts` in `glpi.rs`,
  `telegraf.rs` and `icinga2.rs`; the packing side is `glpi_finds_both_zip_spellings_and_repacks_only_linux`,
  `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` and
  `icinga_2s_windows_artifact_is_the_msi_verified_by_its_publisher` in
  `crates/fleet-tools/src/bin/opamp-package-fetch.rs`. Each names its document.

