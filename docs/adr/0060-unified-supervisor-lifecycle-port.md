# ADR-0060: One lifecycle vocabulary for every Supervisor — install, uninstall, start, stop, update, reload, and configuration handled by the specific plugin

- **Status:** 🟡 proposed
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

More specific Supervisors are coming: process kinds beyond the shipped `collector` and `command`,
each with its own way of being installed, configured, reloaded, and removed. What they need is a
**uniform interface** that every kind implements, so that the supervision core drives any of them
identically while the kind-specific mechanics — installation, uninstallation, start, stop, update,
reload, configuration handling — live in the specific implementation.

Much of that interface already exists. ADR-0011 bound the Managed-Process-facing Port as a message
pair — `ProcessCommand` in, `ProcessEvent` out — plus the `Plugin` factory trait and the
compiled-in registry; a new kind is one module and one registry line (goal 8). Start, stop,
restart, and configuration apply already run inside the specific adapter task, with the shared
`Runner` (spawn, watchdog, backoff, bounded stop) as the implementation both shipped plugins use.

But the vocabulary is incomplete, and two operations sit in the core that the coming kinds need to
own:

1. **There is no reload.** `ApplyConfig` means "restart on the new files" — the only apply
   strategy the Port can express. Agents that reload their configuration in place (Fluent Bit and
   many daemons on `SIGHUP`, others through an admin API) are needlessly restarted, losing
   in-flight state and buffers on every configuration change. The service managers this project
   already integrates with (ADR-0010) all treat reload as first-class beside restart: systemd's
   `ExecReload`/`reload-or-restart` is exactly this distinction.
2. **Install and update mechanics are fixed by the core.** `ApplyPackage` hands the adapter a
   verified artifact, but what "install" means is hard-coded in `InstallTarget` — a file or tree
   swap (ADR-0015, ADR-0023). A kind whose program is not a swappable file — one installed
   through a native installer or an OS package manager — cannot express its install step at all.
3. **Uninstall bypasses the plugin entirely.** ADR-0059 purges a removed Supervisor: the core
   stops the process and deletes the directory. A kind whose installation had side effects
   outside its directory (a registered service, package-manager state, created users) has no
   hook to undo them — the purge leaves them behind.

Three forces constrain the shape. ADR-0011 deliberately made the Port channels, not an async
trait — object safety without `async-trait`, adapters as plain tasks, a domain core free of
process handles — and nothing here invalidates that reasoning. ADR-0021 derives the
fleet-visible `AcceptsPackages` capability from the written shape of the program path in the
core; a plugin that decided installation for itself could disagree with the declared consent.
And ADR-0008's strict parsing means any new per-kind setting must fail loudly on a typo.

## Decision

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
  nothing is a valid, complete Supervisor — it behaves exactly like the `command` kind today.
- **The Port stays a message pair.** The uniform interface *is* the command/event vocabulary of
  ADR-0011, now complete — not a new trait with lifecycle methods. Start (spawn on adapter
  startup), stop (`Shutdown`), and restart (`Restart`) are already the adapter's; they stay
  unchanged.
- **Reload is an apply strategy, not a new operation.** OpAMP has no reload command — a reload
  only ever happens *because* a configuration arrived — so `ApplyConfig` remains the single
  configuration operation, and the adapter chooses how to apply it: restart (the default) or a
  kind-specific reload declared in the block's plugin settings (for the `command` kind a
  `reload_signal`, unix-only and rejected by the strict parse on Windows; a future kind may
  reload through an API call). The semantics are systemd's `reload-or-restart`: a reload that
  fails — the mechanism errors, or the process dies — falls back to a restart on the new files.
  The health-gated acknowledgement (`apply_grace`, ADR-0011) applies to either path.
- **Install and update mechanics move behind the plugin.** `ApplyPackage` stays the operation and
  keeps its contract (verified artifact in, health-gated outcome out, rollback on failure —
  ADR-0015, ADR-0058). The swap-and-gate on `InstallTarget` becomes the shared default helper an
  adapter calls; a kind whose installation is not a file swap implements the step itself, inside
  the same contract. Program-path resolution and the package consent derived from it stay in the
  core — ADR-0021 is untouched.
