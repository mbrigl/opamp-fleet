# Layered

Also called n-tier. The code is split into **presentation**, **domain**, and **data**, and each
layer depends only on the layer below it. The domain may know the data layer; the data layer
knows neither of the others.

## Signals

- The system is mostly forms or endpoints over a database, with modest rules in between.
- The framework the project chose is built around this split.
- Request-handling code reaches straight into the database from the UI.

## Fits when

- The domain is modest and the database will not be replaced.
- The team, or the agents' training data, know the shape well, so a newcomer reads it at once.
- The framework's own conventions already enforce most of it.

## Does not fit

- A domain that must be tested without its database: the downward direction puts the data layer
  under the domain.
- A system expected to change its persistence or add a second way in: every change crosses all
  layers.
- A system that grows along the domain: layers cut across features, and one feature touches
  every layer; a [modular monolith](modular-monolith.md) cuts the other way.

## The rule a test decides

No module imports from a layer above its own, and the presentation layer does not reach the data
layer past the domain where the project decides it may not.

## Sensors

The dependency-direction sensor the toolchain ADR names, such as ArchUnit, dependency-cruiser,
import-linter, go-arch-lint, or ArchUnitNET; most of them ship a
layered rule as their first example.

## Alternatives

- [Ports and adapters](ports-and-adapters.md): turns the domain-to-data dependency around, so the
  domain is tested alone, at the price of ports.
- [Modular monolith](modular-monolith.md): cuts along the domain first; it can hold layers inside
  each module.

## Sources

- Martin Fowler, *Presentation Domain Data Layering* —
  <https://martinfowler.com/bliki/PresentationDomainDataLayering.html>: the three layers, and why
  larger systems split by domain first and layer second.
