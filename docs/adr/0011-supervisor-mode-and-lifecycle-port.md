# ADR-0011: Supervisor Mode — a hexagonal supervision core, compiled-in plugins, n Agents over one connection, and one lifecycle vocabulary every plugin executes

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

The Supervisors that ADR-0003 binds are the reason the client side exists: processes managed on a
host, each served a Supervisor Endpoint, each visible to the Server as an Agent. On the Server, the
n-Agents-over-m-connections shape is implemented and tested (`two_agents_share_one_connection`,
`crates/server/tests/ws_transport.rs`). This decision covers the client half for **Supervisor Mode
only**. Gateway Mode (ADR-0024) and package delivery (ADR-0015) are decided in their own ADRs. The
scope is the three integration paths a Managed Process can take, all within Supervisor Mode
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
need makes them crates. ADR-0008 anticipated `[[supervisor]]` blocks in the configuration file, now
`supervisor.toml` (ADR-0022 clause 10). ADR-0010 gives every instance a state directory and a bounded
shutdown budget under service managers.

The prior art (see Sources) is the Collector contrib repository's `opampsupervisor`: it runs a local
OpAMP server the extension connects to, injects the extension's configuration from an embedded
template whose endpoint is **`ws://127.0.0.1:{{port}}/v1/opamp`** — WebSocket only — passes
configuration to the Collector via `--config`, restarts it on remote-config change, stops it with
SIGTERM then kill after a timeout, and watchdog-restarts an unexpectedly exited Collector with
exponential backoff. The `opampextension` itself is a client supporting both transports, but the
reference supervisor's local endpoint is exercised exclusively over `ws://` on loopback.

One Rust-specific force: serde cannot combine `#[serde(flatten)]` with `deny_unknown_fields`
(serde-rs/serde#1547), so a `[[supervisor]]` block whose type-specific keys live beside the common
ones cannot be parsed as one strict struct — loud typo rejection, which ADR-0008 requires, needs a
two-stage parse.

Process kinds beyond the shipped `collector` and `command` each have their own way of being
installed, configured, reloaded, and removed. They need a **uniform interface** that every kind
implements, so that the supervision core drives any of them identically while the kind-specific
mechanics — installation, uninstallation, start, stop, update, reload, configuration handling —
live in the specific implementation. Three gaps stand between the Port and that interface:

1. **Reload.** Agents that reload their configuration in place (Fluent Bit and many daemons on
   `SIGHUP`, others through an admin API) are needlessly restarted when restart is the only apply
   strategy, losing in-flight state and buffers on every configuration change. The service
   managers this project integrates with (ADR-0010) all treat reload as first-class beside restart:
   systemd's `ExecReload`/`reload-or-restart` is exactly this distinction.
2. **Install and update mechanics.** A swap of a file or tree (ADR-0015) is one way to
   install. A kind whose program is not a swappable file — one installed through a native
   installer or an OS package manager — needs to express its install step itself.
3. **Uninstall.** A purge of a removed Supervisor (ADR-0029) that only stops the process and
   deletes the directory leaves behind whatever an installation did outside its directory (a
   registered service, package-manager state, created users).

Two further forces constrain the shape of that vocabulary. ADR-0018 keep program-path
resolution, and with it the fleet-visible `AcceptsPackages` capability, in the core; a plugin that
decided installation for itself could disagree with the declared capability. And ADR-0008's strict
parsing means any new per-kind setting must fail loudly on a typo.

## Decision

We will implement Supervisor Mode as a **hexagonal supervision core in `crates/client`** — modules,
not new crates — with **compiled-in Supervisor plugins** selected by a `type` field in
`[[supervisor]]` TOML blocks, each Supervisor appearing to the Server as **its own Agent**
multiplexed over **one shared upstream connection**, and each Supervisor serving a **WebSocket-only
Supervisor Endpoint** on loopback. The Managed-Process Port is **one closed lifecycle vocabulary** —
install, uninstall, start, stop, update, reload, and configuration apply — where every operation is
**executed by the specific plugin's adapter** behind the channel Port, and the shared `Runner` is the
default implementation a kind opts into, never a constraint it must fit.

### The core, the plugins, and the Agents

1. **Two Ports, as modules.** The Managed-Process-facing Port is a message pair —
   `ProcessCommand` (apply this persisted configuration; shut down) and `ProcessEvent`
   (description, health, effective configuration, configuration outcome) — plus a `Plugin` factory
   trait that validates a block's settings and starts the adapter task. Channel-based messages keep
   the trait object-safe without an `async-trait` dependency, make every adapter a plain tokio task,
   and keep the domain core free of process handles. The Server-facing Port is the engine seam the
   transports consume — build reports, handle a `ServerToAgent`, produce disconnects — over *n*
   Agents; the WebSocket and plain-HTTP transports are its adapters.
2. **A compiled-in plugin registry.** `"collector"` (the Collector Supervisor) and `"command"` (the
   example Custom Supervisor for a Foreign Agent) ship first; a new process kind is a new module and
   one registry entry (goal 8). Dynamic loading is not taken up — it buys third-party plugins at the
   price of ABI stability and unsafe code, and no present need justifies it.
3. **TOML shape.** Each `[[supervisor]]` block carries the common keys `type` and `name`;
   everything else belongs to the plugin, which parses it strictly (`deny_unknown_fields`) in the
   second stage of a two-stage parse, so a typo anywhere in the block still fails loudly at startup.
   Which further keys a block may carry is ADR-0037's rule: `endpoint_port` (default `0` = ephemeral
   loopback port) is a key of the `collector` kind only (ADR-0037 clause 4), and `stop_timeout_secs`
   (default 10) is a global default that only the unwrapped kinds override per block (ADR-0037
   clause 5). Supervisor names follow the instance-name grammar of ADR-0010 — they become directory
   names.
