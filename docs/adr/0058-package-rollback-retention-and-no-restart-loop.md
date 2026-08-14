# ADR-0058: A failed package apply rolls back only to a predecessor, never loops, and a superseded version is kept for a grace period before deletion

- **Status:** 🟢 accepted
- **Date:** 2026-08-13
- **Deciders:** Markus Brigl

## Context

[ADR-0015](0015-package-delivery-for-managed-processes.md) makes a Managed Process's package
install **health-gated**: the Supervisor swaps the new program over the old, restarts, and if the
process does not survive `apply_grace_secs` the swap is **rolled back**. [ADR-0023](0023-multi-file-packages.md)
extends the same gate to a whole tree. The mechanism lives in `crates/client/src/supervisor/process.rs`
(`InstallTarget` + `Runner::swap_and_gate`): `set_aside` renames the live program to a `.rollback`
sibling, `install` writes the new one, the process is restarted and gated, and then:

- **fail + a predecessor exists** → `restore()` renames the `.rollback` back over live;
- **fail + no predecessor** (a first install onto an empty `program/`) → `discard()` removes the
  just-installed program, leaving `program/` empty;
- **success** → `drop_backup()` deletes the `.rollback` **immediately**.

Operating this against a real Collector surfaced three gaps, each a way the system harms itself:

1. **The discard-on-first-install case loops.** A first package that installs but cannot survive the
   grace (a port already bound, an empty config, any crash-on-start) is discarded, `program/` goes
   empty, the Server sees `installed hash ≠ desired` and re-offers, the Client re-downloads and
   re-unpacks (hundreds of megabytes each round), it crashes again — indefinitely. Observed live:
   the same 0.157.0 artifact downloaded and unpacked once per second.
2. **A rolled-back predecessor that also fails to start loops.** After a rollback the restored old
   program is respawned by the Runner, which retries **forever** on a capped backoff and never gives
   up (`Runner::run`, the `exited` arm). If both the new and the old version are broken, the
   Supervisor spins.
3. **The predecessor is deleted the instant the new one is up.** `drop_backup()` runs on success, so
   there is no window in which an operator (or an automatic later health signal) can fall back to the
   version that was running an hour ago — the moment a new version survives its first three seconds,
   the only copy of its predecessor is gone.

None of these is a new feature request in disguise; they are the rollback lifecycle not being
finished. The forces:

- **Health-gating is the right primitive, but "survives the grace" is a *first* signal, not a
  *final* one.** A Collector can pass three seconds and still be the wrong version — a slow leak, a
  dropped exporter. Keeping the predecessor briefly turns a bad rollout into a one-line fix instead
  of a re-delivery.
- **A retry that never gives up is a denial of service against the fleet's own Server.** The
  Client's *self*-update already draws this line — it rolls back after **three** failed attempts
  (ADR-0020) rather than restarting forever. The Managed-Process path should be no less disciplined.
- **A first install has nothing to roll back to.** Discarding what was just written is not a
  rollback; it is throwing away the only artifact the Server has, which is exactly what makes the
  re-offer loop turn. Leaving it in place — reported `InstallFailed` — stops the churn and keeps the
  bytes that were already verified.
- **Retention is a policy, and policy is the operator's.** How long a superseded version is worth
  keeping depends on the host (disk) and the rollout (risk), so it must be configurable — a global
  default with a per-Supervisor override, the shape `apply_grace_secs` already has.

## Decision

We will finish the package-apply lifecycle so that it **rolls back only to a real predecessor,
never loops, and retains a superseded version for a configurable grace period before deleting it.**

1. **Rollback needs a predecessor (points 1 + the first-install case).** A failed apply with a
   `.rollback` present restores it, unchanged from today. A failed apply with **no** predecessor
   performs **no rollback**: the just-installed (and content-verified) program is **left in place**,
   the package is reported `InstallFailed` with the reason, and nothing is discarded. `program/` is
   never emptied by a failure, so the "installed ≠ desired, re-offer, re-download" loop cannot start.

