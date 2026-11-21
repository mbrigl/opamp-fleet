# SRV-fleet-contexts-call-by-rank: The contexts of the Server's fleet core are ranked, and each names only those below it

- **Status:** 🟡 proposed
- **Date:** 2026-11-21
- **Deciders:** Markus Brigl
- **Applies to:** the module root `crates/fleet-server/src/fleet.rs` and every submodule of `crates/fleet-server/src/fleet/` outside `#[cfg(test)]`, the order of the contexts in `docs/ARCHITECTURE.md`, the rank table in `crates/fleet-core/tests/dependency_direction.rs`

## Context

[DEV-ports-and-adapters](DEV-ports-and-adapters.md) leaves the internal structure of the core open,
and with it whether the system has one core or several bounded contexts.
[SRV-two-use-case-layers](SRV-two-use-case-layers.md) decides what each plane may reach of the
core, not how the core is cut inside.

The Server's core module `fleet` was split by context in #202 into seven submodules: `placement`
(the Agents of a Client under its Client Agent), `held` (what one instance holds of the Agents it
carries), `delivery` (Packages in the fleet), `rollout` (Configurations, Deployments and rollouts),
`views` (the views of the fleet), `session` (the round of an Agent's report) and `approval`
(enrolment and certificates). The module root keeps the state, its construction and limits, the
lease, the reads of the fleet and the types the contexts share. The split moved code and nothing
else. Every context is an `impl AppState` block over one shared state (the fleet under one lock, the
stores, the services), takes the root's items by name through `use super::{…}`, and reaches the
others by calling their methods on `self` or naming their types by path. Its spec left open whether
a context later gets state of its own, "once the boundaries can be seen in the code".

Measured at ac31fc719, the boundaries are close to an order but not one. With the order of the
Decision, all calls and names between the contexts point down except these six:

- `held` calls `session`'s `end_session` and names `session::Processed`;
- `held` calls `rollout`'s `take_labels`;
- `rollout` names three types of `views`: `DeploymentReach`, `PendingConfigurationView` and
  `PendingPackageView`;
- `approval` reaches `session` through `persist_if_dirty`, the helpers of the round's answers
  `bad_request`, `unavailable` and `compose_settings_offer`, the types `Ending` and `Processed`
  and two constants;
- the module root calls `placement`'s `place_all` in its construction and `placed` and
  `arrived_over` in its reads of the fleet;
- the module root names context types as the types of the fields of `AppState` and `AgentRecord`:
  `held`'s `Kept` and `AGENT_LOCKS`, `placement`'s `Groups` and `Placement`, and `delivery`'s
  `PackageOffering`.

The opposite direction carries more of most pairs: `session` reaches `held` through some ten
methods and six types, and `views` reaches `rollout` through `waiting` and `Waiting`. Between
`approval` and `session` it carries less, one call of `approval`'s `issue` from the round, but that
call reaches the issuing of certificates, which four accepted ADRs bind by the file
`fleet/approval.rs` in their *Applies to*:
[SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md),
[SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md),
[CLT-modes-and-gateway](CLT-modes-and-gateway.md) and
[SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md). The helpers `approval` reaches
are bound by file as well:
[SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md) binds every
`Unavailable` reply in `fleet/session.rs` and `fleet/approval.rs`, CLT-modes-and-gateway binds the
two constants of the enrolment's refusals by `fleet/approval.rs`,
[SRV-instance-opens-chosen-planes](SRV-instance-opens-chosen-planes.md) binds the persist path by
`fleet/held.rs`, and
[SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md) binds the
composition of the settings offer by `fleet/session.rs` and names `fleet.rs` only for the
capability declaration. Nothing holds the
direction: a session that needs a method of another context calls it, and the cycles grow.

The architecture catalogue's *modular monolith* fits when several areas of the domain grow at
different speeds, when parallel work needs boundaries a ticket can stay inside, or when a later
split into services would cut along them. It does not fit where every use case crosses all areas:
the boundary is then drawn in the wrong place or not yet known. Both hold here in part. The areas
differ, and the glossary keeps their terms apart, but the round of an Agent's report crosses
`held`, `placement`, `rollout`, `delivery` and `approval` under one lock, so that state of its own
per context would cut through the atomicity that lock gives. No current goal or quality goal of the
specification asks for the boundaries, and no split into services or parallel work on `fleet` is
planned; the decision serves evolvability ([`AGENTS.md` §1](../../AGENTS.md#1-principles)), so
that a reviewer sees from a change which contexts it can affect and a later decision on state per
context finds boundaries without cycles.

It is recorded because it binds every later change to `fleet`, and because a reader who finds a
method moved down into a context that does not seem to own it, a type of a view kept in `rollout`,
or the helpers of the round's answers in `approval` and its persist path in `held`, would take it
for a mistake.

## Decision

We will rank the contexts of `fleet` in one order and let each name only the contexts ranked below
it, while they keep sharing one state.

1. **The ranks.** From the bottom: the module root `fleet.rs`, `placement`, `held`, `delivery`,
   `rollout`, `views`, `approval`, `session`. `docs/ARCHITECTURE.md` names the contexts in this
   order.

2. **A context must name only contexts ranked below it.** It must not call a method that another
   context ranked at or above it defines on `AppState`. It must not name such a context's function,
   type or constant either: not by path, not through a `use`, not through a re-export of the module
   root.

3. **The module root is the base.** Every context may use it; it must not name a context. Its `pub
   use` of a context's types keeps the public paths and is not a use by the root's own code. A step
   of construction or a read that needs a context's work belongs to that context or above it, or to
   the caller that builds the state.

4. **Every context has a rank before it has code.** The change that adds a submodule of `fleet`
   must give it a rank in `docs/ARCHITECTURE.md` and in the test. The author of a dependency that
   would point up must resolve it: the method or type moves down to where its callers can reach
   it, or this ADR's order is revised. The author must not add an exception for one call.

5. **The state stays shared.** The ranks do not split `AppState`, its lock, its stores or its
   services. Every context may read and write every field, and what one lock holds together stays
   under it.

6. **Test code is not ranked.** The `#[cfg(test)]` modules and the test-only module `fixtures` may
   name every context.

7. **Nothing an Agent or an operator sees may change.** The REST API, the OpAMP exchange, the audit
   entries and the persisted layout stay as they are. A change of behaviour must not ride on a move.

**Out of scope:** state owned by one context, service types per context, and any change to the
lock, the persist path or the wake-up of the connections; the modules of `fleet-server` outside
`fleet`; the use-case modules, which [SRV-two-use-case-layers](SRV-two-use-case-layers.md)
decides; the structure of the other crates.

## Alternatives considered

- **A modular monolith with state per context.** Each context a type that owns its data behind a
  public interface, `AppState` only composing them. It is the catalogue's full form and the one a
  split into services would need, but the round of a report changes the record, its placement, its
  rollout state and its certificate under one lock; owning them apart means a new answer to that
  atomicity, which no current requirement pays for. Rejected for now; the ranks are where it would
  cut.
- **A list of the allowed edges between contexts.** Stricter than an order, since `delivery` and
  `placement`, ranked apart, never meet; but every new dependency would be a change of the ADR.
  Rejected; an order says the same about cycles with one line per context.
- **The module root as base with named exceptions** for its construction and reads that use
  `placement`. Less to move, but an edge the order hides is a cycle through the root. Rejected.
- **`approval` above `session`.** It leaves one name pointing up instead of eight, the round's call
  of `issue`. But resolving that edge moves the issuing of certificates out of `fleet/approval.rs`,
  and the four accepted ADRs that bind it by that file would no longer come into scope for a
  change to the reading of a CSR until a normalization moved their paths. Rejected; the helpers
  move down instead, each to a file that already binds it.
- **The helpers of the round's answers in the module root.** One place for what `approval` and
  `session` share, but the persist path calls `held`, which the root must not name, and the replies
  would leave the files through which their ADRs bind them. Rejected.
- **`views` below `rollout`.** `rollout` names three view types, but `views` reads what waits for an
  Agent through `rollout`; the view types are values `rollout` computes and move more easily.
  Rejected.
- **The split recorded as files alone, with no rule.** It is what the code has today, and needs no
  ADR; the cycles above are what it leads to. Rejected.
- **[Layered](DEV-ports-and-adapters.md#alternatives-considered).** Rejected by
  [DEV-ports-and-adapters](DEV-ports-and-adapters.md); it cuts across the domain, where the ranks
  run along it.

## Sources / Prior art

- The catalogue entry *modular monolith* of the `propose-adr` skill under
  [`.agents/skills/`](../../.agents/skills/), its *Fits when* and *Does not fit* checked against
  this project above, and its sources:
  - Kamil Grzybek, *Modular Monolith: A Primer* —
    <https://www.kamilgrzybek.com/blog/posts/modular-monolith-primer>: modules with their own data
    and public interfaces inside one deployable.
  - Jimmy Bogard, *Vertical Slice Architecture* —
    <https://www.jimmybogard.com/vertical-slice-architecture/>: coupling within a slice, not across
    layers.
  - Martin Fowler, *MonolithFirst* — <https://martinfowler.com/bliki/MonolithFirst.html>: why the
    boundaries are found inside one deployable before they become services.
- The spec of the split in #202, `docs/plan/fleet-by-context/spec.md` as of 982553e14, which
  deferred state per context.
- The code at ac31fc719: the calls on `self` and the paths between `fleet.rs` and the submodules of
  `crates/fleet-server/src/fleet/` outside `#[cfg(test)]`, counted by method and type name.

## Consequences

- Positive: a change to one context can affect only the contexts above it, which a reviewer reads
  from the order, and a cycle between contexts cannot form.
- Positive: a later decision on state per context, or a split along the planes beyond
  [SRV-instance-opens-chosen-planes](SRV-instance-opens-chosen-planes.md), starts from boundaries
  that hold.
- Negative / trade-offs: a linear order ranks contexts that never meet, so a dependency between
  them can point the wrong way for no domain reason and need a move or a new order.
- Negative / trade-offs: methods and types move to where the order lets their callers reach them,
  and a helper goes to a file that already binds it. Either can put it in a context a reader would
  not look in first; the module comment names why.
- Negative / trade-offs: the module root carries the types of the state, so their fields are no
  longer private to the one context that works on them; clause 5 allows every context to reach
  them.
- Negative / trade-offs: the test resolves calls on `self`, `Self::` and `AppState::` in `impl
  AppState` blocks, and review covers what it does not see. It does not see a call on another
  binding of `AppState`, nor a method of a type of the state that a context defines and another
  calls through a field. It does not collect a block written as `impl super::AppState` or `impl
  Trait for AppState`. It resolves `super::` one level off in an inline module inside a context.
- Negative / trade-offs: accepted ADRs cite some moved items by the file they left, until the
  next normalization moves the citations.
  [SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md) cites
  `unavailable` and `RETRY_AFTER` in `fleet/session.rs`, now in `fleet/approval.rs`.
  [SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md) cites the construction and the
  stored reads in `fleet.rs`, now in `fleet/placement.rs`.
  [SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md) cites the
  composition of the offer in `fleet/session.rs`, now in `fleet.rs`. The binding sensor still
  lists these ADRs for the new files, since it routes every file under `fleet/` to the ADRs of
  `fleet.rs` as well.
- Follow-ups: state per context, when a requirement asks for it. The six edges of the Context are
  resolved with this decision's implementation.

## Enforcement

`crates/fleet-core/tests/dependency_direction.rs` will carry `Verifies:
SRV-fleet-contexts-call-by-rank`:

- A rank table of the module root and the submodules of `fleet`. A submodule of
  `crates/fleet-server/src/fleet/` outside `#[cfg(test)]` without a rank fails (clause 4).
- Outside `#[cfg(test)]` and `fixtures`, it collects the methods each context defines in its `impl
  AppState` blocks. It fails on a call on `self`, or a call or function value through `Self::` or
  `AppState::`, of a method another context defines at or above the caller's rank (clause 2). It
  fails on a path, a `use` or a name reached through the root's re-exports that resolves into such
  a context (clauses 2, 3). It skips the root's own `pub use` lines and test code (clauses 3, 6).
- Review covers the order in `docs/ARCHITECTURE.md` and what the test does not see, as
  *Consequences* lists it.

The existing tests of the Server stay unchanged and green, which is what clause 7 asks.
