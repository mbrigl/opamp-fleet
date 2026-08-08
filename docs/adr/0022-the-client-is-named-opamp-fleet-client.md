# ADR-0022: The Client ships as `opamp-fleet-client` — the artifact, the installed binary, the version directory, and one service name on every platform

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **naming** decisions of two accepted ADRs, and nothing else in them:
[ADR-0010](0010-client-os-service.md)'s installed binary file name (`client` / `client.exe`)
and version-directory prefix (`opamp-client-…`), and
[ADR-0021](0021-release-pipeline-and-artifacts.md)'s artifact name
(`opamp-client-<version>-<os>-<arch>.7z`). Every mechanism those ADRs decide — the side-by-side
layout with its `current` pointer and manifest, the five targets, the `.7z` container — is untouched
and still binding. This ADR changes what the shipped thing is called, not how anything works.
[ADR-0005](0005-workspace-and-server-runtime.md)'s three-crate workspace, `crates/client` included,
is **not** touched: see decision point 1.

Supersedes the **service-label** decision of
[ADR-0010](0010-client-os-service.md) — `io.opamp-fleet.client.<instance>`, and the
human-readable Windows display name it promised. Everything else in it is untouched: the
per-instance isolation and the install layout.

## Context

The Client answers to four different names, depending on where one looks:

| Where | Name |
|---|---|
| Cargo package and binary target | `client` |
| The file the service runs (`layout::BINARY_FILENAME`) | `client` / `client.exe` |
| The version directory prefix (`layout::COMPONENT`) | `opamp-client` |
| The release artifact (`PRODUCT` in the release workflow) | `opamp-client` |
| The service label (ADR-0010) | `io.opamp-fleet.client.<instance>` |
| The Agent's default `service.name` (`config::default_name`) | `opamp-fleet-client` |

The last row has been right all along: `opamp-fleet-client` is what the Client calls itself *to the
fleet*, and it is what an operator reads in the fleet view. The names above it are shorter ones that
made sense while the repository was the only place the word appeared, and they have stopped making
sense on a host. A file called `client` is a file with no owner in its name, sitting next to a
hundred other programs; `ps` shows `client`; a support question says "the client crashed" and
conveys nothing. `opamp-client` and `opamp-fleet-client` differing by one word between the artifact
and the fleet view is the same defect one level up.

**The cost of this rename is entirely a function of when it happens, and right now it is zero.**
`git tag -l` is empty, the release workflow has never published, and no `.7z` bearing the old name
exists in any fleet. Every host that will ever run this Client will meet it under the new name.

That is worth stating as a hazard rather than a convenience, because the same change *after* a
release is a different decision entirely. Two facts make it so, and both are structural:

- **`BINARY_FILENAME` is what the service unit points at.** ADR-0010 registers the service against
  `<root>/current/client`. Renaming the file means every already-registered unit names a program
  that no longer exists in the next version directory. The ADR-0018 self-update would switch the
  pointer, exit for its restart, and come back to a unit whose program is gone — surviving only by
  its own rollback, three attempts later, on every host at once.
- **`BINARY_FILENAME` is also the member the self-update extracts** from an offered package
  (`selfupdate` asks `archive::extract_7z` for exactly that name). An already-deployed Client looks
  for `client` and would not find `opamp-fleet-client` in a new artifact — so the rename could not be
  delivered by the mechanism whose whole purpose is delivering new versions. Shipping it would have
  required an artifact carrying the program under *both* names for a release cycle, plus a
  compatibility entry in every version directory so the old unit kept resolving, plus a decision
  about when to withdraw both.

None of that has to be built, and none of it has to be maintained — provided the rename lands before
the first release. Deferring it is what makes it expensive.

ADR-0010 chose one reverse-DNS label, `io.opamp-fleet.client.<instance>`, and handed it to the
`service-manager` crate. What an operator actually sees is not that string, because the crate renders
a label through **two** different functions and the backends do not agree on which to use:

| | Rendering | Today's name |
|---|---|---|
| systemd | `{organization}-{application}` | `opamp-fleet-client.default.service` |
| launchd | `{qualifier}.{organization}.{application}` | `io.opamp-fleet.client.default` |
| Windows SCM | the same qualified form | `io.opamp-fleet.client.default` |