2. **A failed apply is terminal for that package hash — no restart loop (points 1 + 2).** After a
   failed apply (whether it rolled back or not), the Supervisor does **not** respawn the Managed
   Process in a tight loop. It reports the failure as health and `InstallFailed` and then **waits**
   for a state change — a new configuration, a different package hash, or an operator restart —
   rather than retrying the same broken artifact. In particular a rolled-back predecessor that then
   also fails to stay up is reported unhealthy and **not** restarted again. The Server's own gate
   (it does not re-offer a hash the Agent reported `InstallFailed`) is the matching half; this
   decision is the Client half. The give-up threshold mirrors the self-update's **three** attempts
   (ADR-0020) so the two update paths behave alike.

3. **A superseded version is kept for a grace period, then deleted (point 3).** On a **successful**
   apply the predecessor is **not** deleted immediately. The `.rollback` is retained and a persisted
   marker records the deadline (`applied_at + retain_previous`); a cleanup pass on startup and on a
   periodic tick deletes any `.rollback` past its deadline. The period is configured by a new global
   `[updates] retain_previous_secs` (default **86400**, one day) with a per-Supervisor override in
   the `[[supervisor]]` block, exactly as `apply_grace_secs` is global-with-override today. A
   subsequent update within the window supersedes the marker (each Supervisor keeps at most one
   predecessor — the immediately previous version). `0` restores today's behaviour: delete on
   success.

The persisted marker lives in the Supervisor's own directory (ADR-0010/0021), so the deadline
survives a Client restart the way the self-update outcome marker does. Deletion is best-effort and
logged; a marker whose `.rollback` is already gone is simply cleared.

## Alternatives considered

- **Leave it as is (immediate delete, infinite retry, discard-on-first-install).** Rejected: the
  three behaviours above were each observed to harm a running deployment — an unbounded re-download
  loop, a spin between two broken versions, and no rollback window at all.
- **Keep every superseded version, not just the last.** Rejected: it turns `program/` into an
  unbounded version store on a host chosen for its disk budget, and the Managed-Process rollback is
  a *one-step* safety net, not the versioned layout the Client's own self-update keeps (ADR-0010).
  One predecessor is what a rollback needs.
- **Give up after one failed apply rather than three.** Rejected for consistency: the self-update
  path already settled on three (ADR-0020), and a single transient failure (a briefly-held port)
  should not permanently strand a rollout.
- **Retention as time-to-live on the artifact store rather than the `.rollback`.** Rejected: the
  predecessor a rollback needs is the *installed* program, not the downloaded artifact; tying
  retention to the staged download would delete the very thing a fallback restores.

## Sources / Prior art

- This project's own [ADR-0020](0020-client-self-update.md) (the Client self-update: staged version,
  three-attempt give-up, marker across restart) is the precedent this aligns the Managed-Process
  path to.
- The versioned-layout-with-pointer model of [ADR-0010](0010-client-os-service-and-cli.md) is the
  contrast that justifies keeping only *one* predecessor here rather than a full version store.

## Consequences

- Positive: the two loops end. A first package that will not start stays installed and
  `InstallFailed` instead of triggering an endless re-download; a pair of broken versions is
  reported, not spun on. Token, bandwidth, and disk churn against the Server stop.
- Positive: a rollout gains a real fallback window — for `retain_previous_secs` after a successful
  update, the previous version is still on disk and an operator can put it back.
- Positive: the Managed-Process update path and the Client self-update path now behave alike
  (three-attempt give-up, marker across restart), which is one rule to reason about instead of two.
- Negative / trade-offs: a superseded version now occupies disk for up to a day by default (one
  extra program or tree per Supervisor). The per-Supervisor override and `0` exist for hosts that
  cannot spare it. A persisted marker and a periodic cleanup tick are new moving parts on the
  Managed-Process side.
- Follow-ups: the exact config key names and the cleanup-tick interval are settled in
  implementation; a regression test per behaviour (no-predecessor keeps the binary and does not
  loop; a twice-failing pair is reported and not respawned; a retained predecessor is deleted only
  after its deadline and survives a restart until then).
