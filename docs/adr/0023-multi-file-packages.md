# ADR-0023: A package may be a directory tree, unpacked whole beside the one it replaces

- **Status:** 🟢 accepted
- **Date:** 2026-08-07
- **Deciders:** Markus Brigl

## Context

[ADR-0015](0015-package-delivery-for-managed-processes.md) delivers a Managed Process's program, and
[ADR-0018](0018-packages-imported-from-a-url.md) lets that program arrive inside an archive the Agent
opens. Both assume the program is **one file**: `install_executable` in
`crates/client/src/supervisor/process.rs` extracts exactly one member — the one whose file name
equals the configured program's — and renames it over `<supervisor_dir>/<name>/program/<file>`.

That assumption holds for a Go or Rust agent and for the Collector, which is why it was reasonable.
It excludes an entire class of agent, and Fluent Bit is the case that makes it concrete: what
upstream ships for Linux is a `.deb`/`.rpm` installing an executable *plus* the shared objects and
plugins it loads. `config/client.toml` already says so, and points the operator at an absolute path
and the machine's package manager. That is an honest workaround and a real gap: the fleet can
configure Fluent Bit centrally, watch its health centrally, and roll its configuration back
centrally — and then needs configuration management to put the binary there in the first place, on
every host, for every version.

Building it statically is not the answer available to us. `FLB_STATIC_BINARY` exists only as a draft
pull request open since 2020 (`fluent/fluent-bit#2558`), unmerged, with unresolved GPL concerns
about static linking, and it drops LuaJIT and SQLDB. Third-party static builds exist; asking an
operator to swap the upstream artifact for someone else's rebuild in order to be managed is a worse
bargain than the absolute path they have today.

Four forces shape what may be done about it:

- **The protocol does not constrain this.** The Baseline is explicit that "the content of the file,
  functionality provided by the packages, how they are stored and used by the Agent side is Agent
  type-specific and is outside the concerns of the OpAMP protocol". A package that unpacks into a
  tree is entirely within it.
- **ADR-0021 made the written shape of the program path the whole of a Supervisor's consent** to
  being updated. Whatever this adds must not make that consent depend on a value the file does not
  show.
- **Today the archive never chooses a path**, which is exactly why there is no traversal defence to
  get wrong: one member, one destination the Client picked. Unpacking a tree gives the archive a say
  in where bytes land, and that is a security boundary this decision creates rather than inherits.
- **A failed update must still be undoable.** The current rollback is a sibling rename
  (`<binary>.rollback`) — atomic, free, and available because there is exactly one file.

## Decision

We will let a package be a **directory tree**, unpacked whole **beside the one it replaces** and
swapped in by renaming directories — the same move the single-file path already makes with
`<binary>.rollback`, one level up. The principle is the one
[ADR-0010](0010-client-os-service-and-cli.md) uses for the Client's own versions and
[ADR-0020](0020-client-self-update.md) trusts for replacing a running program: build the new one
somewhere else entirely, switch by a single atomic operation, and keep what ran until the new one
has proved itself.

1. **A `[[supervisor]]` block gains one optional key, `program_path`** — a relative path *inside*
   the package, e.g. `bin/fluent-bit`. Absent, the layout is what it is today: one member, one file.
   Present, **every member of the archive is extracted**, each keeping its own relative path, and
   `program_path` says which of them is the program.

   `binary`/`command` keeps its ADR-0021 meaning untouched: a bare file name still means "this
   Supervisor's own directory, and therefore it takes packages", an absolute path still means the
   machine's. Consent stays readable in exactly the place it is read today; `program_path` says
   *where inside* the delivered tree, never *whether*. Tree mode is triggered by that key rather
   than by what the archive happens to hold, so what a host will run is readable in the
   configuration before any artifact exists.

2. **`program_path` matches a member by its trailing path components, not from the archive root.**
   A release tarball almost always wraps everything in one version-named directory —
   `fluent-bit-3.1.0/bin/fluent-bit` — and a `program_path` that had to name it would be wrong at
   the next release, silently, which is the failure ADR-0022 exists to make unspellable. So
   `program_path = "bin/fluent-bit"` matches any member whose path *ends* with those components,
   exactly as today's single-member rule matches a file name "wherever the archive keeps it". More
   than one match is refused, naming the candidates, and answered by writing more of the path.

