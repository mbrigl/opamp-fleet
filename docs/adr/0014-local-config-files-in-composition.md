# ADR-0014: Local regular configuration files in the config composition, restored after reconnect

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The upstream Go supervisor supports `config_files`: operator-provided **regular** configuration files
that are part of the Collector's configuration and are **restored after the OpAMP server reconnects** —
they are always present in the effective config, not just before the Server first answers.

Our Collector Supervisor composes the effective config as **base → remote → own-telemetry**
([ADR-0008](0008-collector-supervisor-go-reference-compat.md), [ADR-0010](0010-collector-supervisor-own-telemetry.md)),
where `base_config` is a single local file merged underneath the remote config, plus a **startup
fallback** list that runs *only before the Server answers* (an
[ADR-0008](0008-collector-supervisor-go-reference-compat.md) follow-up).
There is no notion of operator-provided **regular** local files that are a *permanent* layer of the
composition, on the same footing as the Go supervisor's `config_files`. `base_config` is the closest
thing but is a single file; the fallback is pre-Server only.

This is architecture-relevant because it adds a layer to the config-composition model and changes what
the effective config is composed from.

## Decision

We will add `config_files` — an **ordered list of local files that generalises `base_config`** — as a
permanent layer of the config composition, and treat it as the operator's regular local configuration.

- **Composition order becomes `config_files → remote → own-telemetry`.** The `config_files` are
  deep-merged in order (later files win), then the remote config is merged on top (remote wins over local),
  then own-telemetry (ADR-0010). This is exactly how `base_config` already layers under remote, extended
  from one file to an ordered list.
- **`base_config` is folded into this.** `base_config: X` becomes sugar for `config_files: [X]`; both are
  accepted (config precedence: if both are given, `base_config` is the first entry), so existing
  configurations keep working.
- **Always present, restored after reconnect.** Because `config_files` are a permanent layer, the
  supervisor re-composes from them whenever it applies a config — including on a reconnect where the
  Server has not (yet) re-sent a remote config. The persisted "last applied config"
  ([ADR-0008](0008-collector-supervisor-go-reference-compat.md)) already survives a supervisor restart;
  this ADR makes the *local* layer authoritative so a Server that reconnects without immediately
  re-pushing does not strand the Collector on a stale composition.
- **Distinct from the startup fallback.** The fallback list runs only until the Server first answers and
  is then superseded; `config_files` are a permanent layer that the remote config *merges with*, not
  *replaces*. The two stay separate config keys with separate meanings.

## Alternatives considered

- **Keep only `base_config` (single file).** Rejected: it cannot express the ordered multi-file local
  configuration `config_files` provides, which operators use to split concerns (a receivers file, a
  pipelines file). Generalising to a list is the whole point.
- **Reuse the startup-fallback list as `config_files`.** Rejected: they have different lifetimes — the
  fallback is replaced once the Server answers, `config_files` persist and merge with remote. Conflating
  them would drop the operator's local config the moment a remote config arrives.
- **Make `config_files` win over remote (local overrides server).** Rejected: it inverts the fleet-control
  model — the Server is the source of truth and remote config must win; `config_files` are the *base* the
  Server layers on, matching `base_config` and the Go supervisor.

## Sources / Prior art

- The upstream OpAMP Supervisor's `config_files` (regular files restored after reconnect):
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>.
- The composition model this extends: [ADR-0008](0008-collector-supervisor-go-reference-compat.md)
  (`base_config`, file-plus-restart, persisted last config) and
  [ADR-0010](0010-collector-supervisor-own-telemetry.md) (own-telemetry on top).

## Consequences

- Positive: closes a named Go-reference parity gap; operators can pin an ordered set of local files the
  fleet's config layers on, and a reconnect never strands the Collector on a composition missing them.
- Negative / trade-offs: one more input to the merge, so the composition and its tests grow; the
  `base_config`↔`config_files` equivalence must be documented so operators are not surprised that both
  exist.
- Follow-ups: none required; a future ADR could let `config_files` be watched for local edits (hot
  reload) rather than read once, if operators want that.
