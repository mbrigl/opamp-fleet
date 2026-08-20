# ADR-0080: The program is `supervisor`, and so is its service and its configuration file

- **Status:** ⚪ superseded by [ADR-0082](0082-the-fleets-own-agent-is-called-supervisor.md)
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

Supersedes [ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) on the shipped program's name
and [ADR-0030](0030-one-service-name-on-every-platform.md) on the service's, and amends
[ADR-0010](0010-client-os-service-and-cli.md) and
[ADR-0027](0027-interactive-install-writes-the-first-configuration.md) on the configuration file's
name and [ADR-0048](0048-the-packaged-cli-is-a-symlink-through-current.md) on what the PATH symlink
is called. The *shape* every one of them decided — one name on every platform, the versioned layout,
the symlink through `current`, the file the installer writes — is untouched. Only the string changes.

## Context

[ADR-0077](0077-the-clients-own-agent-type-is-supervisor.md) made `supervisor` the Agent type this
program reports, and [ADR-0078](0078-a-release-is-named-after-the-set-it-becomes.md) made it the name
of the package that carries it and of every published artifact. What is left under the old name is
the part an operator actually touches: the program in `ps`, the unit in `systemctl`, the file in
`/etc`. A fleet whose Server calls this thing `supervisor` everywhere and whose hosts call it
`opamp-fleet-client` everywhere is the doubled vocabulary ADR-0028 set out to end, arrived at from
the other side.

**ADR-0028 wrote the warning this decision has to answer.** It said the cost of renaming the program
"is entirely a function of when it happens, and right now it is zero" — `git tag -l` was empty. It is
not zero now: fourteen releases are out, and both hazards ADR-0028 named are real.

- **The service unit points at the file.** A registered unit runs `<root>/current/opamp-fleet-client`
  (ADR-0010). If the next version directory holds only `supervisor`, a self-update moves the pointer,
  exits for its restart, and the service manager starts a program that is not there. The ADR-0020
  rollback does not save it: the rollback runs *in the new version*, which never starts.
- **The self-update extracts the file by name.** `install::write_program` asks the artifact for the
  member `BINARY_FILENAME`. A deployed Client asks for `opamp-fleet-client`, a renamed artifact holds
  `supervisor`, and the install fails — loudly, and without touching the running binary.

So the rename cannot be delivered by the mechanism whose whole purpose is delivering new versions.
That is a fact about this change, not a defect to design around: what is left to decide is whether to
carry both names for a transition or to say plainly that this one release is installed the way the
first one was.

**The configuration file has the same question with a worse failure mode.** A Client that cannot find
its configuration does not fail — it comes up on defaults, dials the development endpoint, and
manages nothing (ADR-0027). A host whose `client.toml` is suddenly not the file being looked for
would go quiet in exactly the way that is hardest to notice.

## Decision

We will rename the program, its service, its layout and its configuration file to **`supervisor`**,
as a **clean break** with no compatibility name, and make the one silent failure loud.

1. **The program is `supervisor`** (`supervisor.exe` on Windows): the binary Cargo builds, the file
   in every version directory, the member a package artifact carries, the payload under
   `/usr/libexec`, and the `PATH` symlink ADR-0048 lays through `current`.
2. **The service is `supervisor`** on systemd, launchd and the SCM, with the instance suffixed as
   before (`supervisor-prod`) — ADR-0030's rule, with its string replaced. The version directories
   follow the same constant: `supervisor-<MAJOR.MINOR.PATCH>-<hash>`.
3. **The configuration file is `supervisor.toml`**, and the `--config` default with it. There is no
   fallback to the old name — and because there is none, **a Client that finds a `client.toml` beside
   the `supervisor.toml` it was looking for refuses to start**, naming both paths. Coming up on
   defaults there would be the one outcome nobody would see.
4. **The break is not carried across.** No dual-named artifact, no compatibility link in a version
   directory, no reading of the old configuration name. A deployed Client offered this release
   refuses it (the member it asks for is not in the artifact) and stays on the version it runs, which
   is the safe half of ADR-0028's hazard and the only half that survives this decision.
5. **This release is installed, not updated**: by `.deb`, `.rpm` or MSI, or by hand. Every one of
   those paths re-runs `service install`, which registers the new service — and the Linux
   post-install additionally **deregisters the old one**, because the package's own pre-removal
   deliberately falls through on an upgrade and the old unit would otherwise survive, pointing at a
   file the new layout does not have. It also removes the old `PATH` symlink and the orphaned
   `opamp-fleet-client-*` version directories, the way it already does the ADR-0053 migration.
6. **The default `name` moves too, but not to this word.** The top-level key an operator sets to
   say *which* Client this is (ADR-0033) defaulted to `opamp-fleet-client`, which after this decision
   is on no file, no service and no artifact. It becomes **`Supervisor Agent`** — a display name,
   spaces and capitals included. Not `supervisor`: that is the Agent *type*, and a default equal to
   it would print the same word in both columns of the fleet view, which is the collapse ADR-0033
   ended. Nothing resolves a path or a service from this key — the ADR-0010 grammar governs
   `--instance` and the `[[supervisor]]` block names — so a name that reads like one is free.