3. **The tree is unpacked to `<supervisor_dir>/<name>/program/tree/`**, keeping whatever directory
   structure the archive has below the stripped prefix, and the tree it replaced is kept beside it
   as `program/tree.rollback`. The Managed Process is spawned from `program/tree/<program_path>` —
   a path that follows from the configuration alone, so it is known at startup, before any package
   has ever arrived. The new tree is built in `program/.staging` and moved into place by a single
   rename, so the live name is either the old tree or the new one and never a mixture; a failed
   install renames the previous one back.

   Two fixed names rather than a version-named directory and a `current` pointer, which is what
   this ADR first proposed: a directory rename is atomic on every platform this Client runs on,
   while a pointer is a symlink on Unix and a junction on Windows — machinery ADR-0010 runs once,
   as an Administrator, at install time, and which would here run on every package. It is also the
   move the single-file swap already makes (`<binary>.rollback`), so there is one mechanism to
   understand rather than two. What is lost is the version being legible on disk; the Agent reports
   it, which is where an operator reads it anyway.

4. **The archive's paths are sanitized, and every member is bounded.** A member is refused, and the
   whole install with it, when its path is absolute, contains a `..` component, or is a symlink or
   hard link. Extraction is additionally bounded by a total byte count and a member count, not only
   the per-member limit that exists today. Refusing the install is the only correct answer: a
   partially unpacked agent is worse than none.

5. **File modes come from the archive on Unix, plus `program_path` is always made executable.** A
   tree carries its own modes and a `tar` preserves them; the one thing that must not depend on how
   the archive was built is whether the program can be executed at all.

   A `.7z` is the exception: it stores Windows attributes, and a Unix mode survives in them only by
   a convention this Client will not bet an agent's executability on. A tree packed as `.7z` gets
   its program made executable and nothing else, so an agent that ships helper executables beside
   its program is a reason to use `.tar.gz` — the format upstream releases use anyway.

6. **Nothing changes for a single-file package.** No `program_path`, no tree, no `tree.rollback` —
   the existing path stays exactly as it is, including its own rollback. This decision adds a second
   shape; it does not migrate the first.

## Alternatives considered

- **Use the protocol's Addon packages for the supplementary files** — top-level for the executable,
  addons for the rest. It is the mechanism's own vocabulary, and it is the wrong shape here: nothing
  expresses that a set of packages belongs together, so there is no version they are consistent at,
  no ordering, and no atomicity — an Agent could sit with a new executable and old shared objects,
  which is precisely the state that will not start. ADR-0017 also targets *one* top-level package
  per Agent by Selector; addons would need a second targeting model to answer "which addons, at
  which version, for this Agent".
- **A `strip_components` key**, as `tar` has, to drop the release tarball's leading directory.
  Stable across versions — it describes how a publisher builds archives, not which version — and one
  more number an operator has to get right by inspecting an artifact first. Suffix matching needs
  nothing written down and fails loudly when it is ambiguous, which is the better trade for a value
  nobody can verify until a rollout runs.
- **Strip a leading directory automatically** when every member shares one. It reads as the obvious
  convenience and is wrong on an archive whose top level is *meaningful* — one holding only `lib/`
  would be flattened into the root, and the failure would be a program that starts and cannot find
  its libraries. Behaviour that changes with the archive's contents is exactly what should not sit
  under a rollout.
- **Find the program by searching the unpacked tree**, with no `program_path` at all — the natural
  extension of today's match-by-file-name. It removes a key and it removes the answer to "what will
  this host run" from the configuration file: before the first package there is nothing to search,
  so the spawn path would exist only after an install, discovered rather than written. ADR-0021 put
  that value in the file on purpose.
- **A self-extracting single file** — a script carrying an embedded payload, installed as the
  program by today's mechanism, unpacking itself on first run. It needs no change here at all, which
  is its whole appeal. It also re-extracts on every update, hides what is installed from everything
  that inspects the host, makes the reported version a property of the wrapper rather than the
  agent, and turns a rollback into "run the old wrapper again and hope". A supported mechanism
  should not be an unsupported one wearing a costume.
