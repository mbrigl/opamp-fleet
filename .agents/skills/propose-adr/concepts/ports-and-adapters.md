# Ports and adapters

Also called hexagonal architecture. A technology-agnostic **core** holds the domain; **ports**,
owned by the core, say what it needs and what it offers; **adapters** at the edges bind those
ports to a technology: user interface, persistence, network, external services. Every dependency
points **inward**: adapters depend on the core, and the core depends on no adapter and on no
technology.

## Signals

- The specification names more than one way in or out: a CLI and an API, two storage backends,
  an external service that may be replaced.
- Tests of domain behaviour need a database, a network, or a framework to run.
- Domain terms of the glossary appear in the same file as SQL, HTTP, or UI code.

## Fits when

- The domain is the asset and is expected to outlive its technologies.
- A quality goal asks for the domain to be verified fast and without infrastructure.
- Agents work in parallel: a ticket names the port it works behind, and a diff stays on one side
  of a boundary a reviewer can hold in one reading.

## Does not fit

- A thin CRUD application, where the domain *is* the data and every port has one adapter
  forever.
- A script, a small CLI, or a library without infrastructure: the indirection costs more than the
  seam it buys.
- A team that will not keep the split: without its test, it erodes into a layered system with
  extra interfaces.

## The rule a test decides

No module of the core imports an adapter or a technology. What counts as a technology in the
language, usually not the standard library or a pure utility library, is the golden path's to
show and `docs/CONVENTIONS.md`'s to say. What no test decides, a port no wider than its callers
need or a core free of an adapter's vocabulary, is review.

## Sensors

The dependency-direction sensor the toolchain ADR names, such as ArchUnit, dependency-cruiser,
import-linter, go-arch-lint, or ArchUnitNET.

## Alternatives

- [Layered](layered.md): familiar, but the direction runs top-down, so persistence shapes the
  domain and a test of the domain needs the database.
- Clean or onion architecture: the same dependency rule with more prescribed rings; heavier
  vocabulary for the same testable seam.
- [Functional core, imperative shell](functional-core.md): the same seam drawn between pure and
  effectful code instead of between core and technology; lighter where effects are few.

## Sources

- Alistair Cockburn, *Hexagonal Architecture* —
  <https://alistair.cockburn.us/hexagonal-architecture/>: the core, the ports it owns, and the
  adapters that plug into them.
- Robert C. Martin, *The Clean Architecture* —
  <https://blog.cleancoder.com/uncle-bob/2012/08/13/the-clean-architecture.html>: the dependency
  rule, source code dependencies point inward only.
