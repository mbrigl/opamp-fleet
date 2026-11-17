# Spec: The Server's fleet core, split by context

<!-- How it is written is the plan-feature skill under .agents/skills/. Decided in
     docs/adr/0006-feature-layer.md. -->

## Problem

The Server's core module `fleet` is one file of about 14 000 lines. The impl of `AppState`
alone runs about 3 400 lines and about 130 methods, followed by about 8 800 lines of unit tests.
Enrolment and certificates, Configurations and Deployments, Packages, the round of an Agent's
report, and the views of the fleet sit side by side in it. A session that changes one context
loads all of them, a reviewer cannot see from the diff which context a change belongs to, and
it is hard to see where a later service with its own state could be cut.

## Solution

`fleet` becomes a module root with one submodule per context. Each submodule holds that context's
`impl AppState` block, its types and its unit tests, as `fleet::held` and `fleet::placement`
already do. The module root keeps the state itself, its construction, its limits, and the types
every context shares. Nothing outside `fleet` changes: every public path stays as it is.

Whether a context later gets a service struct that owns its own state is decided by a later
spec, once the boundaries can be seen in the code.

## Existing behaviour

Nothing changes in behaviour. The unit tests of `fleet` (153 of them) and every test suite of
the Server pin it. Each ticket moves tests alongside its code and keeps their names, since
accepted ADRs cite them by name in their *Enforcement*. No caller outside `fleet` is touched.

## Goals served

None directly. The work serves the principle of evolvability
([`AGENTS.md` §1](../../../AGENTS.md#1-principles)). It is a refactoring that changes no
interface, which the specification does not ask for, and it is done at the human's request
after an architecture review (2026-11-17).

## User stories

1. As an agent changing the rollout of Configurations, I want to load only the rollout code and
   what it shares, so that the session's context holds the change and not the whole fleet.
2. As a reviewer, I want a change to name the context it touches by its file, so that I can see
   from the diff what it may affect.
3. As the maintainer, I want the contexts apart in the code, so that a later decision on service
   structs with their own state rests on boundaries that can be seen.

## Implementation decisions

- One submodule of `fleet` per context: the views of the fleet (`views`), Packages (`delivery`),
  Configurations, Deployments and rollouts (`rollout`), enrolment and certificates
  (`approval`), and the round of an Agent's report (`session`). They take the shape of
  `fleet::placement`: `use super::*`, an `impl AppState` block, and a module comment that names
  the ADR clauses it carries.
- The module root keeps `AppState` and its fields, its construction (`with_*`) and limits, the
  lease, and the types the contexts share, such as `AgentRecord`, `Assignment`, the error enums
  and the ports `Clock` and `CertificateSigner`.
- A type that moves stays reachable at its old path through a `pub use` in the module root.
  Accepted ADRs cite paths such as `fleet::CertificateSigner` and `fleet.rs`, and those must
  stay true until the next normalization re-points them.
- A private method another context calls becomes `pub(super)`, never `pub`.
- The submodules for Packages and enrolment are named `delivery` and `approval`, not `packages`
  and `enrolment`, so that a reader never mistakes them for the crate's modules of those names.
- The test helpers several contexts share go into a test-only submodule of `fleet` in the
  tracer bullet. A helper only one context uses goes with that context.
- Which context a method belongs to is decided by what it changes, not by what it reads. A
  method two contexts would claim stays in the module root, and the ticket says so in its
  hand-off.

## Testing decisions

- No new tests. The seam is the existing unit tests of `fleet` and the Server's test suites,
  unchanged in number and in name.
- Each ticket proves the move with the count of unit tests under `fleet` before and after, with
  `scripts/check-traceability.sh` (named tests are found in any file), and with
  `scripts/check-all.sh`.

## Out of scope

- Service structs with state of their own, and any change to the locks, the persist path or the
  push.
- Every module outside `fleet`, among them `api`, `transport` and the test suite `instances`.
- The 40 top-level modules of `fleet-server` and their grouping.
- Rewording the ADRs that cite `fleet.rs`. That is the next normalization's work.

## ADR impact

None new. [`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules) rule 2 exempts a refactoring that
changes no interface, and [ADR-0009](../../adr/0009-ports-and-adapters.md) leaves the core's
internal structure open. The ADRs that bind `fleet` stay binding on every submodule, since
`scripts/sensor-binding-adrs.sh` routes `fleet/` to the same ADRs as `fleet.rs`: ADR-0008,
-0012, -0013, -0015, -0018, -0023, -0025, -0026, -0027, -0028, -0030, -0032 and -0033.
