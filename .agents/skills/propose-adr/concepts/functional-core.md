# Functional core, imperative shell

The decisions of the system are pure functions over plain values: the **functional core**. A
thin **imperative shell** around it reads input, the clock, files, the database, and the network,
calls the core with values, and carries out what the core returns. The core performs no effect
and calls nothing that does.

## Signals

- The interesting logic is a computation: parsing, validation, pricing, planning, diffing.
- Tests need mocks for the clock, the file system, or the network to reach a branch of logic.
- A function both decides something and writes the result somewhere.

## Fits when

- The logic is rich and the effects are few and simple: a CLI, a compiler or code generator, a
  rules engine, a data transformation.
- A quality goal asks for exhaustive or property-based tests of the logic.
- The language makes values cheap: immutable records, data classes, structs.

## Does not fit

- Logic that is mostly effects in a sequence, a workflow over external services, where the shell
  would be the whole program.
- Several technologies to swap behind the same behaviour: the seam does not name them, and
  [ports and adapters](ports-and-adapters.md) does.
- Large mutable state the core would have to copy on every step, where the language makes
  copying expensive.

## The rule a test decides

No module of the core imports an I/O, clock, randomness, or framework API. The list of what
counts is the project's, kept beside the test. That a shell function stays thin is review.

## Sensors

The dependency-direction sensor the toolchain ADR names, such as ArchUnit, dependency-cruiser,
import-linter, go-arch-lint, or ArchUnitNET, configured with
the forbidden APIs instead of forbidden modules.

## Alternatives

- [Ports and adapters](ports-and-adapters.md): the seam is drawn at technologies, not at effects;
  it pays where technologies change.
- No split: simplest while the logic is small; a test that needs a mock to reach a branch is the
  signal it stopped being small.

## Sources

- Gary Bernhardt, *Functional Core, Imperative Shell* —
  <https://www.destroyallsoftware.com/screencasts/catalog/functional-core-imperative-shell>: the
  core of values and the shell of effects around it.
- Gary Bernhardt, *Boundaries* — <https://www.destroyallsoftware.com/talks/boundaries>: why
  values at the boundary make the core testable without mocks.
