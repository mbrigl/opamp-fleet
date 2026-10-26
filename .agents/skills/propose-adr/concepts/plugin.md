# Plugin

Also called microkernel. A small **host** defines an extension interface and loads
**plugins** that implement it, found by configuration or discovery at start-up. The host knows
the interface, never a plugin.

## Signals

- The specification names extension by others, or variants for customers, platforms, or formats.
- A `switch` over a kind keeps growing, and each new kind touches the core.

## Fits when

- Third parties or other teams extend the system without changing it.
- Variants ship from one code base, chosen by configuration.
- The extension interface is stable enough to publish: it is a public interface, and an ADR of
  its own under [`AGENTS.md` §3](../../../../AGENTS.md#3-adr-rules), rule 1.

## Does not fit

- Two or three known variants that change with the core: a plain interface with its
  implementations in the tree does the same without loading or discovery.
- An extension interface nobody can yet describe: a premature one freezes the wrong shape.

## The rule a test decides

The host imports no plugin; a plugin imports only the extension interface and what the host
publishes for it. A host test runs with a fake plugin.

## Sensors

The dependency-direction sensor the toolchain ADR names, such as ArchUnit, dependency-cruiser,
import-linter, go-arch-lint, or ArchUnitNET, with the host and
each plugin as separate modules.

## Alternatives

- [Ports and adapters](ports-and-adapters.md): the same inversion with adapters compiled in,
  without loading; enough when the variants are the project's own.
- Configuration flags: enough for variants that differ in values, not in behaviour.

## Sources

- Martin Fowler, *Patterns of Enterprise Application Architecture: Plugin* —
  <https://martinfowler.com/eaaCatalog/plugin.html>: linking implementations at configuration
  time, not compile time.
