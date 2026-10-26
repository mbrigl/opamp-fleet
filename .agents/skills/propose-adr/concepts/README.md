# Architecture and project concepts

Established answers to the decisions most projects face early, kept here so that a session
proposing one starts from known answers instead of recalled ones. An entry is knowledge, not a
decision: nothing here binds a project. A concept binds once the project records it as its own
ADR through the `propose-adr` skill ([`AGENTS.md` §3](../../../../AGENTS.md#3-adr-rules)),
together with the test that decides its rule
([ADR-0002](../../../../docs/adr/0002-decisions-verified-by-tests.md)).

What the template ships is one candidate among the others, such as no prescribed shape or no
container access. Choosing it is recorded like any other choice, in the mini-ADR of
[`template.md`](../../../../docs/adr/template.md): without the record, the next session finds
no ADR that decides the question and proposes the concepts again
([`AGENTS.md` §3](../../../../AGENTS.md#3-adr-rules), rule 2).

## Harness and toolchain

Every project decides both at setup; neither has a template default.

| Concept | One sentence | Think of it when |
|---|---|---|
| [Harness](harness.md) | Which agents and models work here, who reviews what, and what may run unattended | a project starts, or a second agent, human, or parallel session joins |
| [Toolchain](toolchain.md) | The language, its build, test, and lint commands, and its sensors in three kinds | a project starts, or a design revision finds a mistake a sensor would have caught |

## Shape of the code

| Concept | One sentence | Think of it when |
|---|---|---|
| [Ports and adapters](ports-and-adapters.md) | A technology-agnostic core with ports it owns and adapters at the edges, every dependency pointing inward | the domain outlives its technologies, or must be tested without them |
| [Functional core, imperative shell](functional-core.md) | Decisions in pure functions over values, effects in a thin shell around them | the logic is rich and the effects are few, as in a CLI, a compiler, or a rules engine |
| [Layered](layered.md) | Presentation, domain, and data in layers, each depending only on the one below | a small team, a familiar shape, and a database the domain may lean on |
| [Modular monolith](modular-monolith.md) | One deployable split into modules along the domain, each owning its data and talking through a public interface | several areas of the domain grow at different speeds, or parallel sessions keep colliding |
| [Pipes and filters](pipes-and-filters.md) | Independent steps, each reading one input and writing one output, composed into a pipeline | data flows one way through transformations, as in an import, a build, or an ETL job |
| [Plugin](plugin.md) | A small host that loads extensions through an interface it defines | others extend the system without changing it, or variants ship from one code base |

The entries combine: a modular monolith whose modules are each ports and adapters, a pipeline
whose filters are each a functional core. Each part of a combination is a decision of its own.

## Environment

| Concept | One sentence | Think of it when |
|---|---|---|
| [Container access](container-access.md) | How the work inside the Dev Container reaches the containers it needs without the host's socket | the tests need a database or start containers themselves |

## Security and supply chain

| Concept | One sentence | Think of it when |
|---|---|---|
| [Secrets](secrets.md) | Where credentials live and how a leak is caught, beyond the baseline | the specification names credentials as an asset |
| [Action references](action-references.md) | Major version tag or commit SHA for the actions the workflows run | a workflow adds an action outside `actions/*`, or CI holds a token worth stealing |

An entry has the same sections as every other: *Signals*, *Fits when*, *Does not fit*, *The rule
a test decides*, *Sensors*, *Alternatives*, *Sources*. Each is a small harness for its concept:
the first three guide the decision, the rule and the sensors hold it once chosen, and *Does not
fit* is what the review checks where no sensor reaches.