So the Client already has two names in the field, and neither was chosen — both fell out of how a
four-token label happens to be split (`io` / `opamp-fleet` / `client.default`). That the systemd unit
reads `opamp-fleet-client.default` is a coincidence of `organization` and `application` being joined
with a hyphen; it is not the shipped binary's name arriving there on purpose. An operator who learns
one name and moves to another platform has learned the wrong one, and every runbook has to say both.

Points 1 to 4 give the shipped artifact, the installed file, and the version directory a single name
for exactly this reason. A label is the identity of a registered service rather than a file name.
That explains *why the label may differ from the binary*; it does not answer why the label should
differ **from itself** across three platforms.

Two constraints shape what is possible.

**The two renderings can never coincide for a compound label.** One joins with `-`, the other with
`.`, so any label with more than one part produces two different strings. There is, however, a case
where they agree: with no qualifier and no organization, `to_qualified_name()` and `to_script_name()`
both reduce to the application alone. A label that is a **single token — no dots** — therefore
renders identically on all three backends. That is the whole mechanism points 8 to 11 need, and it
costs nothing: no fork per platform, no unit file of our own.

**A display name is not a thing all three platforms have.** Windows SCM has one, and
`service-manager` sets it to the service name with no way to override — the same class of gap
ADR-0018 found in that backend's restart policy, and closable the same way, with a post-install call
through the `windows-service` crate that is already a Windows dependency. systemd has `Description=`,
which the crate fills with the unit name; changing it means supplying the entire unit text through
`ServiceInstallCtx::contents` and owning a template per platform — precisely the "hand-write the
three backends" alternative ADR-0010 weighed and rejected. launchd has no display name at all; a job
is its `Label`.

## Decision

We will ship the Client as **`opamp-fleet-client`** — the artifact, the file installed on a host, and
the version directory that holds it — while the source tree keeps the crate it has. We will register
the service as **`opamp-fleet-client`** on systemd, launchd, and the Windows SCM alike, and give it
the display name **`OpAMP Fleet Client`** wherever the platform has one.

1. **The crate stays `client` at `crates/client`; the binary *target* is renamed.** These are two
   different kinds of name, and only one of them leaves the repository. A Cargo package name is a
   build-time identifier: it appears in `cargo run -p client`, in path dependencies, and in
   `CARGO_BIN_EXE_*`, and no operator ever sees it. What ships is the binary target, renamed with an
   explicit `[[bin]] name = "opamp-fleet-client"` — the mechanism Cargo has for exactly this. So
   `cargo build -p client` produces `target/release/opamp-fleet-client`, and the workspace's
   three-crate shape (ADR-0005) is untouched, as are the several hundred references to
   `crates/client/…` in the ADRs, the manual, and the README.

2. **The installed program file becomes `opamp-fleet-client` / `opamp-fleet-client.exe`**
   (`layout::BINARY_FILENAME`), so the service runs `<root>/current/opamp-fleet-client` and a package
   artifact carries its program under that member name.

3. **The version-directory prefix becomes `opamp-fleet-client`** (`layout::COMPONENT`):
   `<root>/versions/opamp-fleet-client-1.2.3-a1b2c3d/opamp-fleet-client`. The prefix repeats the file
   name, which reads redundantly and is correct: the directory names a *version of a component* and
   the file names a *program*, and ADR-0010's rule that a rebuild of the same commit maps to the same
   directory depends on the prefix being fixed rather than clever.

4. **The release artifact becomes `opamp-fleet-client-<version>-<os>-<arch>.7z`** (`PRODUCT` in the
   release workflow), and the release notes' example package name follows.

5. **The service label is the product's name too, the same on every platform.** It identifies a
   *registered service* rather than a file, and it is the single token `opamp-fleet-client`, with
   the instance as a suffix and the display name `OpAMP Fleet Client`. Points 8 to 11 decide it.

6. **`[self_update] package` is not touched by this.** The package name is the operator's choice in
   their own fleet (ADR-0018) — only the examples in the manual and in `config/client.toml` move to
   `opamp-fleet-client`, as examples.

7. **Superseded ADRs keep their text.** ADR-0010 and ADR-0021 still say `client` and `opamp-client`
   where they were written; they are accepted and immutable, and this ADR is the entry point that
   says what those names are today. Only the operator-facing documents — the manual, the README,
   `CHANGELOG.md`, `config/client.toml` — are rewritten to the new name.

