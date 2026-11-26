# Ticket: The host functions a wrapped kind needs

## What to build

The full vocabulary of clause 4 beyond the minimum, so a wrapped kind can move to the port:

- reading, writing, renaming and removing files, and listing and creating directories, under the
  roots `config`, `program`, `data` and `state`, through the no-follow adapter;
- the new `state` directory in each Supervisor's directory, the Client's alone;
- running a program from the `program` root once, through the second service where a process
  account is registered, with its exit status and output;
- timers that arrive as inputs;
- host facts, the fully qualified host name and the account a program runs under among them.

No kind on `main` needs all of them; a test Plugin in the test tree exercises each.

## Acceptance criteria

- [ ] Given a test Plugin, when it writes and reads a file under `state` and `data`, then the
      files exist with the owning account, group and mode CLT-accounts-service-runs-under §21
      names, and `state` is the Client's alone.
- [ ] Given a test Plugin, when it names a path with `..`, an absolute path or a link out of its
      root, then that call fails with a reason and the Plugin keeps running.
- [ ] Given a test Plugin, when it writes under `config` or `program`, then the call is refused.
- [ ] Given a registered process account, when a test Plugin runs a program once, then the
      program runs through the second service under that account.
- [ ] Given a test Plugin that sets a timer, when the timer fires, then the Plugin receives it as
      an input, and a cancelled timer never arrives.
- [ ] Given a test Plugin, when it asks for the host facts, then it receives the host name and
      the account its programs run under.

## Read first

- Touches:
  `crates/fleet-agent/src/supervisor/ports.rs`,
  `crates/fleet-agent/src/supervisor/process.rs`,
  `crates/fleet-agent/src/supervisor/channel.rs`,
  `crates/fleet-agent/src/supervisor/mod.rs`,
  `crates/fleet-agent/src/storage.rs`,
  `crates/fleet-util/src/nofollow.rs`,
  `crates/fleet-agent/tests/supervisor_process.rs`
- ADR: CLT-wrappers-act-through-the-core §1, §3, §4;
  CLT-accounts-service-runs-under §15, §18, §20, §21, §24.
- Glossary: Process account, Second service.
- Golden path: the version probe through the second service, and the Client's state kept
  through the no-follow adapter.
- Seam: the process tests, with the second-service tests the version probe already has.

## Blocked by

- docs/plan/plugins-act-through-the-core/01-command-runs-on-the-new-port.md

## Mode

with the human: it touches the second service and the directory modes, which
CLT-accounts-service-runs-under keeps under a security review.
