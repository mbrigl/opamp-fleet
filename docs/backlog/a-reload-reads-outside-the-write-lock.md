# A reload reads its store outside the write lock

**Kind:** performance.

**Observed.** Three costs of a notice grow with the store, not with what changed:

- `ConfigStore::reload`, `DeploymentStore::reload` and `PackageStore::reload`
  (`crates/fleet-server/src/configs.rs` line 263, `deployments.rs` line 371, `packages.rs` line
  729) read the whole store while they hold its write lock, on every batch of notices. A rollout
  sends one `configuration` notice per pinned Configuration.
- `ConfigStore::retain_only` collects in the database under the Configurations' write lock
  (`configs.rs`, near lines 383 to 388).
- Every reload of the register copies the whole revocation list (`fresh.revocations.clone()`,
  `revocation.rs`, line 1992).

The three reloads read under the lock for a reason their doc comments give (`configs.rs`, near
lines 258 to 259, and the same words in `deployments.rs` and `packages.rs`): "so no write of this
instance's own lands between the read and the swap". `reload_ledger` (`revocation.rs`, near lines
1964 to 1993) already reads outside the lock and keeps that guarantee with a counter of its acts: a
swap is refused when an act landed meanwhile. Only its fallback, after `RELOAD_SWAPS_REFUSED`
refused swaps, reads under the lock.

A host or certificate notice has every instance read the whole register again, up to 100 000
entries.

**Idea.** Each of the three reloads reads outside the write lock and swaps, as `reload_ledger` does.
Any such change must keep the guarantee that no write of the instance's own is lost between the
read and the swap. `retain_only` collects outside the lock, and the list is shared rather than
copied. Reading the register by what a notice names contradicts SRV-instances-share-one-postgresql §12 ("Caches are
read again, not updated"), so that part needs a superseding ADR through `propose-adr` first.

**Not now.** The human deferred it on 2026-11-19: no criterion asks for it, and the G-20 load test
meets its target without it. It comes back when the timing of each kind of act
(`docs/backlog/each-operator-act-is-timed-in-the-g20-test.md`) or another measurement shows it.

**From.** Ticket 01 of `fleet-at-scale-costs`, the part the human did not keep in the ticket
(2026-11-15).