8. **The label is a single token, so every backend renders it the same.** `label()` builds
   `ServiceLabel { qualifier: None, organization: None, application }` rather than parsing a
   dotted string. The unit is `opamp-fleet-client.service`, the launchd job and its plist are
   `opamp-fleet-client`, and the SCM service is `opamp-fleet-client`. One name in one runbook.

9. **The default instance carries the bare name; any other appends its own.** `--instance prod`
   registers `opamp-fleet-client-prod`. ADR-0010 says most hosts run exactly one instance, and that
   host should show the product's name and nothing else; the suffix appears only where an operator
   deliberately asked for a second Client. A hyphen rather than a dot, because a dot would split the
   label again and undo point 8 — and the instance-name grammar (lowercase, digits, `-`) already
   guarantees the result is a legal unit name, launchd label, and SCM name.

10. **The display name is `OpAMP Fleet Client`**, and `OpAMP Fleet Client (prod)` for a named
    instance. It is set where it exists:
    - **Windows:** after `install`, with `sc.exe config <name> displayname= "…"` — in the same
      module (`windows_config`) that already closes this backend's restart-policy gap. Deliberately
      *not* the `windows-service` crate's `Service::change_config`: it maps onto
      `ChangeServiceConfigW` with every field taken from a `ServiceInfo`, so setting one field means
      restating the executable path, the launch arguments, the start type and the account exactly as
      registered — and one of them wrong rewrites the registration instead of the name. `sc config`
      changes the field it is given and leaves the rest alone, and it is also how the service was
      created, since the backend is the `sc.exe` wrapper. This makes ADR-0010's promise of a readable
      name in the services list true for the first time; it had never been implemented.
    - **systemd:** `Description=` is whatever the crate writes, which is the unit name — now
      `opamp-fleet-client`, the product's name rather than a mangled label. Making it the prose form
      would mean owning the unit template, and this ADR does not take that step.
    - **launchd:** nothing to set. A job is its label; there is no second name to give it.

11. **This renames an installed service, and there is no migration.** A service registered under the
    old label is not found under the new one. `service uninstall` with the *old* binary, then
    `service install` with the new one. Nothing has been released, so this is a developer-machine
    chore rather than a fleet operation — and it is the last moment that is true, for the same reason
    the Context gives about names in general.

## Alternatives considered

- **Rename the Cargo package and `crates/client` too, for a single name end to end** — the tidier
  story, and it buys an operator nothing: it would rewrite every path in every ADR, the manual, the
  README, and every `cargo run -p client` a contributor has in muscle memory, to change an
  identifier that never appears outside the build. The split in decision point 1 is deliberate:
  internal identifier, shipped name, and only the second one has to be right on a host.
- **Rename only the artifact, leaving the installed file `client`** — the smallest possible change,
  and it misses the place the name actually matters. The artifact is read once, at download; the
  file on disk is what `ps`, the unit, and the support conversation see for years.
- **Defer until after 1.0 and carry a transition** (both member names in the artifact, a
  compatibility entry in every version directory, a withdrawal release) — the mechanism is
  described in the Context and is entirely avoidable. Deferring buys nothing and costs a migration
  that would have to be right on every host at once.
- **A shorter product name (`opampc`, `ofc`, `fleetd`)** — shorter is genuinely better for something
  typed often, and none of these is what the Client already reports to the fleet. Matching
  `default_name` is the whole point; inventing a fifth name to fix four is not.
