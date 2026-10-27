# Pipes and filters

The work is a sequence of independent **filters**, each reading one input and writing one
output, connected by **pipes** that carry data in one direction. A filter knows the shape of its
input and output and nothing about its neighbours.

## Signals

- The specification describes a flow: read, clean, transform, enrich, write.
- Steps are reordered, skipped, or reused in more than one flow.
- A bug is found by looking at what a step received and what it produced.

## Fits when

- Data flows one way: an import, a build, a compiler pass, an ETL job, a media pipeline.
- Steps are tested alone on fixtures of their input and output.
- Steps may later run in parallel or in separate processes.

## Does not fit

- Interactive behaviour where state goes back and forth between steps.
- Steps that need to know each other's internals or share mutable state: they are one step.

## The rule a test decides

No filter imports another filter; filters share only the data types that flow through the
pipes. The composition of the pipeline lives in one place.

## Sensors

The dependency-direction sensor the toolchain ADR names, such as ArchUnit, dependency-cruiser,
import-linter, go-arch-lint, or ArchUnitNET, with filters as
modules that may not see each other.

## Alternatives

- [Functional core, imperative shell](functional-core.md): one pure computation instead of a
  chain; simpler while there is one flow.
- Event-driven integration: steps decoupled in time and process, at the price of a broker and of
  tracing a flow across it.

## Sources

- Gregor Hohpe, Bobby Woolf, *Enterprise Integration Patterns: Pipes and Filters* —
  <https://www.enterpriseintegrationpatterns.com/patterns/messaging/PipesAndFilters.html>: filters
  with one input and one output, joined by pipes.
