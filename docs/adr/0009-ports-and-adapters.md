# ADR-0009: Application code is structured as ports and adapters, with every dependency pointing at the core

- **Status:** 🟢 accepted
- **Date:** 2026-09-19
- **Deciders:** Markus Brigl
- **Applies to:** every module of the system's code, the module roles in `docs/ARCHITECTURE.md`, and the structural test that decides the dependency direction, `crates/fleet-core/tests/dependency_direction.rs`

## Context

The specification says what is built and the rule file how work is governed; something has to say
how the code is shaped. The specification's strategy puts the supervision domain at the centre,
with Ports for the Server-facing and the managed-process side and Plugins as the adapters that
implement concrete process types, and G-8 asks that another kind of Managed Process take a Plugin
written against the existing Ports, without a change to the supervision core. Two further forces
make the shape worth deciding once:

- **Agents work in seams.** A boundary that is explicit lets a session change one side of it and
  produce a diff a reviewer can hold in one reading; without one, changes reach across the code
  base and the review sees everything or nothing.
- **The Definition of Done demands testable behaviour**
  ([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)): the suite is the
  contract the implementation is held to, so the domain has to be verifiable without the
  technology it runs on, and a decision about the shape has to be one a test can decide
  ([ADR-0002](0002-decisions-verified-by-tests.md)); the shape is what entropy erodes first.

The workspace has its toolchain ([ADR-0008](0008-five-crates-on-tokio-and-axum.md)), so the
structural test that keeps the shape exists beside the code it reads.

## Decision

We will structure application code as **ports and adapters**, with every dependency pointing
**inward**.

1. **A technology-agnostic core holds the domain.**
2. **Ports, owned by the core, say what it needs and offers.**
3. **Adapters at the edges bind those ports to a technology:** user interface, persistence,
   network, external services.
4. **Every dependency points inward.** Adapters depend on the core, and the core depends on no
   adapter and on no technology.

**Out of scope:** where ports and adapters live in a given language and how they are named, and
what counts as a technology there (a standard library or a pure utility library usually does not),
which the code and its structural test show and [`docs/CONVENTIONS.md`](../CONVENTIONS.md) says;
whether the system has one core or several bounded contexts; and the internal structure of the
core.

## Alternatives considered

- **No prescribed shape.** Re-decided per change, or not decided and drifting, with domain logic
  bound to the first framework that touched it.
- **Layered (n-tier).** Familiar, but its top-down direction lets the persistence layer shape the
  domain, and a test of the domain needs the database.
- **Clean or onion architecture.** The same dependency rule with more prescribed rings; the
  vocabulary is heavier for the same testable seam.
- **Decide it in the harness or toolchain ADR.** The shape is a decision of its own, with its own
  alternatives and its own test; folding it into another ADR hides it from a reader who looks for
  how the code is structured.

## Sources / Prior art

- Alistair Cockburn, *Hexagonal Architecture (Ports and Adapters)*:
  <https://alistair.cockburn.us/hexagonal-architecture/>, the core, the ports it owns, and the
  adapters that plug into them.
- Robert C. Martin, *The Clean Architecture*:
  <https://blog.cleancoder.com/uncle-bob/2012/08/13/the-clean-architecture.html>, the dependency
  rule (source code dependencies point inward only), which is the one clause of SOLID's dependency
  inversion a test can decide.

## Consequences

- Positive: the domain is tested without its technology; a technology is an adapter decision,
  swapped behind a port instead of rewritten through the code base; a ticket names the port it
  works behind, so parallel sessions collide less; the structural test has a rule to decide.
- Negative / trade-offs: indirection and interface ceremony that a thin part of the system does
  not earn; such a part is changed by superseding this ADR rather than by ignoring it. The split
  erodes unless its test exists.
- Follow-ups: a context map once the system has more than one core.

## Enforcement

The structural test
[`crates/fleet-core/tests/dependency_direction.rs`](../../crates/fleet-core/tests/dependency_direction.rs)
carries a `Verifies:` marker for this ADR. It gives every module of the workspace's library crates
a role (core, adapter, or composition root), reads the source of every core module, and fails when
a core module depends on an adapter or on a technology, and when a module has no role (clause 4).

**Not mechanically decidable:** what the test cannot decide (one responsibility per module, a port
no wider than its callers need, a core open to a new adapter without change to itself, a core that
knows an adapter's vocabulary) is review
([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)) and the design revision.
