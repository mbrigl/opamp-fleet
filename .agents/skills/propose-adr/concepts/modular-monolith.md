# Modular monolith

One deployable, split into **modules along the domain**, each owning its data and its internals
and reachable only through its public interface. Vertical slices are the same cut at a finer
grain: one slice per use case, from the entry point to the data, sharing little.

## Signals

- The glossary has groups of terms that rarely meet: billing and catalogue, ingest and report.
- Parallel sessions keep editing the same files for unrelated tickets.
- A change to one feature touches every layer and several other features on the way.

## Fits when

- Several areas of the domain grow at different speeds or are owned by different people.
- Parallel agent work needs boundaries that a ticket can stay inside.
- Splitting into services may come later, and the module boundary is where it would cut.

## Does not fit

- A small system with one area of domain: the modules are one module with ceremony around it.
- Areas so entangled that every use case crosses all of them: the boundary is drawn in the wrong
  place, or not yet known; start without it.

## The rule a test decides

No module imports another module's internals, only its public interface, and no module reads
another's data store directly. Which modules exist is the architecture overview's to name.

## Sensors

The dependency-direction sensor the toolchain ADR names, such as ArchUnit, dependency-cruiser,
import-linter, go-arch-lint, or ArchUnitNET, one rule per
module boundary.

## Alternatives

- [Layered](layered.md): cuts across the domain instead of along it.
- Services: the same boundaries with a network between them, paying for deployment and
  consistency what the modular monolith pays for nothing until it is needed.

## Sources

- Kamil Grzybek, *Modular Monolith: A Primer* —
  <https://www.kamilgrzybek.com/blog/posts/modular-monolith-primer>: modules with their own data
  and public interfaces inside one deployable.
- Jimmy Bogard, *Vertical Slice Architecture* —
  <https://www.jimmybogard.com/vertical-slice-architecture/>: coupling within a slice, not across
  layers.
- Martin Fowler, *MonolithFirst* — <https://martinfowler.com/bliki/MonolithFirst.html>: why the
  boundaries are found inside one deployable before they become services.
