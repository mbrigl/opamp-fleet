# Ticket: An uninstall reaches the Plugin after the core has stopped the Managed Process

## What to build

[CLT-wrappers-act-through-the-core §9](../../adr/CLT-wrappers-act-through-the-core.md) orders an
uninstall: the core stops the Managed Process, the Plugin then receives the uninstall as an input,
undoes within its roots what installing did, and answers. The Client logs an `Err` answer with
what was left undone, and the removal goes on.

The core on `main` does it the other way round. `supervisor::host::front` turns
`ProcessCommand::Uninstall` into `Input::Uninstall` while the Managed Process still runs. The host
function `uninstall` sends `ProcessCommand::Uninstall` to the `Runner`, which stops the process
and answers `ProcessEvent::Uninstalled { result: Ok(()) }` at once. So:

- a Plugin that undoes something before it calls `uninstall` acts on a running agent;
- a Plugin that undoes something after it calls `uninstall` races the purge of its directory,
  which follows the answer;
- an `Err` a Plugin returns never reaches the answer, which is always `Ok`.

No kind on `main` overrides the uninstall, so nothing an operator sees changes today. The wrapped
kinds move onto this port next, on their `feat/` branches, and `icinga2` undoes its enrolment
there.

After this ticket, the core stops the Managed Process first, then feeds the instance
`Input::Uninstall`, and answers the uninstall with the instance's outcome once the handler has
returned. The default handling answers `Ok`. The purge follows the answer, as before.

Evidence: the first review of PR #231 found the ADR ahead of the code on the host functions
(at ff170c6de). The rework 0ce532ad7 adapted the ADR's text to the port as built, and left
clause 9 as it was, unbuilt.

## Acceptance criteria

- [ ] Given a Supervisor whose Plugin records whether its Managed Process still runs when
      `Input::Uninstall` arrives, when the core uninstalls it, then the process has stopped
      before the input arrives.
- [ ] Given a Plugin whose handler of `Input::Uninstall` writes a file in `state` and then
      answers, when the core uninstalls it, then `ProcessEvent::Uninstalled` follows the write.
- [ ] Given a Plugin whose handler of `Input::Uninstall` returns `Err("left x")`, when the core
      uninstalls it, then `ProcessEvent::Uninstalled` carries that `Err`, the Client logs it, and
      the Supervisor's directory is purged all the same.
- [ ] Given `collector` or `command`, when the core uninstalls it, then
      `an_uninstall_stops_the_process_answers_and_exits` and
      `retiring_uninstalls_the_removed_and_only_stops_the_changed` pass unchanged.

## Read first

- Touches: `crates/fleet-agent/src/supervisor/host.rs`,
  `crates/fleet-agent/src/supervisor/port.rs`, `crates/fleet-agent/src/supervisor/process.rs`,
  `crates/fleet-agent/src/supervisor/ports.rs`, `crates/fleet-agent/tests/wrapped_supervisors.rs`
- ADR: CLT-wrappers-act-through-the-core §1, §4, §5 and §9; PKG-versions-high-water-mark-floor for
  the purge that follows the answer.
- Golden path: `the_core_stops_the_process_on_an_uninstall_the_plugin_left` and the test Plugin of
  `crates/fleet-agent/tests/wrapped_supervisors.rs`.
- Seam: the host tests in `supervisor/host.rs` and the test Plugin of the test tree.

## Blocked by

- None (can start immediately)

## Mode

with the human: the host function `uninstall` changes its meaning from "stop and answer" to
"answer". The human may prefer to supersede clause 9 instead, and decides before the work starts.