4. **Per-Supervisor state.** Each Supervisor owns one directory, `<supervisor_dir>/<name>/`, laid out
   by ADR-0018 clause 1 and reusing the `Storage` layout: its own persisted UUID-v7 `instance-uid`,
   the last `remote-config.pb`, and the written-out `config/` files its Managed Process reads.
5. **Each Supervisor is one Agent; m = 1.** Own identity, own `sequence_num`, own capability set,
   all carried over a single upstream connection and disambiguated by `instance_uid` alone — the
   general n-over-m model of ADR-0003 with the pool fixed at one; pool sizing belongs to Gateway
   Mode (ADR-0024). The Server needs no change.
6. **Zero supervisors presents the Client itself.** A `supervisor.toml` without `[[supervisor]]`
   blocks presents the Client itself as the single Agent — the same agent state machine with no
   process handle, one code path, no fork. Deployments without blocks, the shipped default
   configuration, and the tests built on it stay valid.
7. **Supervisor Endpoint: WebSocket only.** Bound at startup to `127.0.0.1:<endpoint_port>`,
   accepted with `tokio-tungstenite` — a dependency the Client already carries; no HTTP server
   framework enters the client crate. The endpoint folds **content, not identity**: the extension's
   description, health, and effective configuration are folded into the owning Supervisor's Agent
   (its `service.instance.id` stays the Supervisor's), and the extension keeps its own uid locally.
   Plain-HTTP polling on the endpoint is a recorded possible follow-up; the reference supervisor's
   injected extension config is ws-only, so nothing needs it today.
8. **Process management mirrors the reference supervisor.** Spawn via `tokio::process`; on a
   remote-config change, stop gracefully and respawn with the newly written files; watchdog-restart
   an unexpectedly exited process with the existing exponential backoff; graceful stop is
   SIGTERM → bounded wait (`stop_timeout_secs`) → kill on Unix (a unix-only `libc` dependency for
   `kill(2)`) and `Child::kill` on Windows. On Client shutdown, Managed Processes stop first, then
   each Agent's `agent_disconnect` goes out — inside the service managers' stop budgets (ADR-0010).
   The Collector Supervisor passes every written config-map entry as its own `--config` argument and
   lets the Collector do its own merging — no YAML manipulation in Rust.
9. **Configuration status is honest.** `RemoteConfigStatus` reports `APPLYING` on receipt,
   `APPLIED` only after the Managed Process (re)started successfully with the new configuration, and
   `FAILED` with the error otherwise — goal 4 end to end, not storage-deep.

### One lifecycle vocabulary, executed by the plugin

10. **Defaults first: every operation has a generic implementation, and silence selects it.** The
    shared `Runner` implements the whole vocabulary — spawn-and-watchdog start, graceful bounded
    stop, restart as the configuration apply, swap-and-gate as the package install, stop-only
    uninstall. A specific Supervisor overrides only the steps its process kind genuinely does
    differently (a reload mechanism, a native install, an uninstall with outside side effects);
    every step it leaves alone falls through to the generic behavior. A kind that overrides
    nothing is a valid, complete Supervisor — it behaves exactly like the `command` kind.
11. **The Port stays a message pair.** The uniform interface *is* the command/event vocabulary of
    clause 1, complete — not a new trait with lifecycle methods. Start (spawn on adapter startup),
    stop (`Shutdown`), and restart (`Restart`) are the adapter's.
12. **Reload is an apply strategy, not a new operation.** OpAMP has no reload command — a reload
    only ever happens *because* a configuration arrived — so `ApplyConfig` remains the single
    configuration operation, and the adapter chooses how to apply it: restart (the default) or a
    kind-specific reload (a signal, or an API call). Which kinds reload, and how, is the kind's own
    knowledge, not a block key: ADR-0037 clause 2 removes `reload_signal`, so an unwrapped agent
    applies by restart. The semantics are systemd's `reload-or-restart`: a reload that fails — the
    mechanism errors, or the process dies — falls back to a restart on the new files. The
    health-gated acknowledgement (`apply_grace`) applies to either path.
13. **Install and update mechanics live behind the plugin.** `ApplyPackage` stays the operation and
    keeps its contract (verified artifact in, health-gated outcome out, rollback on failure —
    ADR-0015). The swap-and-gate on `InstallTarget` is the shared default helper an
    adapter calls; a kind whose installation is not a file swap implements the step itself, inside
    the same contract. Program-path resolution stays in the core (ADR-0018).
14. **Uninstall joins the vocabulary.** `ProcessCommand::Uninstall`, answered by
    `ProcessEvent::Uninstalled(Result)`, is sent when a Supervisor is retired (ADR-0029) before the
    core purges its directory (ADR-0029): the adapter stops its process and undoes whatever its
    installs did outside the directory; the default is the graceful stop. The purge itself —
    deleting the directory — stays the core's, and stays bounded: an adapter that does not answer
    within the stop budget is treated as stopped and the purge proceeds, so a hanging uninstall
    cannot block retirement.
15. **Configuration management is split by ownership: the core persists, the adapter delivers.**
    Receiving, validating, persisting, and status-reporting a remote configuration stay in the
    core — they are OpAMP mechanics, identical for every kind. Everything between the written
    files and the running process — pointing the process at them, merging, choosing restart or
    reload, applying through an API — is the specific implementation's.
16. **The registry and the two-stage parse hold for every kind.** A new process kind remains one
    module and one registry line; its settings parse strictly in the second stage.

## Alternatives considered

- **Dynamic plugin loading (shared libraries).** Rejected. Rust has no stable ABI, so this
  means a C ABI boundary, unsafe code, and version skew handling — real complexity for a
  third-party-plugin capability nobody needs yet. The registry keeps goal 8 cheap (a new kind is a
  new module); dynamic loading can supersede this in its own ADR when a concrete need appears.
  Nothing in the lifecycle vocabulary changes the ABI calculus.
- **A serde tagged enum instead of a registry with two-stage parsing.** Rejected. `#[serde(tag =
  "type")]` would hard-code every plugin's settings into `config.rs` and cannot be combined with
  `deny_unknown_fields` (serde#1547) — precisely the strictness ADR-0008 demands. The two-stage
  parse keeps the core generic over plugins and keeps typo rejection loud.
- **`async-trait` object methods as the Managed-Process Port** — including a `Supervisor` trait with
  async `install()`/`start()`/`stop()`/`reload()`… methods. Rejected. Async trait objects need
  either the `async-trait` crate or hand-rolled boxing, and a trait whose methods the domain awaits
  couples the core to adapter timing. Channels make the Port a data contract, adapters plain tasks,
  and the domain testable without any process. The channel vocabulary *is* the unified interface;
  making it complete is cheaper than replacing it.
- **A distinct `ProcessCommand::Reload` beside `ApplyConfig`.** Rejected. Nothing upstream ever
  asks for a bare reload — OpAMP drives configuration and packages, and its only process command
  is restart — so a separate reload command would have no sender. Reload is *how* an apply is
  executed, which is precisely the kind-specific knowledge the plugin owns.
- **Moving program resolution and package consent into the plugins.** Rejected: `AcceptsPackages`
  is fleet-visible, and a capability the Server acts on must derive from what the core decides
  (ADR-0018), never from per-kind behavior that could disagree with it.
- **Operator-written lifecycle hooks in TOML (`uninstall_cmd`, `reload_cmd` on every block).**
  Rejected. Kind knowledge belongs in the kind's module, written once — not re-invented in every
  operator's configuration, where a wrong hook silently corrupts a host. A key that *is* the
  mechanism is not acceptable; which mechanism keys remain at all is ADR-0037's rule.
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
  Gateway Mode generalizes.
- **Requiring at least one `[[supervisor]]` block.** Rejected. It would invalidate every deployment
  without blocks, the shipped default configuration, and the shutdown test, and buy nothing: the
  self-agent is the same state machine without a process handle.
- **Extension-config injection à la `opampsupervisor` (template + YAML merge).** Deferred. It
  requires a YAML stack and templating in Rust. Instead, the operator pins `endpoint_port` and the
  *distributed* Collector configuration carries the `opamp` extension block pointing at
  `ws://127.0.0.1:<port>/v1/opamp`. Injection is the natural follow-up once configuration
  templating is wanted for other reasons.

## Sources / Prior art

- [`opampsupervisor` specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the reference supervisor's architecture: local OpAMP server, config handling, restart and stop
  behaviour, noop-config bootstrap. It restarts on configuration change and on package
  replacement; it has no reload or uninstall, which is the gap the lifecycle vocabulary fills for
  process kinds beyond the Collector.
- [`opampsupervisor` embedded extension template](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/templates/opampextension.yaml)
  — `endpoint: "ws://127.0.0.1:{{.SupervisorPort}}/v1/opamp"`: the injected extension
  configuration is WebSocket-only on loopback, which justifies the WS-only Supervisor Endpoint.
- [`opampextension` README](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — the extension is a client only, supports ws and http transports scheme-selected, and reports
  effective configuration, health, and available components.
- [serde-rs/serde#1547](https://github.com/serde-rs/serde/issues/1547) — `deny_unknown_fields`
  does not compose with `#[serde(flatten)]`; motivates the two-stage `[[supervisor]]` parse.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `ServerToAgent.instance_uid`, the multiplexing provision this design rides on, already cited
  and bound by ADR-0003. The only process command is restart, and packages arrive via
  `PackagesAvailable`: upstream never speaks "reload" or "uninstall", confirming both as
  Client-side vocabulary, not protocol. Baseline `v0.18.0` (see [`CONFORMANCE.md`](../CONFORMANCE.md)).
- [Nomad task driver plugins](https://developer.hashicorp.com/nomad/docs/concepts/plugins/task-drivers)
  — the closest shape: one driver interface (`StartTask`, `StopTask`, `SignalTask`,
  `DestroyTask`) over arbitrary process kinds, with **stop** (halt the process) and **destroy**
  (clean up what running it created) as deliberately separate steps — the same separation
  `Shutdown` vs. `Uninstall` draws here.
- [systemd service units](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
  — reload (`ExecReload`, `reload-or-restart`) as first-class beside restart, with restart as
  the fallback when a unit exposes no reload path; the semantics clause 12 adopts for the apply
  strategy.
- [Puppet package providers](https://www.puppet.com/docs/puppet/7/types/package.html) — one
  resource type with `install`/`uninstall`/`update` implemented per provider (dpkg, rpm, msi, …):
  the established pattern of a uniform lifecycle interface whose mechanics live in the specific
  implementation.

## Consequences

- Positive: goals 1–8, 14, and 16 become reachable — many Supervisors per Client, a Foreign Agent
  managed indistinguishably from a Collector, a new process kind as one new module, and the
  extension-carrying Collector visible in the fleet through its own reporting. No Server change is
  needed; the tested `instance_uid` routing carries it.
- Positive: the hexagonal seam is real code — the domain core knows Ports, not process kinds —
  so Gateway Mode composes onto the same seam instead of forking the engine.
- Positive: at most one new dependency (`libc`, unix-only, for `SIGTERM`); the Supervisor Endpoint
  reuses `tokio-tungstenite` and the shared `opamp::frame` codec.
- Positive: each specific Supervisor is one module implementing one complete, uniform vocabulary —
  a kind with a native installer, a reload signal, or an API-applied configuration fits without
  touching the core (goal 8 stays cheap). Reload-capable agents keep their in-flight state across
  configuration changes. Retiring a Supervisor undoes what installing it did, instead of only
  deleting its directory.
- Negative / trade-offs: losing the single upstream connection affects every Agent riding it —
  accepted by ADR-0003; reconnection resends full state per Agent. Ephemeral endpoint ports cannot
  serve an extension-carrying Collector without injection: that path requires the operator to pin
  `endpoint_port` and put the extension block into the distributed configuration.
- Negative / trade-offs: `APPLIED` means "the process restarted with the new config", not "the
  process validated the config" — a Managed Process that starts and later chokes on its
  configuration surfaces as unhealthy, not as a rejected configuration.
- Negative / trade-offs: the vocabulary has two commands beyond start, stop and apply, but a kind
  that rides the shared `Runner` never sees them — only an adapter that replaces the `Runner`
  wholesale must handle them itself. A reload leaves less outside-observable evidence than a
  restart — the process never exits, so the health gate reads a process that may still run on the
  old configuration; an adapter that cannot verify its reload took effect must say so in the
  `ConfigApplied` outcome rather than acknowledge blindly. Kind-specific install steps mean the
  rollback guarantee of ADR-0015 is only as good as each kind's implementation of it — the shared
  helper keeps that honest for the swap case, new kinds carry the burden themselves.
- Follow-ups (by topic): extension-configuration injection/templating; plain-HTTP polling on the
  Supervisor Endpoint; making reload vs. restart visible upstream (both surface only as health); a
  verified-reload gate (asking the process which configuration it now runs before acknowledging);
  process kinds that install through OS package managers, within ADR-0018's rule that a Managed
  Process is a program this Client installs.