- **Keep `opamp-client` (the artifact's current name) as the one shipped name** — one word shorter,
  and the wrong word: the fleet view, the default `service.name`, the service label, and the
  repository are all `opamp-fleet`. The odd one out should move.
- **Keep the label `io.opamp-fleet.client.<instance>` while the file is renamed** — it already
  carries the product, it identifies a *registered service* rather than a file, and the reverse-DNS
  segment is not the binary's name. Changing it orphans every service any developer has installed,
  and a reader of `systemctl status` already gets the product from the program path beside it.
  Rejected because it keeps the two names the label renders to, and neither was chosen.
- **Keep reverse-DNS on launchd and the plain name elsewhere** — Apple's convention for a
  `LaunchDaemon` label is reverse-DNS, and following it is why the label looked like this at all.
  Rejected because it is the status quo restated: the operator still has two names, and the one
  advantage — matching a convention — is worth less than a name that is the same everywhere. The
  convention is a recommendation about collision avoidance, and `opamp-fleet-client` does not collide.
- **Always append the instance (`opamp-fleet-client-default`)** — one rule instead of two shapes,
  and it puts a word in front of every operator that carries no information on the hosts that run one
  Client, which is nearly all of them.
- **Own the unit, plist, and SCM registration ourselves** to control every field, `Description=`
  included — the maximal-control option ADR-0010 rejected, with three code paths to maintain. It
  buys a prose description on systemd and nothing else points 8 to 11 want.
- **Change nothing and document the three names** — what the manual should have done all along, and
  the reason for points 8 to 11: the names were never chosen, and documenting an accident makes it
  permanent.

## Sources / Prior art

- The repository's own state at the time of writing: `git tag -l` empty and no published release,
  which is what makes the compatibility mechanism described in the Context unnecessary.
- Comparable agents ship under their product's full name rather than a role noun — `elastic-agent`,
  `otelcol` / `otelcol-contrib`, `vector`, `datadog-agent`, `telegraf`. None of them installs a file
  called `client` or `agent` alone, for the reason in the Context: a host has many.
- Cargo's [`[[bin]]` target configuration](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#configuring-a-target)
  — a binary's name is a property of the target, not of the package, which is what makes decision
  point 1 a one-line change rather than a repository-wide rename.
- [ADR-0010](0010-client-os-service.md) §"Versioned install layout" for the directory and
  pointer naming this changes, and [ADR-0021](0021-release-pipeline-and-artifacts.md) §4 for the
  artifact naming — the two decisions whose *shape* is kept and whose *strings* move.
- `service-manager` 0.11.0, `ServiceLabel::to_qualified_name` and `to_script_name` — the two
  renderings, and the single-token case where they agree; `systemd::make_service`, which fills
  `Description=` with the unit name; and the `sc` backend, which sets `displayname=` to the service
  name.
- `windows-service` 0.8.1, `Service::change_config` — the API that looks like the way to set a
  display name and is not, because it restates the whole registration; and `sc.exe config`, which
  changes one field.
- What comparable agents register: `elastic-agent`, `datadog-agent`, `vector`, `telegraf` — a plain
  product name on every platform, not a reverse-DNS label, and a prose display name on Windows.
- [ADR-0018](0018-client-self-update.md)'s Windows section — the precedent for reaching past this
  crate with `windows-service` when a backend cannot express what the decision requires.

## Consequences

- **Positive:** everything an operator touches carries one name — the download, the file on disk, the
  version directory, the process in `ps`, and the Agent in the fleet view. A support conversation has
  a noun. The rename is free exactly once, and this is that moment.
- **Positive:** one service name to know, to document, and to type — `systemctl status
  opamp-fleet-client`, `launchctl list opamp-fleet-client`, `sc query opamp-fleet-client`. The
  Windows services list gets the readable name ADR-0010 promised and never delivered. The service
  name matches the binary, the artifact, and the Agent in the fleet view.
- **Negative / trade-offs:** the repository now uses two names for one component on purpose — `client`
  inside the build, `opamp-fleet-client` everywhere it ships. That is a thing a newcomer has to be
  told once, and decision point 1 is where they are told. Anyone with a locally installed development
  service re-runs `service install`: a service registered under the old layout points at a program the
  new build no longer produces, so it is deregistered and re-registered rather than upgraded. Accepted
  ADRs now disagree verbally with the code they govern, which is the standing cost of an immutable
  record and the reason for point 7.
- **Negative / trade-offs:** a `LaunchDaemon` label that is not reverse-DNS departs from Apple's
  convention — accepted deliberately, and noted here so nobody re-derives it as a mistake. Every
  developer machine with an installed service must uninstall and reinstall. systemd's `Description=`
  stays a name rather than a sentence until someone owns the unit template. Two label shapes exist
  (bare, and suffixed for a named instance) instead of one.
- **Follow-ups:** for the shipped name, none required. If the name ever changes again after a
  release, the transition sketched in the Context — the artifact carrying two member names, and a
  compatibility entry beside the renamed binary in each version directory — is the shape that
  decision has to take, and it should be its own ADR. For the service name, the manual documents no
  unit name at all today and should carry the per-platform table this decision collapses into one
  row; `service install` prints the label and should print the name the platform will actually show.
  Whether to own the unit and plist text — for `Description=`, and for the systemd hardening
  directives a reviewer will eventually ask about — is a separate decision, and a larger one.
