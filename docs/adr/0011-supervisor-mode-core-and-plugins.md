# ADR-0011: Supervisor Mode — a hexagonal supervision core, compiled-in plugins that own their process kind, n Agents over one connection

- **Status:** 🟡 proposed
- **Date:** 2026-07-23
- **Deciders:** Markus Brigl

## Context

### Supervisor Mode

The base control loop is complete on both ends and both transports (ADR-0005, ADR-0006, ADR-0007, ADR-0008, ADR-0009 and ADR-0010), but
without Supervisor Mode the Client presents exactly **one Agent — itself** — and "applying" a
configuration means persisting it to disk (`crates/client/src/agent.rs`). The Supervisors that
ADR-0003 binds — the reason the client side exists — need a home: a process to manage, a Supervisor
Endpoint to serve, and the n-Agents-over-m-connections shape on the client side. On the Server that
shape is already implemented and tested (`two_agents_share_one_connection`,
`crates/server/tests/ws_transport.rs`).

This decision covers the client half of that gap for **Supervisor Mode only**. Gateway Mode,
connection pools larger than one, and package delivery remain out of scope. The scope is the three
integration paths a Managed Process can take, all within Supervisor Mode
([specification vocabulary](../SPECIFICATION.md)):

1. A **Collector carrying the `opampextension`** — reports its own description, health, and
   effective configuration to the Supervisor Endpoint, which relays them upstream (goal 16).
2. A **Collector without the extension** — the Supervisor observes what it can from the outside:
   spawn success, exit status, restart behaviour.
3. A **Foreign Agent under a Custom Supervisor** — a plugin translates the process's lifecycle,
   configuration, and health into OpAMP (goals 7 and 8). This project ships an example Custom
   Supervisor that runs a configured command-line invocation.

The forces are fixed by earlier decisions. The specification demands a hexagonal core: the
supervision domain written against two **Ports** — the Server-facing side speaking OpAMP and the
Managed-Process side (lifecycle, configuration, health) — with **Plugins** as adapters on the
Managed-Process side. ADR-0003 binds the Supervisor Endpoint as intrinsic to every Supervisor and
`instance_uid` as the sole routing key. ADR-0005 keeps hexagonal seams as modules until a concrete
need makes them crates. ADR-0008 anticipated `[[supervisor]]` blocks in `client.toml`. ADR-0010
gives every instance a state directory and a bounded shutdown budget under service managers.

The prior art (reconfirmed for this decision; see Sources) is the Collector contrib repository's
`opampsupervisor`: it runs a local OpAMP server the extension connects to, injects the extension's
configuration from an embedded template whose endpoint is **`ws://127.0.0.1:{{port}}/v1/opamp`** —
WebSocket only — passes configuration to the Collector via `--config`, restarts it on remote-config
change, stops it with SIGTERM then kill after a timeout, and watchdog-restarts an unexpectedly
exited Collector with exponential backoff. The `opampextension` itself is a client supporting both
transports, but the reference supervisor's local endpoint is exercised exclusively over `ws://` on
loopback.

