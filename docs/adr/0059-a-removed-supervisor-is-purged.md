# ADR-0059: A removed Supervisor is purged — its Managed Process stops and its directory goes with it

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

When a Supervisor leaves the set — the ADR-0056 apply diffs the offered `[[supervisor]]` blocks
against the running ones and retires what left — the Client stops its Managed Process and sends the
Agent's `agent_disconnect` ([`engine.rs`](../../crates/client/src/engine.rs#L636-L662)). What it
does **not** do is touch the disk: the whole per-Supervisor directory of ADR-0021 stays behind —

```
<supervisor_dir>/<name>/
  instance-uid            # the Agent identity that just said goodbye
  remote-config.pb        # its last received configuration
  installed-package.json  # what was installed
  config/                 # the written configuration entries
  program/                # the Client-owned binary — a Collector is hundreds of megabytes
  packages/               # download staging
```

Nothing ever reclaims it. There is no cleanup keyed to a retired Supervisor anywhere in the Client
(the only `remove_dir_all` calls are package-staging housekeeping), and no startup pass over
`supervisors_root()` looks for directories no block names. The e2e test pins exactly this contract:
after a removal, the goodbye arrives and the file no longer names the block — the directory is not
mentioned ([`e2e.rs`](../../crates/client/tests/e2e.rs#L465-L495)).

The leftovers are not just disk waste, though on a host whose fleet role changes a few times they
are that too (one Collector binary per removed Supervisor). They are **stale identity and stale
trust**: a Supervisor later re-added under the same name silently inherits the old `instance-uid` —
an Agent that formally disconnected resurrects with its predecessor's identity and history — plus
the old remote configuration and whatever program version the old install left, instead of starting
as the new Agent it is. ADR-0021 already flagged the shape of this problem for a *moved* root
("changing `supervisor_dir` on a running host leaves the old tree behind — `instance-uid`
included") and left it to the operator; a *Server-driven removal* (ADR-0056) has no operator on the
host, so nobody is there to clean up.

Constraints: ADR-0021 (the directory layout, and the ownership rule — a bare program name is
Client-owned, an absolute path is the machine's file the Client never writes), ADR-0056 (the apply
order: stop, write `client.toml`, start; a failed write restarts the stopped Supervisors from the
old file), ADR-0058 (retention applies to a *superseded package version* of a living Supervisor —
not to a Supervisor that no longer exists).

## Decision

We will make the ADR-0056 apply **delete a removed Supervisor's directory** after its Agent has
retired and the new `client.toml` is written: stop the Managed Process, say the goodbye, and then
remove `<supervisor_dir>/<name>/` recursively — program, packages, configuration, and identity.

1. **Removed means removed, not changed.** The purge applies only to names absent from the new
   set. A *changed* block is stopped and restarted by the same apply (ADR-0056 point 3) and keeps
   its directory — identity, program, and installed package ride through a change exactly as they
   do today.

2. **The purge comes after the write, and only after it succeeds.** The apply order stays
   stop → write → start, with the purge between write and start. A write that fails restarts the
   stopped Supervisors from the still-standing old file (ADR-0056) — their directories must still
   be whole, so nothing is deleted before the file says the Supervisor is gone.

3. **The Client deletes only what it owns.** The per-Supervisor directory is Client-created state
   and is removed whole — including `program/` when the block named a bare program, which is the
   Client-owned binary (ADR-0021). A block whose program was an absolute path keeps that rule's
   promise in the other direction: the machine's binary lies outside the directory and is never
   touched; only the Supervisor's state directory goes.

4. **A directory the apply cannot delete fails nothing.** The Supervisor is already stopped, the
   file already written; a purge error (a file held open on Windows, permissions) is a warning
   naming the path, not a `FAILED` apply — the set the Server asked for *is* running. The
   directory becomes an orphan (point 5).

5. **An orphaned directory is reported, not reaped.** At startup, a directory under
   `supervisors_root()` that no `[[supervisor]]` block names is logged as a warning with its path —
   whether it survived a purge error, a crash between write and purge, or a block removed from
   `client.toml` by hand while the Client was down. It is **not** deleted: a hand edit is an
   operator's act on the operator's file (ADR-0056's boundary), and a block temporarily commented
   out must not cost the Agent its identity and program. The log line makes the leftover visible;
   removing it stays the operator's call.

## Alternatives considered

- **Keep the data (status quo)** — rejected: it leaks a program-sized directory per removal, and a
  re-added Supervisor of the same name resurrects a disconnected Agent's identity and stale
  configuration instead of starting fresh. The Server-driven removal path has no operator on the
  host to clean up after it.
- **Keep the identity, delete the rest** (preserve `instance-uid`, purge program and state) —
  rejected: half-measures split the directory into kept and deleted parts nobody can reason about,
  and a removed Supervisor's identity *should* end — its Agent said `agent_disconnect`; the
  Baseline's goodbye is meaningless if the same identity reconnects later as something else.
- **A retention window before deletion**, like ADR-0058's `retain_previous_secs` — rejected:
  that retention exists so a *living* Supervisor can roll back to its previous version; a removed
  Supervisor has no process left to roll back. Re-adding it is a fresh install by the same
  delivery path that installed it the first time. A grace period would be machinery for an undo
  nobody has asked for, against Simplicity first.
- **Rename aside instead of deleting** (`<name>.removed-<timestamp>/`) — rejected: it converts a
  leak into a slower leak and hands the operator a janitorial duty the removal was supposed to
  perform.
- **Also reap orphaned directories at startup** — rejected as the default (point 5 logs instead):
  startup cannot tell a Server-driven removal's leftover from an operator's deliberate or
  temporary hand edit, and the destructive reading of that ambiguity deletes an identity and a
  program that were not meant to go. If the log line proves insufficient, a follow-up can revisit
  reaping with an explicit opt-in.

## Sources / Prior art

- **Debian/apt: `remove` vs `purge`** — the established distinction between stopping/removing a
  thing and purging its configuration and state; this ADR chooses purge semantics for a
  Server-driven removal precisely because no operator is present to run the second step.
  <https://www.debian.org/doc/manuals/debian-faq/uptodate.en.html>
- **Bindplane collector uninstall** — removes the collector's install directory and state as one
  act; the managed-fleet precedent that removal means the files go.
  <https://docs.bindplane.com/deployment/virtual-machine/collector/install-and-uninstall-bindplane-collectors>
- **OpAMP specification `v0.20.0`** — `agent_disconnect` as the Agent's final word; an identity
  that has said it should not silently return (the reasoning behind deleting `instance-uid`).
  <https://github.com/open-telemetry/opamp-spec/blob/v0.20.0/specification.md>
- In-repo: ADR-0021 (the directory and its ownership rule, and the flagged leftover-tree problem),
  ADR-0056 (the apply whose removal branch this completes), ADR-0058 (retention for the living,
  contrasted deliberately).

## Consequences

- Positive: a removal is complete — no program-sized leftovers, no stale identity, no stale
  configuration. Re-adding a Supervisor under the same name is a genuinely fresh Agent, installed
  by the same package path as any new one.
- Positive: the ADR-0021 leftover problem shrinks to the cases an operator causes by hand (moved
  root, offline edit), and those become visible in the log instead of silent.
- Negative / trade-offs: **removal becomes destructive and final.** A Supervisor removed by a
  mis-scoped Selector loses its identity, its Server-side history continuity, and its locally
  installed program; re-adding restores service, not history. This is accepted as the honest
  meaning of "removed" — the alternative (stale resurrection) is worse, and rollout scoping is
  the Server-side gate for it.
- Negative / trade-offs: a crash between the `client.toml` write and the purge, or a purge error,
  leaves an orphan directory that startup only reports — a human closes that loop.
- Negative / trade-offs: the e2e removal contract widens — the existing test pins goodbye and
  file; it must additionally pin "directory gone" (and "directory kept" for a changed block).
- Follow-ups: whether the startup report of orphaned directories should grow an explicit,
  opt-in reap; whether `service uninstall` (which today deliberately keeps all state) should
  offer a purge of the whole `supervisors_root()` by the same rule.