7. **What keeps its name, and why:**

   | Stays | Because |
   |---|---|
   | the install roots, `/opt/opamp-fleet/client/<instance>` and `/var/lib/opamp-fleet/client/<instance>` (ADR-0010, ADR-0053) | the state directory holds the instance UID and the configuration; moving it makes every host a *new* Agent in the fleet view and loses the credential an operator typed |
   | the dpkg and rpm package identity, the MSI `ProductName` and `UpgradeCode` (ADR-0046) | an `apt`, `dnf` or MSI upgrade stays an upgrade rather than becoming a second product beside the first |
   | the Cargo package name `client` | a build-time identifier that never leaves the repository (ADR-0028's own reasoning, undisturbed) |

## Alternatives considered

- **A transitional release carrying both names** — the artifact holding the program twice, each
  version directory laying the old name beside the new, the loader accepting either configuration
  file, and the old name withdrawn a release later. It is the only option under which a deployed host
  updates itself across the rename, and it was rejected deliberately: it re-creates precisely the
  defect ADR-0028 removed — one file with two names — in the packer, the layout, the loader and the
  documentation at once, and it ends in a second decision about when to withdraw the old name that
  every host has to survive as well. One break, announced, is cheaper than two.
- **Let the installers rename `client.toml`.** Tempting, and it works exactly where the OS package
  performs the upgrade. Rejected because that is not every host: one updated by hand or by a fleet
  self-update would be left without a configuration while the release notes said the installer
  handles it — two classes of host and one sentence covering both.
- **Rename the program but keep the service and the layout** (ADR-0030's string). Rejected: the unit
  name is what an operator types every day, and a service called `opamp-fleet-client` running a
  program called `supervisor` is the doubled vocabulary this decision exists to end.
- **Rename everything, package identity included.** Rejected: it buys consistency in a place nobody
  reads and costs every host a second package beside the first, with the old one left installed.
- **Keep `opamp-fleet-client`.** The status quo, and it is not broken. Rejected: the fleet's own
  vocabulary has moved (ADR-0077, ADR-0078), and the only names left on the old string are the ones
  an operator sees most.

## Sources / Prior art

- [ADR-0028](0028-the-client-is-named-opamp-fleet-client.md) — the naming decision this replaces, and
  the two hazards it recorded for exactly this moment. They are quoted above because they are the
  reason this is a break rather than an update.
- [ADR-0030](0030-one-service-name-on-every-platform.md) — one service name everywhere, kept in
  shape and changed in string; its collision argument is revisited under Consequences.
- [ADR-0010](0010-client-os-service-and-cli.md), [ADR-0048](0048-the-packaged-cli-is-a-symlink-through-current.md),
  [ADR-0053](0053-the-linux-service-executes-from-opt.md) — the layout, the symlink and the roots
  this leaves in place.
- [ADR-0077](0077-the-clients-own-agent-type-is-supervisor.md),
  [ADR-0078](0078-a-release-is-named-after-the-set-it-becomes.md) — the two decisions that already
  moved the fleet's side of the vocabulary, and which this one completes on the host's side.

## Consequences

- **Positive:** one word for one thing, from the Agent type in the fleet view down to the unit an
  operator restarts. Every runbook line, every artifact name and every attribute now says
  `supervisor`.
- **Negative — every deployed host must be touched once, and nothing else will do.** A self-update
  cannot cross this release: an old Client refuses the artifact and stays where it is. The upgrade is
  `apt`/`dnf`/MSI or a manual `service install`, and on each host the configuration file has to be
  renamed to `supervisor.toml` by hand. A host that is upgraded and not renamed does not start — by
  design (point 3), because the alternative is a Client that runs, reports nothing, and manages
  nothing.
- **Negative — `supervisor` is a common word in `/usr/bin` and in `systemctl`.** ADR-0030 chose the
  old name partly because it "does not collide"; this one might. It does not collide with the
  best-known claimant — Debian's `supervisor` package installs `supervisord` and `supervisorctl`, not
  `supervisor` — but the margin is thinner than before, and a host running both will have two things
  an operator could mean by the word. The instance suffix (`supervisor-prod`) is unchanged and still
  the way two of ours are told apart.
- **Negative — the term now names three things in this project**: the unit inside a Client that
  manages one Managed Process (the specification's *Supervisor*), the Agent type (ADR-0077), and now
  the program and its service. Documentation keeps them apart the way ADR-0077 already requires:
  `service.name = "supervisor"` for the type, `[[supervisor]]` for the block, and plain
  `supervisor` for the program only where a file or a unit is meant.
- **Follow-ups:** the release after this one is an ordinary self-update again, and the first that
  proves it end to end — worth watching on a real host rather than only in the test that stands in
  for the service manager.
