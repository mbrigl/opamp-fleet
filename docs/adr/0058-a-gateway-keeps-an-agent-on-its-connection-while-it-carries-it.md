# ADR-0058: A Gateway keeps an Agent on its upstream connection while it carries the Agent, and releases its place when it stops

- **Status:** 🟡 proposed
- **Date:** 2026-10-20
- **Deciders:** Markus Brigl
- **Applies to:** the assignment of Agents to upstream connections and its release in `crates/fleet-agent/src/gateway/pool.rs`, the callers that release a place in `crates/fleet-agent/src/gateway/mod.rs`, and the bindings that end one in `crates/fleet-agent/src/gateway/policy.rs` and `crates/fleet-agent/src/gateway/cache.rs`
- **Supersedes:** [ADR-0015](0015-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md) clause 7

## Context

[ADR-0015](0015-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)
clause 7 keeps an Agent on the upstream connection it was assigned to, "sticky while that
connection lives". Read as written, an assignment ends only when its upstream connection drops.
The Gateway does not keep it that long. It already releases the place of every Agent a downstream
WebSocket carried when that socket closes, and of an enrolling peer when its exchange ends
(`crates/fleet-agent/src/gateway/mod.rs:430`, `:596`). A plain-HTTP Agent opens no socket that
closes, so its place stayed until the upstream connection dropped, and a peer that polled under
ever new `instance_uid`s grew the pool's assignments until then (`docs/HARDENING.md`, Gateway).

[ADR-0039](0039-through-a-gateway-a-csr-proves-the-certificate-it-rests-on.md) clause 6 already
bounds what a host holds: at most `max_carried_agents` bindings, and past that a new report takes
the place of the host's least recently reported binding with neither a route nor a current offer.
An Agent whose binding was taken is no longer one the Gateway carries; its next report binds anew.
Its place in the pool is the one thing still left behind. The fix for that releases the place with
the binding, which a reader of clause 7 as written would take for a violation.

## Decision

We will keep an Agent on its upstream connection while that connection lives and the Gateway
carries the Agent, and release its place the moment the Gateway stops carrying it.

1. **Restates ADR-0015 clause 7. An Agent is assigned to the least-loaded live connection, and
   stays there while the Gateway carries it.** Assignment is by `instance_uid`. It is sticky while
   that connection lives and the Gateway carries the Agent, which keeps an Agent's `sequence_num`
   stream and its `ReportFullState` exchanges on one upstream socket. The Gateway carries an Agent
   while a downstream WebSocket that reported for it is open, or while its binding stands
   (ADR-0039 clause 6); an enrolling peer is carried for its one exchange. When the Gateway stops
   carrying an Agent, its place is released, and nothing is said upstream on its behalf (ADR-0015
   clause 10): its next report is assigned anew, to the least-loaded live connection, which may be
   another one. Any live upstream connection carries any downstream Agent: the pool is not divided
   by downstream peer, because nothing the Gateway sends upstream depends on which peer an Agent
   came through.

**Out of scope:** how many bindings a host or the Gateway holds, and which binding gives way
(ADR-0039 clause 6); re-balancing the pool; what the Server does with an Agent that moves to
another upstream connection, which it keys by `instance_uid` alone.

## Alternatives considered

- **Keep clause 7 as written.** A plain-HTTP peer could keep places for `instance_uid`s the Gateway
  no longer carries until the upstream connection drops, which is the gap this closes. Rejected.
- **Release a plain-HTTP Agent's place at the end of each exchange.** Its next poll could land on
  another connection every time, which breaks the one-socket stream clause 7 exists for. Rejected.
- **Release a place after an idle timeout.** A new duration with effect outside the Gateway,
  unrelated to the bound ADR-0039 already sets per host. Rejected.
- **Cap the pool's assignments separately.** A second bound beside the bindings' would have to be
  chosen, configured and kept in step with them. Rejected.

## Sources / Prior art

- `crates/fleet-agent/src/gateway/pool.rs:1-14` and `:239-252`: stickiness by `instance_uid` and
  the release that skips an Agent still routed.
- `crates/fleet-agent/src/gateway/mod.rs:430`, `:596`: the places released at the end of an
  enrolment exchange and when a downstream WebSocket closes.
- `docs/HARDENING.md`, the Gateway entry of *Where each bound applies today*: the plain-HTTP
  residual.

## Consequences

- Positive: a place in the pool lasts no longer than the Gateway carries its Agent, so a host
  holds at most `max_carried_agents` places, as it holds at most that many bindings. Clause 7
  states what the Gateway already did for WebSocket peers.
- Negative / trade-offs: an Agent whose binding gave way and that reports again may move to
  another upstream connection while the old one lives; its `sequence_num` stream then continues on
  another socket, which the Server accepts by `instance_uid`.
- Follow-ups: none.

## Enforcement

Tests that will carry `Verifies: ADR-0058` in `crates/fleet-agent/src/gateway/mod.rs`:

- A plain-HTTP peer that polls under more new `instance_uid`s than `max_carried_agents` leaves
  exactly the bound ones assigned, and each of them on the connection it was first assigned to.
- A report on a downstream WebSocket that takes another Agent's binding releases that Agent's
  place.
- When a downstream WebSocket closes, the places of the Agents it carried are released.
