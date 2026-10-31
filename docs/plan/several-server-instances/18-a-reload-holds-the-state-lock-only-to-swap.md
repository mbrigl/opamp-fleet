# Ticket: An instance reads the ledger again outside its state lock and holds it only to swap

## What to build

Each instance follows the other instances' acts on the trust state by reading the whole ledger
again, the register of up to 100 000 entries, the revocation list, the Hosts and their bindings,
and it holds its state lock for the whole read. Every TLS handshake's revocation check, every
`speaks_for` and every report waits on that lock. Failover is cheap, since a reconnecting Agent is
already bound and writes nothing, but an enrolment or renewal wave makes every other instance read
the ledger once per coalesced batch of notifications, and back-to-back reads can keep the lock
held most of the time.

The read stays correct only if no act of this instance commits while it runs: a read taken before
an own revocation commits and swapped in after it would drop the revocation, or bring back one the
instance just lifted. So the read goes outside the lock, beside a counter of this instance's own
acts on the trust state: under the lock the read is swapped in only when the counter has not moved
since the read began, else it is read again. The lock is held for the swap alone. The acts
themselves stay under the lock, as short transactions bounded by `lock_timeout` and
`statement_timeout`.

## Acceptance criteria

- [ ] Given a reload in progress, when a revocation check runs on the same instance, then it does
  not wait for the read, only for the swap.
- [ ] Given an own revocation committed while a reload reads, when the read ends, then the read is
  not swapped in and the revocation holds; the same for a lift, a binding and a register write.
- [ ] Given a test that holds the store's read open, when the instance is asked whether a
  certificate is revoked, then it answers within a bound the test names.
- [ ] Given the two-instance suite and the store checks, when the change lands, then they pass.

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
  clauses 9 and 19; ADR-draft-the-servers-state-and-its-stores clause 10.
- `crates/fleet-server/src/revocation.rs` `reload_ledger`; `crates/fleet-server/src/instance.rs`
  `follow_revocations`.
- Seam: the ledger port with a store whose `load` the test holds open.

## Blocked by

- docs/plan/several-server-instances/02-the-trust-state-is-written-per-row.md

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended. It changes when the state lock is
held on the path of every TLS handshake.
