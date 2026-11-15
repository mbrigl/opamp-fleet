# A reload reads its store outside the write lock

**Kind:** performance.

**Observed.** `ConfigStore::reload`, `DeploymentStore::reload` and `PackageStore::reload`
(`crates/fleet-server/src/configs.rs` line 263, `deployments.rs` line 371, `packages.rs` line 729)
read the whole store while they hold its write lock, on every batch of notices. A rollout sends one
`configuration` notice per pinned Configuration. `reload_ledger` (`revocation.rs`, near lines 1964
to 1993) already reads outside the lock and swaps. Only its fallback, after `RELOAD_SWAPS_REFUSED`
refused swaps, reads under the lock.

A host or certificate notice has every instance read the whole register again, up to 100 000
entries.

**Idea.** Each of the three reloads reads outside the write lock and swaps, as `reload_ledger`
does. Reading the register by what a notice names contradicts ADR-0045 clause 12 ("Caches are read
again, not updated"), so that part needs a superseding ADR through `propose-adr` first.

**From.** Ticket 01 of `fleet-at-scale-costs`, the part the human did not keep in the ticket
(2026-11-15).