- **Unpack the tree over the existing `program/` directory in place**, with no version directory.
  Smaller diff, and it destroys the only copy of the working agent at the moment it is most needed:
  a failed unpack halfway through leaves a tree that is neither version. The rollback that
  ADR-0015's health gate depends on would stop being available exactly when it fires.
- **Let the operator name a subdirectory in `binary`/`command`** (`bin/fluent-bit`) instead of
  adding a key. Rejected by ADR-0021 on its own terms: anything that is neither a bare file name nor
  an absolute path is refused precisely so consent is unambiguous, and quietly admitting a third
  form would make a fleet-visible capability depend on parsing rather than on shape.
- **Leave it, and let configuration management install multi-file agents.** The status quo, and
  defensible — but it splits the fleet into agents this project can manage and agents it can only
  watch, and the split follows how an agent happens to be linked rather than anything an operator
  chose.

## Sources / Prior art

- **OpAMP Baseline** — packages are opaque to the protocol: "The content of the file, functionality
  provided by the packages, how they are stored and used by the Agent side is Agent type-specific
  and is outside the concerns of the OpAMP protocol." `PackageType_TopLevel`/`PackageType_Addon` in
  the pinned proto (`crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`).
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>
- **Elastic Agent** distributes a `.tar.gz` holding a directory tree and states that the archive
  distributions — not the system packages — are the ones Fleet can upgrade: the same conclusion
  this ADR reaches, that remote lifecycle management wants a self-contained tree.
  <https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent>
- **Datadog Fleet Automation** keeps two installs side by side under `/opt/datadog-packages` "in
  case a rollback is needed" — the versioned-directory-plus-pointer shape, in a fleet product, for
  the same reason.
  <https://docs.datadoghq.com/agent/fleet_automation/upgrade_agents/>
- **Fluent Bit static linking** — `FLB_STATIC_BINARY` is an unmerged draft (open since 2020) with
  GPL concerns raised by a maintainer, and incompatible with `FLB_LUAJIT` and `FLB_SQLDB`. The
  reason "just ship it statically" is not an answer for this agent.
  <https://github.com/fluent/fluent-bit/pull/2558>
- **This project's own ADR-0010 install layout** (`versions/`, `current`, side-by-side, pointer
  move) and ADR-0020's use of it to replace a running program — the mechanism is already here, and
  already trusted for the harder case of the Client replacing itself.

## Consequences

- Positive: the class of agent that is an executable plus its libraries stops being second-class. An
  operator installs Fluent Bit the way they install anything else in the fleet — upload the
  artifact, aim its Selector — and the rollback, health gate, and version reporting that already
  exist come with it.
- Positive: the artifact stays the one upstream published, which is what ADR-0018 exists to protect.
  A `.deb` still is not openable, but the `.tar.gz` many projects publish alongside it now is —
  wrapper directory and all, since `program_path` is written against the part of the path that does
  not change between releases.
- Negative / trade-offs: **the archive gains a say in where bytes land.** Today's "no traversal to
  defend against" property is genuinely lost, and traded for a sanitizer that has to be right —
  absolute paths, `..`, symlinks, and hard links all become refusals that must be tested rather than
  assumed. This is the real cost of the decision and the part most worth reviewing.
- Negative / trade-offs: disk. Two unpacked trees per Supervisor, not two files — an agent with a
  few hundred megabytes of plugins doubles, on hosts where `supervisor_dir` was already the reason
  ADR-0021 made the location movable.
- Negative / trade-offs: a second layout to explain. `program/<file>` and
  `program/tree/<program_path>` coexist, and which one a Supervisor has depends on whether one
  optional key is set.
- Negative / trade-offs: mode and ownership semantics now come partly from the archive, so an
  artifact built with sloppy modes produces an agent that does not start — a failure whose cause is
  in the archive rather than on the host.
- Follow-ups: whether `opamp-package-sign pack` should grow a directory mode, so the tool that
  builds artifacts can build this one too — without it an operator packs the tree by hand, which is
  the same gap this project just closed for single files. Whether the reported `service.version`
  should be re-probed after an install, which this makes more visible: a tree's version is even less
  likely to match what was probed at startup. And whether a Windows host, where the modes above mean
  nothing and links are a different mechanism, needs its own pass before this is enabled there.
