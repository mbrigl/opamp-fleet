# ADR-0028: The Client ships as `opamp-fleet-client` — the artifact, the installed binary, and the version directory

- **Status:** ⚪ superseded by [ADR-0082](0082-the-fleets-own-agent-is-called-supervisor.md)
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **naming** decisions of two accepted ADRs, and nothing else in them:
[ADR-0010](0010-client-os-service-and-cli.md)'s installed binary file name (`client` / `client.exe`)
and version-directory prefix (`opamp-client-…`), and
[ADR-0025](0025-release-pipeline-and-artifacts.md)'s artifact name
(`opamp-client-<version>-<os>-<arch>.7z`). Every mechanism those ADRs decide — the side-by-side
layout with its `current` pointer and manifest, the five targets, the `.7z` container — is untouched
and still binding. This ADR changes what the shipped thing is called, not how anything works.
[ADR-0005](0005-workspace-and-server-runtime.md)'s three-crate workspace, `crates/client` included,
is **not** touched: see decision point 1.

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
  that no longer exists in the next version directory. The ADR-0020 self-update would switch the
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

## Decision

We will ship the Client as **`opamp-fleet-client`** — the artifact, the file installed on a host, and
the version directory that holds it — while the source tree keeps the crate it has.

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

5. **The service label does not change.** It is `io.opamp-fleet.client.<instance>` and already
   carries the product; it identifies a *registered service* rather than a file, the reverse-DNS
   segment is not the binary's name, and changing it would orphan every service any developer has
   installed while buying nothing a reader of `systemctl status` does not already get from the
   program path beside it.

6. **`[self_update] package` is not touched by this.** The package name is the operator's choice in
   their own fleet (ADR-0020) — only the examples in the manual and in `config/client.toml` move to
   `opamp-fleet-client`, as examples.

7. **Superseded ADRs keep their text.** ADR-0010 and ADR-0025 still say `client` and `opamp-client`
   where they were written; they are accepted and immutable, and this ADR is the entry point that
   says what those names are today. Only the operator-facing documents — the manual, the README,
   `CHANGELOG.md`, `config/client.toml` — are rewritten to the new name.

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

## Sources / Prior art

- The repository's own state at the time of writing: `git tag -l` empty and no published release,
  which is what makes the compatibility mechanism described in the Context unnecessary.
- Comparable agents ship under their product's full name rather than a role noun — `elastic-agent`,
  `otelcol` / `otelcol-contrib`, `vector`, `datadog-agent`, `telegraf`. None of them installs a file
  called `client` or `agent` alone, for the reason in the Context: a host has many.
- Cargo's [`[[bin]]` target configuration](https://doc.rust-lang.org/cargo/reference/cargo-targets.html#configuring-a-target)
  — a binary's name is a property of the target, not of the package, which is what makes decision
  point 1 a one-line change rather than a repository-wide rename.
- [ADR-0010](0010-client-os-service-and-cli.md) §"Versioned install layout" for the directory and
  pointer naming this changes, and [ADR-0025](0025-release-pipeline-and-artifacts.md) §4 for the
  artifact naming — the two decisions whose *shape* is kept and whose *strings* move.

## Consequences

- **Positive:** everything an operator touches carries one name — the download, the file on disk, the
  version directory, the process in `ps`, and the Agent in the fleet view. A support conversation has
  a noun. The rename is free exactly once, and this is that moment.
- **Negative / trade-offs:** the repository now uses two names for one component on purpose — `client`
  inside the build, `opamp-fleet-client` everywhere it ships. That is a thing a newcomer has to be
  told once, and decision point 1 is where they are told. Anyone with a locally installed development
  service re-runs `service install`: a service registered under the old layout points at a program the
  new build no longer produces, so it is deregistered and re-registered rather than upgraded. Accepted
  ADRs now disagree verbally with the code they govern, which is the standing cost of an immutable
  record and the reason for point 7.
- **Follow-ups:** none required. If the name ever changes again after a release, the transition
  sketched in the Context — the artifact carrying two member names, and a compatibility entry beside
  the renamed binary in each version directory — is the shape that decision has to take, and it
  should be its own ADR.
