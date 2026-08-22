# ADR-0093: The GLPI Agent gets a kind of its own

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Applies [ADR-0091](0091-a-kind-knows-its-own-agent.md) to the agent that shows most plainly why that
rule exists. The rule itself, the `Plugin` seam and the artifact-document requirement are stated
there.

## Context

**The GLPI Agent has no kind, so the manual carries two blocks.** `docs/manual/glpi-agent.md`
documents it as a recipe for the generic `command` Supervisor — seven keys on Linux, eight on
Windows — and the two differ in nearly everything:

| | Linux | Windows |
|---|---|---|
| program | `AppRun` | `glpi-agent.exe` |
| `program_path` | `AppRun` | `perl/bin/glpi-agent.exe` |
| `working_dir` | — | `${supervisor_dir}/program/tree` |
| `args` | `--script=glpi-agent`, then the daemon flags | four Perl `-I` paths, the script by path, then the daemon flags |

Not one of those differences is a decision. They follow from `std::env::consts::EXE_SUFFIX` and from
where the AppImage puts its interpreter — facts of the artifact this project packs itself
([ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md)), transcribed into every
host's file by hand.

**The rest of the block is equally fixed.** `--conf-file` points at `glpi-agent-conf`, the name
`opamp-package-fetch` uploads; `--vardir` points beside the tree because a package swap replaces the
tree whole; the file logging exists so a daemon with no console has somewhere to write; `--daemon
--no-fork` is not optional at all — forking would hand the Supervisor a pid it does not own, which
is a supervision bug, not a preference. And `service_name = "glpi-agent"` is the Agent type every
GLPI Configuration is aimed at (ADR-0033): the same string on every host, written on every host.

**The one thing an operator does decide** — which hosts run a GLPI Agent — is `type` and `name`.

## Decision

We will add a **`glpi` kind**, and its block will be two lines on both platforms.

1. **The kind knows the invocation, per platform**: the program name and `program_path` from the
   table above, the full argument list including the Perl `-I` paths, `--conf-file` against
   `glpi-agent-conf`, `--vardir` beside the tree, the file logging, `--daemon --no-fork`, how it is
   asked for its version, and `service_name = "glpi-agent"`.

   **The working directory is where the two platforms part.** ADR-0091's general rule — the
   directory the program lives in — is already right on Linux, where the program *is* the tree
   root's `AppRun`, so the kind names nothing. On Windows it is not: the program sits at
   `perl/bin/`, and the bundled Perl expects the tree root, which is why upstream's own portable
   `.bat` sets it before invoking the agent. So the kind names it there — the one place a wrapper
   overrides the general derivation, and the case that shows why the derivation is a default rather
   than a law.

2. **It has no settings of its own.** Its strict parse (`Plugin::check`) accepts an empty table and
   refuses every key, so a block carrying one fails at startup naming what supplies the value now.
   A wrapper that needed an escape hatch would be a wrapper that does not know its agent.

   ```toml
   [[supervisor]]
   type = "glpi"
   name = "glpi"
   ```

3. **`--daemon --no-fork` is part of the kind, not a default an operator may drop**, and the two
   flags answer two different failures. Without `--daemon` the agent runs its tasks once and exits,
   and the watchdog restarts it for ever. Without `--no-fork` it detaches, leaving the Supervisor
   holding a pid that ends immediately while the real process runs on unsupervised. Both were
   warnings in prose; they become properties of the kind, which is where a supervision requirement
   belongs.

4. **Its artifact document is `docs/artifacts/glpi-agent.md`**, in ADR-0091 clause 9's shape, with
   the two tests that clause requires — one against the AppImage repack plan in
   `opamp-package-fetch`, one against the constants above, `cfg`-gated per platform. GLPI is the
   agent where those tests earn the most: its Linux artifact is the one this project **repacks**
   rather than ships as published, so its internal layout is ours to keep in step.

## Alternatives considered

- **Leave it as a `command` recipe and fix the documentation.** The status quo. Rejected: the
  documentation is not wrong, it is *duplicated per host* — and it is duplicated per platform on top
  of that. A recipe cannot be updated when an upstream release moves a path; a kind can.

- **One block with placeholders instead of two.** `${exe_suffix}` and friends would collapse the two
  blocks into one. Rejected: it makes the operator's file a template of the artifact's layout, which
  is the transcription this ADR removes rather than shortens — and the Windows argument list is not
  the Linux one with a suffix appended, it has four extra paths.

- **Wait for GLPI to publish a self-contained Linux archive**, after which the repack and half the
  Windows/Linux divergence would go away. Rejected as a reason to wait: the divergence in the block
  is real today, and if that release comes, this kind is the one place to change.

## Sources / Prior art

- **[ADR-0064](0064-self-contained-glpi-agent-packages-for-both-platforms.md)** — the Windows zip as
  published and the Linux AppImage repacked deterministically: the artifact whose shape this kind
  compiles in.
- **`docs/manual/glpi-agent.md`** — the two blocks quoted in the Context; the reasons for `--daemon`
  (*"the agent runs its tasks once and exits, and the watchdog would restart it forever"*) and
  `--no-fork` (*"it stays the Supervisor's direct child, one process, on every platform"*), which
  become clause 3; and the warning never to spawn `glpi-agent.bat`, whose wrapper would make the
  supervised child `cmd.exe`.
- **`crates/package-tools/src/bin/opamp-package-fetch.rs`** — `glpi_plans` and the `AgentKind` entry
  naming `glpi-agent` and the Configuration `glpi-agent-conf`, which is where this kind's `--conf-file`
  points.

## Consequences

- **Positive: one block, both platforms.** The GLPI page stops documenting two, and a host's file
  stops saying anything about Perl.
- **Positive: the repack and the kind can be kept in step.** They are the two ends of the same
  artifact, and after this they are tested against one document instead of against a manual page.
- **Negative: a GLPI Agent that somebody packed differently no longer fits.** ADR-0091 clause 8 in
  the concrete: the answer is the packing tool, not a key.
- **Negative: an operator who needs an extra GLPI flag now needs a code change** — or the agent's
  own configuration, which is where most of what one would want to pass belongs anyway. `command`
  stays available for an installation that genuinely needs to invoke it differently, at the price of
  writing the whole invocation again.
