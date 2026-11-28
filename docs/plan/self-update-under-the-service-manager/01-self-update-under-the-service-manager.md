# Ticket: The smoke test updates the installed service in place, and rolls a failing version back

## What to build

No test runs the Client's self-update under a real service manager. Two tests each cover half
of it:

- `self_update_e2e.rs` swaps the binary for real, but the test restarts the Client itself.
- `service_smoke.rs` drives systemd, launchd and the Windows service manager, but it never updates
  anything. It restarts after a crash, not after the deliberate exit code 10 of
  [CLT-self-update-signed-package §8](../../adr/CLT-self-update-signed-package.md).

Rollback is tested only in one process, in `a_new_version_that_does_not_stay_up_is_rolled_back`
(`installer.rs`), which calls `on_start_as` again in place of a restart.

The service smoke test goes on to update the installed service. A Server offers the running
Client a signed package of a newer version of itself. The Client installs it and exits with
code 10, the service manager starts the new version, and the new version commits. A second round
offers a version that does not stay up. The service manager restarts it until the Client rolls
back to the version before, which reports the update failed. This runs on Linux, macOS and Windows
in `service-smoke.yml`, weekly and by hand, as the smoke test runs today.

## Acceptance criteria

- [ ] Given the Client installed and running as a service, when its Server offers a signed package
  of a newer Client version, then the service manager restarts the Client after its exit with
  code 10. The new version reports itself installed, and `current` points at it. This holds on
  Linux, macOS and Windows.
- [ ] Given the Client updated as above, when its Server offers a newer version that exits at once
  on every start, then the Client is back on the previous version within the probation the ADR
  sets. It is still running as the service, and it reports the update failed.
- [ ] Given the extended smoke test, when `service-smoke.yml` runs, then it runs on all three
  platforms, and on no push or pull request.
- [ ] Given the Enforcement of CLT-self-update-signed-package, when it is read against
  `service-smoke.yml`, then its mismatch is listed for the next normalization. The ADR says the
  smoke test runs weekly on Windows alone and covers the restart of clause 8. The ADR is accepted
  and is not edited here.

## Read first

- Touches: `crates/fleet-agent/tests/service_smoke.rs`, `.github/workflows/service-smoke.yml`,
  `scripts/check-service-smoke.sh`, and possibly `crates/fleet-agent/tests/self_update_e2e.rs`
  for the build of a newer Client and a signed package it can share.
- [CLT-self-update-signed-package §8](../../adr/CLT-self-update-signed-package.md) and the
  Enforcement of that ADR, and
  [CLT-installed-service-and-state](../../adr/CLT-installed-service-and-state.md) on the service
  units.
- The golden path: `the_client_installs_a_version_of_itself_and_reports_it_installed` in
  `self_update_e2e.rs`, for the Server, the signed package and the newer build. The seam: the
  installed service of `service_smoke.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human. The test registers system services and accounts and needs elevation, so it runs
only on CI runners or a throwaway machine (`FLEET_SMOKE_THROWAWAY=1`). Its proof is a run of
`service-smoke.yml`, which only the human starts.
