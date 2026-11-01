# Ticket: The newest connection takes an Agent, and a Gateway keeps its Agents

## What to build

When an instance begins to carry an Agent whose holder is another instance with a live lease, it
takes it by a conditional update outside the batch and announces it; the other stops carrying it. An
instance drops an Agent only when the row no longer names it. An Agent whose holder changes more
than ten times in a minute is logged as a likely duplicate. A Gateway keeps each downstream Agent on
one pool connection while it lives; when the instance behind a pool connection is lost, its Agents
re-home once each at their next report, spread over one heartbeat interval, and the Gateway's cached
revocation list and floors are served on. A presence batch, and the write at a connecting report,
name their holder unconditionally for every connected Agent they carry, so a batch that was due
while another instance took the Agent can undo that take; the batch becomes conditional too, naming
itself only where the row names it or no instance with a live lease, and a test holds it. The
batch's disconnect guard, `a.last_seen_ms <= p.last_seen_ms`, compares this instance's clock with
a value another instance's clock may have pushed forward: with clock skew a genuine disconnect is
not written yet counted as written, so the row stays connected under the old holder until its lease
lapses or the Agent goes silent; the guard is settled here, by a monotonic per-row counter or by
recording why it may stay.

## Acceptance criteria

- [ ] Given an Agent carried by A, when its connection moves to B, then B holds it, A stops carrying
  it, and neither drops it twice
- [ ] Given an Agent B took from A, when a presence batch of A that was due before the take is
  written after it, then the row still names B as the holder
- [ ] Given an Agent whose holder changes eleven times within a minute, when the eleventh change
  lands, then one line is logged naming it as a likely duplicate
- [ ] Given a Gateway whose pool connections live, when its downstream Agents report, then their
  holder does not change
- [ ] Given a Gateway with pool connections on A and B, when A is stopped, then each downstream
  Agent of A is taken over once, and the Gateway's pool is not refused by B's throttle
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  8, 14, 15 and 21
- ADR-0015 clauses 7 and 8 for the pool, ADR-0035 for a Gateway's own limits
- Golden path: the duplicate detection and rekeying within one instance, and the Gateway end-to-end
  tests
- Seam: two instances and one Gateway in one test process

## Blocked by

- docs/plan/several-server-instances/04-the-fleet-record-is-written-per-column.md
- docs/plan/several-server-instances/05-every-change-reaches-every-instance.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It decides which instance serves an Agent, and duplicate detection
across instances.
