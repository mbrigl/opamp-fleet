# DEV-ports-and-adapters: Application code is structured as ports and adapters, with every dependency pointing at the core

- **Status:** 🟢 accepted
- **Date:** 2026-09-19
- **Deciders:** Markus Brigl
- **Applies to:** every module of the system's code, the module roles in `docs/ARCHITECTURE.md`, and the structural test that decides the dependency direction, `crates/fleet-core/tests/dependency_direction.rs`

## Context

The specification says what is built, and the rule file how work is governed. This ADR says how
the code is shaped. The specification's strategy puts the supervision domain at the centre. Ports
face the Server and the Managed Process, and Plugins are the adapters that implement concrete
process types. G-8 asks that another kind of Managed Process take a Plugin written against the
existing Ports, without a change to the supervision core. Two further forces make the shape worth
deciding once:

- **Agents work in seams.** An explicit boundary lets a session change one side of it and produce
  a diff a reviewer can hold in one reading. Without one, changes reach across the code base, and
  the review sees everything or nothing.
- **The Definition of Done demands testable behaviour**
  ([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)). The suite is the
  contract the implementation is held to, so the domain has to be verifiable without the
  technology it runs on. A decision about the shape has to be one a test can decide
  ([NUC-decisions-cite-tests §3](NUC-decisions-cite-tests.md)), because the shape is what entropy
  erodes first.

The workspace and its toolchain are decided in
[DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md). The structural test that keeps the
shape lives beside the code it reads.

## Decision

We will structure application code as **ports and adapters**, with every dependency pointing
**inward**.

1. **A technology-agnostic core holds the domain.**
2. **Ports belong to the core and say what it needs and what it offers.**
3. **Adapters at the edges bind those ports to a technology:** user interface, persistence,
   network, external services.
4. **Every dependency points inward.** Adapters depend on the core. The core depends on no adapter
   and on no technology.

**Out of scope:**

- where ports and adapters live in a given language and how they are named;
- what counts as a technology there (a standard library or a pure utility library usually does
  not). The code and its structural test show it, and [`docs/CONVENTIONS.md`](../CONVENTIONS.md)
  says it;
- whether the system has one core or several bounded contexts;
- the internal structure of the core.

## Alternatives considered

- **No prescribed shape.** The shape would be decided again per change, or not at all and drift.
  Domain logic would bind to the first framework that touched it.
- **Layered (n-tier).** Familiar, but its top-down direction lets the persistence layer shape the
  domain, and a test of the domain needs the database.
- **Clean or onion architecture.** The same dependency rule with more prescribed rings. The
  vocabulary is heavier for the same testable seam.
- **Decide it in the harness or toolchain ADR.** The shape is a decision of its own, with its own
  alternatives and its own test. Folding it into another ADR hides it from a reader who looks for
  how the code is structured.

## Sources / Prior art

- Alistair Cockburn, *Hexagonal Architecture (Ports and Adapters)*:
  <https://alistair.cockburn.us/hexagonal-architecture/>. The core, the ports it defines, and the
  adapters that plug into them.
- Robert C. Martin, *The Clean Architecture*:
  <https://blog.cleancoder.com/uncle-bob/2012/08/13/the-clean-architecture.html>. The dependency
  rule (source code dependencies point inward only), which is the one part of SOLID's dependency
  inversion a test can decide.

## Consequences

- Positive:
  - The domain is tested without its technology.
  - A technology is an adapter decision, swapped behind a port instead of rewritten through the
    code base.
  - A ticket names the port it works behind, so parallel sessions collide less.
  - The structural test has a rule to decide.
- Negative / trade-offs: indirection and interface ceremony that a thin part of the system does
  not earn. Such a part changes by superseding this ADR, not by ignoring it. The split erodes
  unless its test exists.
- Follow-ups: a context map once the system has more than one core.

## Enforcement

The structural test
[`crates/fleet-core/tests/dependency_direction.rs`](../../crates/fleet-core/tests/dependency_direction.rs)
carries `Verifies:` markers for this ADR.

- Its table gives every module of the workspace's library crates a role: core, adapter, or
  composition root.
- `the_core_depends_on_no_adapter_and_no_technology` reads the source of every core module
  through its syntax tree. It fails when a core module depends on an adapter or on a technology,
  and when a module has no role (clause 4).
- `no_adapter_names_the_composition_roots_kept_from_adapters` fails when an adapter names a
  composition root the table keeps from adapters (clause 4).

**Not mechanically decidable:** review
([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)) and the design revision judge
what the test cannot decide:

- one responsibility per module;
- a port no wider than its callers need;
- a core open to a new adapter without a change to itself;
- a core that knows no adapter's vocabulary.
