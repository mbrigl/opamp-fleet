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
else. Every context is an `impl AppState` block over one shared state (the fleet under one lock,
the stores, the services), takes the root's items by `use super::*`, and reaches the others by
calling their methods on `self` or naming their types by path. Its spec left open whether a
context later gets state of its own, "once the boundaries can be seen in the code".

Measured at ac31fc719, the boundaries are close to an order but not one. With the order of the
Decision, all calls and names between the contexts point down except these six:

- `held` calls `session`'s `end_session` and names `session::Processed`;
- `held` calls `rollout`'s `take_labels`;
- `rollout` names three types of `views`: `DeploymentReach`, `PendingConfigurationView` and
  `PendingPackageView`;
- `session` calls `approval`'s `issue`;
- the module root calls `placement`'s `place_all` in its construction and `placed` and
  `arrived_over` in its reads of the fleet;
- the module root names context types as the types of the fields of `AppState` and `AgentRecord`:
  `held`'s `Kept` and `AGENT_LOCKS`, `placement`'s `Groups` and `Placement`, and `delivery`'s
  `PackageOffering`.

The opposite direction carries more of each pair: `session` reaches `held` through some ten methods
and six types, `approval` reaches `session` through `persist_if_dirty`, three functions, two types
and two constants, `views` reaches `rollout` through `waiting` and `Waiting`. Nothing holds the
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
method moved down into a context that does not seem to own it, or a type of a view kept in
`rollout`, would take it for a mistake.

## Decision

We will rank the contexts of `fleet` in one order and let each name only the contexts ranked below
it, while they keep sharing one state.

1. **The ranks.** From the bottom: the module root `fleet.rs`, `placement`, `held`, `delivery`,
   `rollout`, `views`, `session`, `approval`. `docs/ARCHITECTURE.md` names the contexts in this
   order.

2. **A context must name only contexts ranked below it.** It must not call a method another
   context defines on `AppState` ranked at or above it, nor name such a context's function, type or
   constant, whether by path, through a `use`, or through a re-export of the module root.

3. **The module root is the base.** Every context may use it; it names no context. Its `pub use`
   of a context's types keeps the public paths and is not a use by the root's own code. A step of
   construction or a read that needs a context's work belongs to that context or above it, or to
   the caller that builds the state.

4. **Every context has a rank before it has code.** A new submodule of `fleet` is given its place
   in the order, in `docs/ARCHITECTURE.md` and in the test, in the change that adds it. A dependency
   that would point up is resolved by moving the method or type down to where its callers can
   reach it, or by revising this ADR's order; never by an exception for one call.

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
- **`approval` below `session`**, as the order of the round suggests: the round issues a
  certificate. It leaves eight names pointing up instead of one, since enrolment answers with the
  round's helpers. Rejected.
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
  which can put them in a context a reader would not look in first; the module comment names why.
- Negative / trade-offs: the test resolves calls on `self`, `Self::` and `AppState::`; a call on
  another binding of `AppState` is left to review.
- Follow-ups: resolving the six edges of the Context that point up — the end of a session and
  `Processed` reached from `held`, the take of labels from `held`, the three view types `rollout`
  names, the issue of a certificate from `session`, the root's construction and reads that use
  `placement`, and the types of the root's fields defined in contexts; the order in
  `docs/ARCHITECTURE.md`; state per context, when a requirement asks for it.

## Enforcement

`crates/fleet-core/tests/dependency_direction.rs` will carry `Verifies:
SRV-fleet-contexts-call-by-rank`:

- A rank table of the module root and the submodules of `fleet`. A submodule of
  `crates/fleet-server/src/fleet/` outside `#[cfg(test)]` without a rank fails (clause 4).
- Outside `#[cfg(test)]` and `fixtures`, it collects the methods each context defines in its
  `impl AppState` blocks and fails on a call on `self`, `Self::` or `AppState::` of a method defined
  at or above the caller's rank, and on a path, a `use` or a name reached through the root's
  re-exports or `use super::*` that resolves into such a context (clauses 2, 3, 6). The root's own
  `pub use` lines are skipped.

The existing tests of the Server stay unchanged and green, which is what clause 7 asks.