One Rust-specific force: serde cannot combine `#[serde(flatten)]` with `deny_unknown_fields`
(serde-rs/serde#1547), so a `[[supervisor]]` block whose type-specific keys live beside the common
ones cannot be parsed as one strict struct — loud typo rejection, which ADR-0008 requires, needs a
two-stage parse.

### One lifecycle vocabulary

More specific Supervisors are coming: process kinds beyond the shipped `collector` and `command`,
each with its own way of being installed, configured, reloaded, and removed. What they need is a
**uniform interface** that every kind implements, so that the supervision core drives any of them
identically while the kind-specific mechanics — installation, uninstallation, start, stop, update,
reload, configuration handling — live in the specific implementation.

Much of that interface exists in the Supervisor Mode decision below: the Managed-Process-facing Port
is a message pair — `ProcessCommand` in, `ProcessEvent` out — plus the `Plugin` factory trait and the
compiled-in registry; a new kind is one module and one registry line (goal 8). Start, stop,
restart, and configuration apply run inside the specific adapter task, with the shared
`Runner` (spawn, watchdog, backoff, bounded stop) as the implementation both shipped plugins use.

But without a complete vocabulary, two operations sit in the core that the coming kinds need to
own:

1. **There is no reload.** An `ApplyConfig` that only means "restart on the new files" is the only
   apply strategy the Port can express. Agents that reload their configuration in place (Fluent Bit
   and many daemons on `SIGHUP`, others through an admin API) are needlessly restarted, losing
   in-flight state and buffers on every configuration change. The service managers this project
   already integrates with (ADR-0010) all treat reload as first-class beside restart: systemd's
   `ExecReload`/`reload-or-restart` is exactly this distinction.
2. **Install and update mechanics are fixed by the core.** `ApplyPackage` hands the adapter a
   verified artifact, but what "install" means is hard-coded in `InstallTarget` — a file or tree
   swap (ADR-0015). A kind whose program is not a swappable file — one installed
   through a native installer or an OS package manager — cannot express its install step at all.
3. **Uninstall bypasses the plugin entirely.** ADR-0019 purges a removed Supervisor: the core
   stops the process and deletes the directory. A kind whose installation had side effects
   outside its directory (a registered service, package-manager state, created users) has no
   hook to undo them — the purge leaves them behind.

Three forces constrain the shape. The Supervisor Mode decision deliberately makes the Port channels,
not an async trait — object safety without `async-trait`, adapters as plain tasks, a domain core
free of process handles — and nothing here invalidates that reasoning. ADR-0019 derives the
fleet-visible `AcceptsPackages` capability from the written shape of the program path in the
core; a plugin that decided installation for itself could disagree with the declared consent.
And ADR-0008's strict parsing means any new per-kind setting must fail loudly on a typo.

## Decision

### Supervisor Mode

We will implement Supervisor Mode as a **hexagonal supervision core in `crates/client`** — modules,
not new crates — with **compiled-in Supervisor plugins** selected by a `type` field in
`[[supervisor]]` TOML blocks, each Supervisor appearing to the Server as **its own Agent**
multiplexed over **one shared upstream connection**, and each Supervisor serving a **WebSocket-only
Supervisor Endpoint** on loopback.

Concretely this binds:

- **Two Ports, as modules.** The Managed-Process-facing Port is a message pair —
  `ProcessCommand` (apply this persisted configuration; shut down) and `ProcessEvent`
  (description, health, effective configuration, configuration outcome) — plus a `Plugin` factory
  trait that validates a block's settings and starts the adapter task. Channel-based messages keep
  the trait object-safe without an `async-trait` dependency, make every adapter a plain tokio task,
  and keep the domain core free of process handles. The Server-facing Port is the engine seam the
  transports already consume — build reports, handle a `ServerToAgent`, produce disconnects — now
  over *n* Agents; the WebSocket and plain-HTTP transports remain its adapters.
- **A compiled-in plugin registry.** `"collector"` (the Collector Supervisor) and `"command"` (the
  example Custom Supervisor for a Foreign Agent) ship first; a new process kind is a new module and
  one registry entry (goal 8). Dynamic loading is not taken up — it buys third-party plugins at the
  price of ABI stability and unsafe code, and no present need justifies it.
- **TOML shape.** Each `[[supervisor]]` block carries the common keys `type`, `name`,
  `endpoint_port` (default `0` = ephemeral loopback port), and `stop_timeout_secs` (default 10);
  everything else belongs to the plugin, which parses it strictly (`deny_unknown_fields`) in the
  second stage of a two-stage parse, so a typo anywhere in the block still fails loudly at startup.
  Supervisor names follow the instance-name grammar of ADR-0010 — they become directory names.
- **Per-Supervisor state.** Each Supervisor owns `<state_dir>/supervisors/<name>/`, reusing the
  existing `Storage` layout unchanged: its own persisted UUID-v7 `instance-uid`, the last
  `remote-config.pb`, and the written-out `config/` files its Managed Process reads.
- **Each Supervisor is one Agent; m = 1.** Own identity, own `sequence_num`, own capability set,
  all carried over a single upstream connection and disambiguated by `instance_uid` alone — the
  general n-over-m model of ADR-0003 with the pool fixed at one; pool sizing stays deferred to
  Gateway Mode. The Server needs no change.
- **Zero supervisors keeps the self-agent.** A `client.toml` without `[[supervisor]]` blocks
  presents the Client itself as the single Agent — the same agent state machine with no process
  handle, one code path, no fork. Existing deployments, the shipped default configuration, and the
  existing tests stay valid.
- **Supervisor Endpoint: WebSocket only.** Bound at startup to `127.0.0.1:<endpoint_port>`,
  accepted with `tokio-tungstenite` — a dependency the Client already carries; no HTTP server
  framework enters the client crate. The endpoint folds **content, not identity**: the extension's
  description, health, and effective configuration are folded into the owning Supervisor's Agent
  (its `service.instance.id` stays the Supervisor's), and the extension keeps its own uid locally.
  Plain-HTTP polling on the endpoint is a recorded possible follow-up; the reference supervisor's
  injected extension config is ws-only, so nothing needs it today.
- **Process management mirrors the reference supervisor.** Spawn via `tokio::process`; on a
  remote-config change, stop gracefully and respawn with the newly written files; watchdog-restart
  an unexpectedly exited process with the existing exponential backoff; graceful stop is
  SIGTERM → bounded wait (`stop_timeout_secs`) → kill on Unix (a unix-only `libc` dependency for
  `kill(2)`) and `Child::kill` on Windows. On Client shutdown, Managed Processes stop first, then
  each Agent's `agent_disconnect` goes out — inside the service managers' stop budgets (ADR-0010).
  The Collector Supervisor passes every written config-map entry as its own `--config` argument and
  lets the Collector do its own merging — no YAML manipulation in Rust.
- **Configuration status becomes honest.** `RemoteConfigStatus` reports `APPLYING` on receipt,
  `APPLIED` only after the Managed Process (re)started successfully with the new configuration, and
  `FAILED` with the error otherwise — goal 4 end to end, not storage-deep.

### One lifecycle vocabulary

We will complete the Managed-Process Port into **one closed lifecycle vocabulary** — install,
uninstall, start, stop, update, reload, and configuration apply — where every operation is
**executed by the specific plugin's adapter** behind the existing channel Port, and the shared
`Runner` remains the default implementation a kind opts into, never a constraint it must fit.

Concretely this binds:

- **Defaults first: every operation has a generic implementation, and silence selects it.** The
  shared `Runner` implements the whole vocabulary — spawn-and-watchdog start, graceful bounded
  stop, restart as the configuration apply, swap-and-gate as the package install, stop-only
  uninstall. A specific Supervisor overrides only the steps its process kind genuinely does
  differently (a reload mechanism, a native install, an uninstall with outside side effects);
  every step it leaves alone falls through to the generic behavior. A kind that overrides
  nothing is a valid, complete Supervisor — it behaves exactly like the plain `command` kind.
- **The Port stays a message pair.** The uniform interface *is* the command/event vocabulary of
  the two Ports above, now complete — not a new trait with lifecycle methods. Start (spawn on
  adapter startup), stop (`Shutdown`), and restart (`Restart`) are the adapter's; they stay
  unchanged.
- **Reload is an apply strategy, not a new operation.** OpAMP has no reload command — a reload
  only ever happens *because* a configuration arrived — so `ApplyConfig` remains the single
  configuration operation, and the adapter chooses how to apply it: restart (the default) or a
  kind-specific reload declared in the block's plugin settings (for the `command` kind a
  `reload_signal`, unix-only and rejected by the strict parse on Windows; a future kind may
  reload through an API call). The semantics are systemd's `reload-or-restart`: a reload that
  fails — the mechanism errors, or the process dies — falls back to a restart on the new files.
  The health-gated acknowledgement (`apply_grace`) applies to either path.
- **Install and update mechanics move behind the plugin.** `ApplyPackage` stays the operation and
  keeps its contract (verified artifact in, health-gated outcome out, rollback on failure —
  ADR-0015). The swap-and-gate on `InstallTarget` becomes the shared default helper an
  adapter calls; a kind whose installation is not a file swap implements the step itself, inside
  the same contract. Program-path resolution and the package consent derived from it stay in the
  core — ADR-0019 is untouched.
- **Uninstall joins the vocabulary.** A new `ProcessCommand::Uninstall`, answered by a new
  `ProcessEvent::Uninstalled(Result)`, is sent when a Supervisor is retired (ADR-0019) before the
  core purges its directory (ADR-0019): the adapter stops its process and undoes whatever its
  installs did outside the directory; the default is exactly the graceful stop. The
  purge itself — deleting the directory — stays the core's, and stays bounded: an adapter that
  does not answer within the stop budget is treated as stopped and the purge proceeds, so a
  hanging uninstall cannot block retirement. This extends ADR-0019; it does not replace it.
- **Configuration management is split by ownership: the core persists, the adapter delivers.**
  Receiving, validating, persisting, and status-reporting a remote configuration stay in the
  core — they are OpAMP mechanics, identical for every kind. Everything between the written
  files and the running process — pointing the process at them, merging, choosing restart or
  reload, applying through an API — is the specific implementation's.
- **The registry and the two-stage parse are unchanged.** A new process kind remains one module
  and one registry line; its settings — reload mechanism included — parse strictly in the second
  stage.

## Alternatives considered

### Supervisor Mode

- **Dynamic plugin loading (shared libraries).** Rejected for now. Rust has no stable ABI, so this
  means a C ABI boundary, unsafe code, and version skew handling — real complexity for a
  third-party-plugin capability nobody needs yet. The registry keeps goal 8 cheap (a new kind is a
  new module); dynamic loading can supersede this in its own ADR when a concrete need appears.
  The lifecycle vocabulary changes nothing in that ABI calculus.
- **A serde tagged enum instead of a registry with two-stage parsing.** Rejected. `#[serde(tag =
  "type")]` would hard-code every plugin's settings into `config.rs` and cannot be combined with
  `deny_unknown_fields` (serde#1547) — precisely the strictness ADR-0008 demands. The two-stage
  parse keeps the core generic over plugins and keeps typo rejection loud.
- **`async-trait` object methods as the Managed-Process Port.** Rejected. Async trait objects need
  either the `async-trait` crate or hand-rolled boxing, and a trait whose methods the domain awaits
  couples the core to adapter timing. Channels make the Port a data contract, adapters plain tasks,
  and the domain testable without any process.
- **axum (or hyper directly) for the Supervisor Endpoint.** Rejected. The endpoint serves exactly
  one loopback WebSocket peer per Supervisor; `tokio-tungstenite::accept_async` on an accepted TCP
  stream does that with a dependency the client already has. axum would enter the client crate to
  route one path. If plain-HTTP polling on the endpoint is ever needed, that follow-up can revisit
  the choice.
- **Plain-HTTP support on the Supervisor Endpoint now.** Rejected. The `opampextension` defaults to
  WebSocket for a supervisor endpoint (the reference template is `ws://`), and no other local
  client exists. A SHOULD-shaped nicety with no consumer is YAGNI.
- **One upstream connection per Supervisor.** Rejected — ADR-0003 already rejected
  connection-per-agent; the Server routes by `instance_uid` regardless, and n-over-1 is the model
  Gateway Mode will generalize.
- **Requiring at least one `[[supervisor]]` block.** Rejected. It would invalidate every existing
  deployment, the shipped default configuration, and the existing shutdown test, and buy nothing:
  the self-agent is the same state machine without a process handle.
- **Extension-config injection à la `opampsupervisor` (template + YAML merge).** Deferred. It
  requires a YAML stack and templating in Rust. Instead, the operator pins `endpoint_port` and the
  *distributed* Collector configuration carries the `opamp` extension block pointing at
  `ws://127.0.0.1:{{port}}/v1/opamp`. Injection is the natural follow-up once configuration
  templating is wanted for other reasons.

### One lifecycle vocabulary

- **A new `Supervisor` trait with async `install()`/`start()`/`stop()`/`reload()`… methods.**
  Rejected. It re-opens what the channel Port above decides against: async trait objects need
  `async-trait` or hand-rolled boxing, and a core that awaits adapter methods couples itself to
  adapter timing. The channel vocabulary *is* the unified interface; making it complete is
  cheaper than replacing it.
- **A distinct `ProcessCommand::Reload` beside `ApplyConfig`.** Rejected. Nothing upstream ever
  asks for a bare reload — OpAMP drives configuration and packages, and its only process command
  is restart — so a separate reload command would have no sender. Reload is *how* an apply is
  executed, which is precisely the kind-specific knowledge the plugin owns.
- **Moving program resolution and package consent into the plugins.** Rejected, as when ADR-0019
  bound it: `AcceptsPackages` is fleet-visible, and a capability the Server acts on must derive
  from configuration the core reads, never from per-kind behavior that could disagree with it.
- **Operator-written lifecycle hooks in TOML (`uninstall_cmd`, `reload_cmd` on every block).**
  Rejected. Kind knowledge belongs in the kind's module, written once — not re-invented in every
  operator's configuration, where a wrong hook silently corrupts a host. A settings key that
  *selects* a mechanism the plugin implements (`reload_signal`) is fine; a key that *is* the
  mechanism is not.

## Sources / Prior art

- [`opampsupervisor` specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the reference supervisor's architecture: local OpAMP server, config handling, restart and stop
  behaviour, noop-config bootstrap.
  It restarts on configuration change and on package replacement; it has no reload or uninstall,
  which is the gap the lifecycle vocabulary fills for process kinds beyond the Collector.
- [`opampsupervisor` embedded extension template](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/templates/opampextension.yaml)
  — `endpoint: "ws://127.0.0.1:{{.SupervisorPort}}/v1/opamp"`: the injected extension
  configuration is WebSocket-only on loopback, which justifies the WS-only Supervisor Endpoint.
- [`opampextension` README](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — the extension is a client only, supports ws and http transports scheme-selected, and reports
  effective configuration, health, and available components.
- [serde-rs/serde#1547](https://github.com/serde-rs/serde/issues/1547) — `deny_unknown_fields`
  does not compose with `#[serde(flatten)]`; motivates the two-stage `[[supervisor]]` parse.
- [OpAMP specification, `ServerToAgent.instance_uid`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — the multiplexing provision this design rides on, already cited and bound by ADR-0003.
  Baseline `v0.18.0` (see [`CONFORMANCE.md`](../CONFORMANCE.md)).
  The only process command it defines is restart, and packages arrive via `PackagesAvailable`:
  upstream never speaks "reload" or "uninstall", confirming both as Client-side vocabulary, not
  protocol.
- [Nomad task driver plugins](https://developer.hashicorp.com/nomad/docs/concepts/plugins/task-drivers)
  — the closest shape: one driver interface (`StartTask`, `StopTask`, `SignalTask`,
  `DestroyTask`) over arbitrary process kinds, with **stop** (halt the process) and **destroy**
  (clean up what running it created) as deliberately separate steps — the same separation
  `Shutdown` vs. `Uninstall` draws here.
- [systemd service units](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
  — reload (`ExecReload`, `reload-or-restart`) as first-class beside restart, with restart as
  the fallback when a unit exposes no reload path; the semantics this decision adopts for the apply
  strategy.
- [Puppet package providers](https://www.puppet.com/docs/puppet/7/types/package.html) — one
  resource type with `install`/`uninstall`/`update` implemented per provider (dpkg, rpm, msi, …):
  the established pattern of a uniform lifecycle interface whose mechanics live in the specific
  implementation.

## Consequences

### Supervisor Mode

- Positive: goals 1–8, 14, and 16 become reachable — many Supervisors per Client, a Foreign Agent
  managed indistinguishably from a Collector, a new process kind as one new module, and the
  extension-carrying Collector visible in the fleet through its own reporting. No Server change is
  needed; the tested `instance_uid` routing carries it.
- Positive: the hexagonal seam is real code — the domain core knows Ports, not process kinds —
  so Gateway Mode later composes onto the same seam instead of forking the engine.
- Positive: at most one new dependency (`libc`, unix-only, for `SIGTERM`); the Supervisor Endpoint
  reuses `tokio-tungstenite` and the shared `opamp::frame` codec.
- Negative / trade-offs: losing the single upstream connection affects every Agent riding it —
  accepted by ADR-0003; reconnection resends full state per Agent. Ephemeral endpoint ports cannot
  serve an extension-carrying Collector without injection: that path requires the operator to pin
  `endpoint_port` and put the extension block into the distributed configuration.
- Negative / trade-offs: `APPLIED` means "the process restarted with the new config", not "the
  process validated the config" — a Managed Process that starts and later chokes on its
  configuration surfaces as unhealthy, not as a rejected configuration. Refining that (e.g. a
  post-start health gate before acking) is future work.
- Follow-ups (by topic): extension-configuration injection/templating; plain-HTTP polling on the
  Supervisor Endpoint; connection pools larger than one and their balancing/failure semantics
  (with Gateway Mode); `AcceptsRestartCommand` and relaying `ReportsAvailableComponents`; a
  health-gated apply acknowledgement.

### One lifecycle vocabulary

- Positive: the coming specific Supervisors are each one module implementing one complete,
  uniform vocabulary — a kind with a native installer, a reload signal, or an API-applied
  configuration fits without touching the core (goal 8 stays cheap). Reload-capable agents keep
  their in-flight state across configuration changes. Retiring a Supervisor undoes what
  installing it did, instead of only deleting its directory.
- Negative / trade-offs: the vocabulary grows by two commands, but a kind that rides the shared
  `Runner` never sees them — only an adapter that replaces the `Runner` wholesale must handle
  them itself. A reload leaves less outside-observable evidence than a restart — the
  process never exits, so the health gate reads a process that may still run on the old
  configuration; an adapter that cannot verify its reload took effect must say so in the
  `ConfigApplied` outcome rather than acknowledge blindly. Kind-specific install steps mean the
  rollback guarantee of ADR-0015 is only as good as each kind's implementation of it — the
  shared helper keeps that honest for the swap case, new kinds carry the burden themselves.
- Follow-ups (by topic): making reload vs. restart visible upstream (both surface only as
  health); a verified-reload gate (asking the process which configuration it now runs before
  acknowledging); process kinds that install through OS package managers and what package
  consent (ADR-0019) means for them.
