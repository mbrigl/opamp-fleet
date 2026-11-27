# Spec: Plugins act through the core

## Problem

A Supervisor Plugin today is a factory that starts a task of its own and receives host paths,
the Endpoint token, the archive key, the process account's group and the event channel. Each kind
wires its own `Runner`. Nothing keeps a kind from reaching files, programs or the host past the
core. A wrapped kind that does, as `icinga2` on its branch does, cannot start its programs
through the second service and so refuses a process account. A Plugin cannot be written in any
other form than native Rust with full access to the Client.

## Solution

An operator sees no change in what a `collector` or `command` Supervisor does. A Plugin author
writes an asynchronous event handler that states a manifest and acts only through the host
functions of the core. Everything the Plugin touches lies under the roots of its Supervisor, and
every program it runs is started the way the Managed Process is started. The port carries plain
data only, so a WebAssembly Plugin can later implement it without a change to the core.

## Existing behaviour

- `collector` and `command` are the only kinds on `main`. Their behaviour stays as it is: spawn,
  watchdog, bounded stop, restart as the apply, health-gated acknowledgement, package swap with
  preflight and rollback, version probe, placeholders, uninstall as the stop.
- The tests that pin it are named in the Enforcement of CLT-wrappers-act-through-the-core,
  carried from the superseded ADR: the process tests of the Client, its Supervisor tests, the
  end-to-end tests and the registry tests. They must stay green through every ticket. A test
  that only wires the old port is rewritten against the new one, with the same assertion.
- The change expands, migrates and contracts. The old port and the new port run side by side
  until the last kind on `main` has moved.
- The wrapped kinds on the `feat/` branches move after this lands, on their own branches.

## Goals served

- G-7: a Foreign Agent is managed like any other, now under the same confinement as every kind.
- G-8: a new process type is a new Plugin, against a port that is plain data.
- Q-5: a remote configuration stays inside its Supervisor; a Plugin reaches only its own roots.
- Q-1: secure by default; every access a Plugin makes passes the core's checks.

## User stories

1. As an operator, I want my `collector` and `command` Supervisors to behave exactly as before,
   so that the revision costs me nothing.
2. As a Plugin author, I want to write my agent's integration as ordinary asynchronous code
   against a host, so that a multi-step integration stays readable.
3. As a Plugin author, I want to test my Plugin against a host in memory, so that I need no stub
   program and no file system.
4. As an operator with a process account, I want every program a Plugin runs to run under that
   account, so that a wrapped kind need not refuse it.
5. As a security reviewer, I want a Plugin to be unable to name a path outside its Supervisor's
   roots, so that a compromised configuration cannot steer it elsewhere.
6. As a maintainer, I want a test that fails when a kind's module reaches past the port, so that
   the boundary holds without a reviewer remembering it.
7. As a maintainer, I want the port's types to carry no host resource, so that a WebAssembly
   Plugin stays possible.

## Implementation decisions

- The port gains a manifest, the inputs, the host functions and an asynchronous handler, as the
  ADR decides. The core feeds inputs one at a time per Plugin instance and may cancel a handler.
- The core's executor of host functions sits beside the `Runner` and drives it. The `Runner`
  keeps spawn, watchdog, stop, apply gate, package swap, preflight and probe.
- The manifest absorbs what the kind states today: the kind defaults, the program key, the
  reload signal, the process group and the data directories. The version probe, which also
  serves as the preflight, stays with the instance, as `command` takes it from its block.
- File host functions resolve a root and a relative path through the no-follow adapter. The
  `state` root is a new directory in each Supervisor's directory, owned by the Client alone.
- A program a Plugin runs once goes the way of the version probe: through the second service
  where a process account is registered, started locally otherwise.
- The configuration entries reach a Plugin as an input with each entry's name, role and digest.
- Timers and host facts are host functions; time reaches a Plugin only as an input.
- The block parse stays two-staged; `check` and `check_delivered` stay on the port.
- The citations of the superseded Supervisor ADR in code, tests and other ADRs move to the
  clauses that now carry them, by the map in the commit that proposed the two ADRs.

## Testing decisions

- The seams stay the Client's existing ones: the process tests drive a Supervisor through its
  commands and events, the Supervisor tests run the Client, and the end-to-end tests run Server
  and Client.
- New port tests run a Plugin against a host in memory, copying the table style of the existing
  unit tests in the supervisor module.
- The boundary test extends the dependency-direction test with a role for Plugin modules.
- The port-data test reads the port's types, as the dependency-direction test reads source text.

## Out of scope

- The wrapped kinds `glpi`, `telegraf` and `icinga2`; they move on their branches afterwards.
- Loading a WebAssembly Plugin, and any WIT file.
- Take-over and any host function that reaches the network.
- Any change on the Server, the REST API or the Supervisor-set handling.

## ADR impact

- Binding: CLT-wrappers-act-through-the-core (proposed, reviewed, implemented here), which
  supersedes the ADR of Supervisor Mode and its kinds on acceptance, and
  CLT-server-manages-the-supervisor-set (proposed, unchanged behaviour), which restates its
  Server-managed set.
- Also binding: CLT-accounts-service-runs-under (second service, no-follow adapter, directory
  modes), PKG-signed-packages-and-delivery (swap, preflight, rollback), DEV-ports-and-adapters
  and DEV-six-crates-tokio-axum (module seams, dependency direction), SEC-secrets-wiped-on-drop
  (the token and key stay in the core), and the `NUC-` ADRs as the sensor lists them.
- No new ADR is needed before the tracer bullet.
