# Spec: The wrapped kinds move onto the Plugin port

## Problem

Three wrapped kinds wait on `feat/` branches: `telegraf`, `glpi` and `icinga2`. Each is written
against the Plugin port that
[CLT-wrappers-act-through-the-core](../../adr/CLT-wrappers-act-through-the-core.md) replaced:

- Each starts a task of its own from a context of host paths.
- Each builds its own process runner.
- `icinga2` reads and writes files, starts its own subcommands and asks the host for its name,
  past the core.

None of the three can be rebased onto `main` without being rewritten, since `main` no longer has
the port they implement. Their proposed ADRs cite a record that the normalization of 2026-11-28
deleted. An operator cannot run any of the three agents under a Client built from `main`. Without
this work, the branches drift further, and `icinga2` keeps refusing a registered process account.

## Solution

An operator runs Telegraf, the GLPI Agent and Icinga 2 under a Client built from `main`, each as
a Supervisor of its kind, configured, health-reported and updated as the kind's ADR proposes. Each
kind acts on the host only through the core, so the core's checks hold for it. `icinga2` runs
under a registered process account like every other kind. Each kind reaches `main` by a pull
request of its own, `telegraf` first.

## Existing behaviour

- **On `main`:** the core and its two kinds, `collector` and `command`, already run on the new port.
  Their tests stay green, and nothing in the core changes for the wrapped kinds unless a ticket
  stops and hands off (Implementation decisions).
- **On the `feat/` branches:** each kind has its own end-to-end test file and the stub program it
  drives: `telegraf_supervisor`, `glpi_supervisor` and `icinga2_supervisor`. What these tests
  decide is the behaviour that must stay, except where a ticket's acceptance criteria say
  otherwise.
- **Changes to the tests:** a test that reached into the old port (a context, a runner, a spawner)
  is rewritten against the new one. A test that asserted the `icinga2` refusal of a process
  account is replaced by one that shows the kind running under it.
- **Data:** none migrates. A kind's directories are created as the core creates them for every
  kind, `data` and `state` included.

## Goals served

- G-7: an agent the project was never built for is managed like any other.
- G-8: a new process type is a new Plugin, written against the existing port without a change to
  the core.
- Q-5: a Plugin stays inside its Supervisor and reaches the host only through the core.

## User stories

1. As an operator, I want to run Telegraf under a Client built from `main`, so that I configure
   it from the Server like a Collector.
2. As an operator, I want to run the GLPI Agent the same way, so that inventory agents share the
   fleet's control loop.
3. As an operator, I want to run Icinga 2 in its Agent role and have it enrol with its master,
   so that monitoring agents are managed from the fleet.
4. As an operator who registered a process account, I want `icinga2` to run under that account
   as every other kind does, so that I need not give up the account to run it.
5. As a maintainer, I want every wrapped kind to pass the Plugin boundary check, so that no kind
   reaches past the core.
6. As a maintainer, I want each kind's proposed ADR to cite the record in force, so that the human
   can accept it without a dead citation.

## Implementation decisions

- **One ticket per kind, each on its own `feat/` branch.** The ticket rebases the branch onto
  `main` and ports the kind to the Plugin port of CLT-wrappers-act-through-the-core: an event
  handler acting through the host, with names by root and relative path and a manifest.
  - `telegraf` is the tracer bullet. `glpi` follows it, then `icinga2`.
  - Each branch lands by a pull request of its own once its ticket is done and reviewed (the
    human's choice, 2026-11-28).
- **Citations follow the normalization's map.**
  - Every citation of the deleted ADR on Supervisor Mode and its kinds moves clause by
    clause to CLT-wrappers-act-through-the-core or CLT-server-manages-the-supervisor-set, by the map
    of PR
    #231.
  - Citations of the other three dead ADRs move to their successors under the same clause numbers.
  - The proposed ADRs are rewritten in the ADR-only commit each branch already carries.
- **`icinga2` takes a registered process account.** The kind runs its subcommands through the
  core, and so through the second service. Its refusal of a registered process account goes, in
  the code and in its proposed ADR (the human's choice, 2026-11-28).
  - What the kind asked the host itself, it receives as host facts: its host name and the account
    it runs as.
- **The vocabulary of host functions stays closed.** A ticket that finds a kind needing a function
  the vocabulary lacks stops at that point. It writes into its ticket what is missing and why,
  and hands off. The superseding ADR goes through `propose-adr` with the human (the human's choice,
  2026-11-28).
- **The kind's other parts stay as its ADR proposes:**
  - `telegraf`'s UI grammar under `docs/adr/SRV-kinds-name-their-ui-grammar.md`;
  - the repacking in the operator tool;
  - the artifact documents and the manual's entries.

  They are rebased, not redesigned.

## Testing decisions

- **The seam:** each kind's end-to-end test file, driving the Client with the kind's stub program,
  as the `command` kind's tests on the new port do.
- **The boundary:** the structural test's Plugin role holds each kind to the port. A kind's module
  has that role, and the check that every implementation of a Plugin carries it finds each one.
- **The handler in isolation:** a fake host in memory, where a kind's steps in a row warrant it
  (`icinga2`'s enrolment), as the tests of a Plugin on the port alone do.
- **The process account for `icinga2`:** the test of a program a Plugin runs going through the
  second service is the prior art.

## Out of scope

- A new host function, or a take-over of an installation the host already has
  (CLT-wrappers-act-through-the-core §13).
- A Plugin compiled to WebAssembly.
- The server-drafts branch, which carries no kind.
- Accepting the kinds' proposed ADRs, which is the human's.

## ADR impact

- **Binding:**
  - CLT-wrappers-act-through-the-core, CLT-server-manages-the-supervisor-set,
    CLT-accounts-service-runs-under, CLT-installed-service-and-state, DEV-ports-and-adapters,
    PKG-signed-packages-and-delivery and OPS-operator-program-fleetctl.
  - For `telegraf`, also SRV-ui-colours-with-shiki.
  - The `NUC-` ADRs on planning and review.
- **Proposed on the branches, rewritten here:**
  - `docs/adr/CLT-telegraf-kind.md` and `docs/adr/SRV-kinds-name-their-ui-grammar.md`
    (`feat/telegraf`);
  - `docs/adr/CLT-glpi-agent-kind.md` (`feat/glpi`);
  - `docs/adr/CLT-icinga2-repacked-kind.md` (`feat/icinga2`).

  Each moves its citations by the map and drops what the old port forced on it.
- **New ADRs:** none, unless a ticket stops for a missing host function. A superseding ADR then
  goes through `propose-adr`.