- **Uninstall joins the vocabulary.** A new `ProcessCommand::Uninstall`, answered by a new
  `ProcessEvent::Uninstalled(Result)`, is sent when a Supervisor is retired (ADR-0056) before the
  core purges its directory (ADR-0059): the adapter stops its process and undoes whatever its
  installs did outside the directory; the default is exactly the graceful stop of today. The
  purge itself — deleting the directory — stays the core's, and stays bounded: an adapter that
  does not answer within the stop budget is treated as stopped and the purge proceeds, so a
  hanging uninstall cannot block retirement. This extends ADR-0059; it does not replace it.
- **Configuration management is split by ownership: the core persists, the adapter delivers.**
  Receiving, validating, persisting, and status-reporting a remote configuration stay in the
  core — they are OpAMP mechanics, identical for every kind. Everything between the written
  files and the running process — pointing the process at them, merging, choosing restart or
  reload, applying through an API — is the specific implementation's.
- **The registry and the two-stage parse are unchanged.** A new process kind remains one module
  and one registry line; its settings — reload mechanism included — parse strictly in the second
  stage.

## Alternatives considered

- **A new `Supervisor` trait with async `install()`/`start()`/`stop()`/`reload()`… methods.**
  Rejected. It re-opens what ADR-0011 already decided against: async trait objects need
  `async-trait` or hand-rolled boxing, and a core that awaits adapter methods couples itself to
  adapter timing. The channel vocabulary *is* the unified interface; making it complete is
  cheaper than replacing it.
- **A distinct `ProcessCommand::Reload` beside `ApplyConfig`.** Rejected. Nothing upstream ever
  asks for a bare reload — OpAMP drives configuration and packages, and its only process command
  is restart — so a separate reload command would have no sender. Reload is *how* an apply is
  executed, which is precisely the kind-specific knowledge the plugin owns.
- **Moving program resolution and package consent into the plugins.** Rejected, as when ADR-0021
  bound it: `AcceptsPackages` is fleet-visible, and a capability the Server acts on must derive
  from configuration the core reads, never from per-kind behavior that could disagree with it.
- **Operator-written lifecycle hooks in TOML (`uninstall_cmd`, `reload_cmd` on every block).**
  Rejected. Kind knowledge belongs in the kind's module, written once — not re-invented in every
  operator's configuration, where a wrong hook silently corrupts a host. A settings key that
  *selects* a mechanism the plugin implements (`reload_signal`) is fine; a key that *is* the
  mechanism is not.
- **Dynamic plugin loading.** Still rejected for the reasons of ADR-0011 — nothing here changes
  the ABI calculus.

## Sources / Prior art

- [Nomad task driver plugins](https://developer.hashicorp.com/nomad/docs/concepts/plugins/task-drivers)
  — the closest shape: one driver interface (`StartTask`, `StopTask`, `SignalTask`,
  `DestroyTask`) over arbitrary process kinds, with **stop** (halt the process) and **destroy**
  (clean up what running it created) as deliberately separate steps — the same separation
  `Shutdown` vs. `Uninstall` draws here.
- [systemd service units](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html)
  — reload (`ExecReload`, `reload-or-restart`) as first-class beside restart, with restart as
  the fallback when a unit exposes no reload path; the semantics this ADR adopts for the apply
  strategy.
- [Puppet package providers](https://www.puppet.com/docs/puppet/7/types/package.html) — one
  resource type with `install`/`uninstall`/`update` implemented per provider (dpkg, rpm, msi, …):
  the established pattern of a uniform lifecycle interface whose mechanics live in the specific
  implementation.
- [`opampsupervisor` specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the reference supervisor (already the model of ADR-0011) restarts on configuration change
  and on package replacement; it has no reload or uninstall, which is the gap this ADR fills for
  process kinds beyond the Collector.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — the only process command is restart, and packages arrive via `PackagesAvailable`: upstream
  never speaks "reload" or "uninstall", confirming both as Client-side vocabulary, not protocol.

## Consequences

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
  rollback guarantee of ADR-0058 is only as good as each kind's implementation of it — the
  shared helper keeps that honest for the swap case, new kinds carry the burden themselves.
- Follow-ups (by topic): making reload vs. restart visible upstream (today both surface only as
  health); a verified-reload gate (asking the process which configuration it now runs before
  acknowledging); process kinds that install through OS package managers and what package
  consent (ADR-0021) means for them.
