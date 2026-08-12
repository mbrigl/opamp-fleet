# ADR-0056: The Client accepts its Supervisor set from the Server — the rest of `client.toml` stays the operator's

- **Status:** 🟡 proposed
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl

Completes, on the Client side, the delivery path ADR-0054 and ADR-0055 built on the Server side: a
typed, published Configuration can already reach the Client's own Agent
(`service.name = "opamp-fleet-client"`, ADR-0028/ADR-0033) — this ADR decides what the Client
*does* with it. It extends ADR-0011 (the `[[supervisor]]` blocks) and ADR-0008 (`client.toml` as
the configuration file) without changing either's shape.

## Context

The Client's own Agent declares `AcceptsRemoteConfig`, and today that declaration is hollow: a
`remote_config` offered to the self-Agent is stored and acknowledged `APPLIED`
([`agent.rs`](../../crates/client/src/supervisor/agent.rs#L851-L886), "storing *is* applying") —
and then changes nothing. The fleet view can now show the Client's own configuration and offer it
Configurations typed for it, but the Client answers every offer with a polite lie.

What the fleet actually needs to manage on a Client is *which Supervisors it runs*: the
`[[supervisor]]` blocks of `client.toml` (ADR-0011). Everything else in that file is host-local
trust and wiring — the Server endpoint, the credential, the state directory, the instance name.
That half must never be Server-writable: the configuration that tells the Client where the Server
is and how to authenticate cannot come from the Server without making a bad push unrecoverable
(the Client that applied it can no longer be told anything).

The specification's vocabulary already anticipates the shape of the answer: the effective
configuration "may differ from the remote configuration (it may **merge in local
configuration**, or have rejected the remote one)"
([`SPECIFICATION.md`](../SPECIFICATION.md), Fleet operations). The OpenTelemetry OpAMP
Supervisor does exactly this — remote configuration merged with local configuration files into
the one document the managed process runs.

Two mechanics constrain the design:

- **Supervisors are built once, at startup** ([`supervisor/mod.rs`](../../crates/client/src/supervisor/mod.rs)):
  the Engine's Agent set never changes while the Client runs, and the reconfigure path
  (`RunOutcome::Reconfigured`) deliberately carries the Engine and its Managed Processes across
  reconnects untouched. Applying a Supervisor change at runtime is new machinery.
- **`client.toml` is what the self-Agent reports as its effective configuration**
  ([`supervisor/mod.rs`](../../crates/client/src/supervisor/mod.rs#L80-L97)): the file is the
  truth the fleet view shows. Wherever a Server-delivered Supervisor set is persisted, that
  report must keep being true.

## Decision

We will make the Client apply a remote configuration **to its `[[supervisor]]` blocks only** —
compare, validate the merge, stop what left, write `client.toml`, start what arrived — and refuse
to let a remote configuration touch anything else in the file.

1. **Only the `[[supervisor]]` blocks of the offered document are read.** Each entry of the
   composed config map is parsed as TOML; the union of their `[[supervisor]]` blocks is the
   offered Supervisor set, and every other top-level key is **ignored** — the boundary is
   enforced by what the Client takes, not by policing what the Server sends. An operator may
   thus publish a full `client.toml`-shaped document (say, one copied from a reference host) and
   exactly its fleet-manageable half takes effect; what actually runs is verifiable in the fleet
   view, which shows the Client's own `client.toml`. A duplicate Supervisor `name` within or
   across entries still fails the offer (`FAILED`, with the reason) — that is not a foreign key
   but a genuine ambiguity inside the accepted scope.

2. **The merge is: local globals, offered Supervisors.** The new document is the current
   `client.toml` with its `[[supervisor]]` blocks replaced by the offered set — nothing else
   changes hands. The merged document is validated by the same loader that validates
   `client.toml` at startup (block schema, program-path resolution, ports, timeouts). A merge
   that fails validation is reported `FAILED`; nothing is stopped, nothing is written, the
   running configuration stays in force.

3. **Apply is a diff, keyed by Supervisor `name`.** Comparing the running blocks with the offered
   ones yields removed, changed (any key differs), added, and unchanged. Then, in order:
   the removed and changed Supervisors are **stopped** (their Managed Processes shut down, their
   Agents say `agent_disconnect`); the merged document is **written to `client.toml`**; the
   changed and added Supervisors are **started** from the file just written. Unchanged
   Supervisors keep running untouched — the point of managing the set from the Server is that a
   fleet-wide change to one Supervisor does not cycle its neighbours. A crash between the stop
   and the write restarts into the old file; one between the write and the start restarts into
   the new one — both converge, because startup builds exactly what the file says.

4. **`client.toml` remains the single truth.** No overlay, no second file: after an apply, the
   file *is* the configuration, the same file the operator reads and the self-Agent reports —
   the effective-configuration report is refreshed from the written file in the same step. A
   Client restarting offline starts the Server-delivered Supervisors, because they are in its
   file. The write preserves the operator's half literally — comments, ordering, formatting —
   by editing the TOML document surgically (`toml_edit`, the format-preserving parser cargo
   itself edits manifests with) rather than re-serializing it.

5. **The status lifecycle becomes honest.** The self-Agent acknowledges `APPLYING` on receipt,
   `APPLIED` once the file is written and the starts are issued, `FAILED` when parsing,
   validation, or the write fails. A started Supervisor whose process then crashes is a *health*
   fact, reported as such — the configuration was applied; the process is unhealthy. "Storing is
   applying" ends; the stored copy remains what a restart reports its hash from.

6. **No offer, no change.** A Client whose Server never publishes a Configuration typed
   `opamp-fleet-client` runs its locally written `[[supervisor]]` blocks exactly as today. The
   first applied offer replaces the local set — from then on the Server's set is authoritative
   for Supervisors on that Client, which is the point, and the reason the offer is compared
   against the *file*, not against the last offer.

## Alternatives considered

- **The Server delivers the whole `client.toml`** — rejected in Context: endpoint, credential,
  and state directory are the host's trust anchors; a Server that can rewrite them can cut a
  Client off with one bad push, and the Client has no path back. The Supervisor set is the
  fleet-shaped half of the file; the boundary runs exactly there.
- **A separate overlay file** (state-dir resident, merged at load — the OpAMP Supervisor's
  model, which keeps the last received remote configuration beside the local files). Rejected:
  two files would answer "what does this Client run", the effective-configuration report would
  have to merge them to stay truthful, and an offline restart would depend on state-dir
  internals the operator never sees. Writing the one documented file keeps the one truth.
- **Restart the Client (or all Supervisors) to apply** — the machinery exists
  (`Exit::RestartForUpdate`) and would avoid runtime Agent-set changes. Rejected: it cycles
  every healthy Supervisor to change one, which on a host running several collectors is exactly
  the disruption Selector-scoped rollouts exist to avoid.
- **Fail the offer on any non-Supervisor top-level key** instead of ignoring them. Rejected: it
  turns tolerable input into a fleet-visible error and makes the offer format needlessly rigid —
  a document with one stray global key would apply nothing, though what to do with it is
  unambiguous. The risk of ignoring — an operator believing a pushed global key took effect —
  is answered by the fleet view showing the file that actually runs (point 4), not by refusing
  the Supervisors that came with it.
- **Re-serialize `client.toml`** with the ordinary `toml` writer instead of adding `toml_edit`.
  Rejected: it destroys the operator's comments and layout in the one file this project
  documents *as* commented prose (the shipped example config) — the file would stop being the
  operator's after the first apply. The dependency is the price of point 4's "remains the
  operator's file"; cargo's own manifest editing is the precedent that it is fit for this.

## Sources / Prior art

- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `EffectiveConfig` is explicitly allowed to differ from the offered remote configuration by
  merging local configuration; `RemoteConfigStatuses` (`APPLYING`/`APPLIED`/`FAILED`) is the
  lifecycle point 5 adopts.
- [OpenTelemetry OpAMP Supervisor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md)
  and its [specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  (checked 2026-08-12) — the established remote-plus-local merge, with explicit precedence
  control (`agent.config_files` placeholders). Taken: the merge and the local-wins boundary for
  host-local settings. Not taken: the separate storage of the remote part (see Alternatives).
- [`toml_edit`](https://docs.rs/toml_edit) (checked 2026-08-12) — format- and comment-preserving
  TOML editing; what `cargo add`/`cargo-edit` mutate manifests with.
- In-repo: ADR-0011 (the `[[supervisor]]` blocks), ADR-0012/ADR-0054/ADR-0055 (the Server-side
  delivery path this completes), ADR-0020 (the self-Agent), ADR-0014 (the pattern of a verified,
  persisted Server offer changing what the Client runs).

## Consequences

- Positive: Supervisors become fleet-manageable end to end — an operator publishes a typed
  Configuration and the matching Clients converge on the named Supervisor set, with the same
  draft/publish gate and Selector scoping every other Configuration has (ADR-0054, ADR-0055).
  The self-Agent's `AcceptsRemoteConfig` stops being a lie.
- Positive: unchanged Supervisors ride through an apply untouched; a fleet-wide change to one
  collector type does not restart the others.
- Negative / trade-offs: the Engine must add and remove Agents at runtime — new machinery where
  the set was startup-fixed. A removed Supervisor's Agent must disconnect cleanly, an added one
  must introduce itself mid-connection, and the event channel's index-keyed routing must survive
  the mutation. This is the substantial implementation cost of the decision.
- Negative / trade-offs: a new dependency (`toml_edit`) in the Client.
- Negative / trade-offs: after the first applied offer, a *local* edit to the `[[supervisor]]`
  blocks drifts silently: the Client reports the offer's hash as applied, so the Server —
  whose composed map is unchanged — never re-offers, and the local edit stands until the next
  publication overwrites it. Reconciling file-vs-offer at startup is a follow-up, not part of
  this decision.
- Follow-ups: startup reconciliation of a locally edited Supervisor set against the last stored
  offer; a bundled-UI affordance for authoring Supervisor-set Configurations (the boundary of
  point 1 suggests a dedicated editor rather than a free-text body); the reserved `SIGHUP`
  configuration reload ([`runtime.rs`](../../crates/client/src/service/runtime.rs#L329-L338))
  could reuse point 3's diff-apply for local edits.
