# ADR-0091: A kind knows its own agent — a block names a decision, never a layout

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Narrows what a `[[supervisor]]` block is for. It builds on [ADR-0011](0011-supervisor-mode-hexagonal-core-and-plugins.md)
(a Plugin is the adapter for a kind of process) and on [ADR-0085](0085-the-client-manages-only-programs-it-installs.md)
(a Managed Process is always the fleet's, in a directory this Client owns), and supersedes neither:
it says what follows for the *configuration* once both hold.

## Context

**A block describes two different things, and only one of them is a decision.** Which agent this
host runs, and how that agent is built on this platform. The first is the operator's. The second is
a constant of an artifact **this project builds itself** — and it is written out by hand on every
host anyway.

The evidence is in this repository's own documentation:

- **Icinga 2** ([ADR-0092](0092-icinga-2s-block-keeps-only-what-enrolment-needs.md)) takes twelve
  keys, of which nine are the shape of the tree
  `opamp-package-fetch --agent icinga2` produces ([ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)):
  `binary`, `program_path`, `include_dir`, `plugin_dir`, and the five state directories that
  `Icinga2Plugin::layout` already defaults to `${supervisor_dir}/…` — the key exists only so it can
  be written again.
- **The GLPI Agent** ([ADR-0093](0093-the-glpi-agent-gets-a-kind-of-its-own.md)) has no kind at all.
  It is a recipe for the generic `command` Supervisor, and
  `docs/manual/glpi-agent.md` therefore carries **two blocks** — seven keys on Linux, eight on
  Windows — differing in the program name, `program_path`, `working_dir` and the whole `args` list,
  including four Perl `-I` paths. Not one of those differences is a choice: they follow from
  `std::env::consts::EXE_SUFFIX` and from where the AppImage puts its interpreter.

**The knowledge already exists here — on the wrong side of the wire.** `opamp-package-fetch` carries
an `AgentKind` table naming, per agent, its `service_name`, the repository its release comes from,
and the **default Configurations** it uploads with fixed names — `icinga2-conf`, `icinga2-zones`,
`glpi-agent-conf`, `telegraf-conf`. So this project already knows what a GLPI Agent *is*; the
operator's TOML is where that knowledge is transcribed by hand, once per host, and where it goes
stale when an artifact's layout moves.

**The reference implementation sits at the other pole, for a reason that does not apply here.** The
`opampsupervisor` configures its agent generically — `agent: executable: /usr/local/bin/otelcol-contrib`
plus `args` — because it supervises a binary *somebody else installed*: it cannot know the layout,
so it must be told. This Client is in the opposite position since ADR-0085: it installs the program
itself, into a directory it created, from an artifact it packed. It is the one end that *can* know.

**The pattern is the ecosystem's default where the tool owns the installation.** Elastic Agent ships
**integrations** whose defaults work without configuration and resolves every path through one
`paths` package rather than through the policy; Datadog ships **integrations** as
`conf.d/<name>.d/conf.yaml`, where the per-integration file carries the defaults and the operator
overrides only what differs. In configuration management the same shape is older still — a Puppet
*provider* is the platform-specific implementation a resource type selects, so a manifest says
`package { 'nginx': }` and not where the package manager lives. What all three have in common is
this ADR's rule: the thing that knows the platform is code, not the operator's file.

## Decision

We will make a **Plugin the authority on its own agent**: whatever follows from the kind and the
platform is supplied by the kind, and a key exists in a `[[supervisor]]` block only where a value is
a decision.

1. **Derivable means derived — and decided elsewhere means not written here.** A value this Client
   can compute from the kind, the platform, and its own layout is not a configuration key; neither is
   a value the fleet is the better place to set. Where such a key exists today it is **removed**, and
   a block still carrying it fails at startup with a message naming what replaces it — the pattern
   `package` and `accepts_packages` already run.

   **This is only survivable because a failed start is rolled back, and today it is not.** A run
   resolves its configuration *before* it resolves an update in flight
   (`runtime::run_until_shutdown`), so a version that cannot read this host's file exits before
   `selfupdate::on_start` counts the attempt — the probation of [ADR-0020](0020-client-self-update.md)
   never fires, the service manager restarts the broken version for ever, and the Server hears
   nothing because the Client never connects. For `icinga2` there is no block both versions accept
   (the old one requires `main_config`, the new one refuses it), so the cutover per host is
   unavoidable and the rollback *is* the safety net. **The order is therefore fixed as part of this
   work**: the update is resolved first, from the state directory an installed service already
   passes on the command line, so a configuration this version cannot read counts as the failed
   attempt it is and the host comes back on the version that worked. That is a gap in ADR-0020's own
   promise — its net catches everything except "the new version cannot read this host's
   configuration" — and closing it is a precondition here, not a side quest.

   The second half is what retires **`[supervisor.attributes]`**. It tagged one Agent among several
   on a host, which a Server **label** does from the fleet ([ADR-0042](0042-server-set-labels.md)):
   keyed by `instance_uid`, matched by the same Selectors, changed with one API call. The block's
   table only ever won in one situation — a host provisioned with a finished file, tagged before the
   Server had ever seen it — and that situation does not arise for a fleet whose Supervisor sets come
   from the Server (ADR-0056), where the block itself is written by the same hand that could set the
   label. The Client-wide `[attributes]` stay: they describe the *host*, they are what a fresh Agent
   carries into its first message, and nothing else can say them before there is an Agent to label.

2. **The `Plugin` trait carries the kind's defaults.** Beside `program_key()`, which already
   establishes that a kind knows something about its block, a plugin states its program's file name,
   its `program_path` inside a tree, its `service_name` — resolved per platform — and whether a block
   of this kind may pin the Supervisor Endpoint's port at all.

   **The working directory is derived for every kind, and `working_dir` goes with it.** A Managed
   Process starts in the directory its program lives in — `program/` for a single-file package,
   `program/tree/` for a tree (ADR-0023) — which is what a program that looks beside itself expects
   and what the GLPI Agent's Windows launcher does by hand today. Until now an unset key meant the
   process inherited whatever directory the service manager left this Client in, typically `/`: a
   default nobody chose and nothing describes. The core applies
   them where the block is silent; ADR-0021's rule (a bare file name, in this Supervisor's own
   directory) is applied to the derived value unchanged. Everything else a kind needs — arguments,
   environment, directories — it builds itself and simply stops reading from the block.

   **A kind names the directories its agent writes into, and the spawn guarantees them.** An agent
   the fleet delivers arrives on a host nobody prepared, and several of the agents wrapped here
   create nothing themselves: Icinga 2 exits when `DataDir` is absent, the GLPI Agent exits when
   `--vardir` is. An installation that fails for want of a directory is therefore **this Client's
   failure, not the operator's** — the fleet installs the agent, so the fleet owes it the ground it
   stands on. `icinga2` already did this for itself (ADR-0068); it becomes a field of the process
   specification instead, so a future kind cannot forget it and the guarantee is one piece of code
   rather than one per plugin.

   Made **before every spawn**, not once at install: a directory removed under a running fleet then
   comes back on the next restart rather than taking the Supervisor down. Made owner-only, because
   what an agent writes about a host is not for every local user to read. The *program's* own
   directories are not part of this — the install creates those — and what a kind lists lives
   outside `program/` precisely because a package swap replaces that whole. `collector`, `command`
   and `telegraf` list nothing: the first two know no agent, and Telegraf writes to its outputs.

   **Asking an agent for its version is part of that, and is never a parameter of a wrapper.** How a
   program states its version is a property of the program: `collector` and `icinga2` already run
   `--version` themselves, the second with a parser for its own banner. Every wrapper does the same,
   so no block names it. `version_args` survives on **`command` alone**, where the kind knows nothing
   about the agent and an operator naming those arguments is the only way the Agent reports a
   `service.version` at all — and, since ADR-0068, the only preflight that kind has.

   **Applying a configuration in place is the same kind of fact, and `reload_signal` goes entirely.**
   Whether a program re-reads its configuration on a signal, and which one, is its own convention:
   `icinga2` and `telegraf` know theirs and use it, a Collector has none and applies by restarting.
   The key is removed from `command` **as well** — a block declaring a signal for a program the kind
   knows nothing about is this ADR's transcription problem one level down, and it is the one key
   whose wrong value is invisible: a signal the process ignores looks like an apply that worked until
   somebody checks what the process is actually running. An unwrapped agent therefore applies a
   configuration by restart, which is the generic behaviour [ADR-0060](0060-unified-supervisor-lifecycle-port.md)
   already defines, and an agent whose in-place reload is worth having is an agent worth wrapping.

3. **Each wrapped agent is decided in an ADR of its own.** The rule above is general; what a
   particular agent's kind knows is not, and an upstream that moves a path should be able to
   supersede one document rather than this one. Three follow, each carrying its removals, what it
   keeps and why, and its artifact document:

   | Kind | ADR |
   |---|---|
   | `icinga2` | [ADR-0092](0092-icinga-2s-block-keeps-only-what-enrolment-needs.md) — the block keeps only its enrolment |
   | `glpi` | [ADR-0093](0093-the-glpi-agent-gets-a-kind-of-its-own.md) — two platform blocks become one of two lines |
   | `telegraf` | [ADR-0094](0094-telegraf-gets-a-kind-of-its-own.md) — five keys, one of them unwritable for a mixed fleet |

   `collector` and `command` stay here, in clause 4: they are the two kinds that know *nothing*
   about their agent, so what they keep is a statement about the rule rather than about an agent.

4. **What stays, stays for a stated reason.** The goal is not zero keys; it is no key without a
   decision behind it.
   - **`collector` stays one kind for both distributions**, and keeps `binary`. `otelcol` and
     `otelcol-contrib` are two agents with one lifecycle, one configuration mechanism and one
     supervision story; what separates them is which distribution a host is meant to run, and that
     is a decision. `binary` is the single place it is stated — a key with a decision behind it,
     which is exactly what this ADR keeps. Splitting the kind would move that decision into `type`
     and buy nothing but a second plugin. (`service_name` is already derived there: it falls back
     to the program's file name.)
   - `collector` keeps `args` and `env`: a feature gate and a per-host endpoint read as
     `${env:VAR}` are decisions, and nothing can derive them. `command` keeps both for the same
     reason, one level more general.
   - **`endpoint_port` narrows to `collector`.** The Supervisor Endpoint is bound for *every*
     Supervisor and stays that way (ADR-0003), but only a Managed Process that speaks OpAMP ever
     connects to one — in practice a Collector carrying the `opampextension`. Pinning it is therefore
     a decision there and nowhere else, and a block of another kind carrying the key is refused with
     that sentence. It cannot be derived: the port has to appear in the Collector configuration **the
     fleet delivers**, and this Client writes a delivered configuration byte for byte rather than
     expanding anything into it — a Managed Process's configuration is not ours to rewrite. So the
     value is agreed between two authors, and only one of them is on this host. The default stays
     **ephemeral**: a fixed one would read better in a manual and would make two Collector
     Supervisors on one host collide where today they do not.
   - **The escape hatch narrows with everything else.** `args` and `env` stay on `collector` and
     `command`, the two kinds that know nothing about their agent, and go from the wrappers, which
     build both whole. A wrapper that needed an escape hatch would be a wrapper that does not know
     its agent — and where an operator genuinely needs to change how a wrapped agent runs, that
     agent's own configuration is the place, which the fleet already delivers.
   - **A wrapped kind may still keep a key, and each says why in its own ADR.** `icinga2` keeps the
     four values of its enrolment (ADR-0092), because they describe the installation a host is
     joining and nothing here can compute them. That is this clause applied, not an exception to it:
     the test is whether a decision exists and whether anyone else is better placed to make it.
   - `command` keeps `args`, `env` and `version_args`, and loses `reload_signal` and `working_dir`
     with everyone else. It is the kind for an agent nobody has written a wrapper for, and those
     three are the price of that generality: without `version_args` the Agent reports no version and
     gets no preflight, and without arguments most Foreign Agents never find their configuration.
     Each fails visibly when it is wrong, which is what separated them from the reload signal — and
     the working directory now has an answer better than the one it had.

5. **Timing is a fleet policy, then a kind's correction — never a host's.** `stop_timeout_secs`
   (how long a graceful stop may take), `apply_grace_secs` (how long a restarted process must survive
   before an apply is acknowledged) and `retain_previous_secs` (how long the superseded version is
   kept) are per-block today, the first two with defaults compiled in and the third already global in
   `[updates]`. All three become **a global default that a wrapped kind may override, and nothing
   below that**: `[supervisors] stop_timeout_secs` and `apply_grace_secs` as the new global,
   `retain_previous_secs` staying in `[updates]`, where it belongs to the update it bounds.

   A kind knows its own timing better than a file does. The manual tells an operator to write
   `stop_timeout_secs = 60` and `apply_grace_secs = 30` into every Icinga block precisely because a
   busy daemon takes a minute to come down — a property of Icinga, restated per host. The three keys
   survive **only on the unwrapped kinds**, `collector` and `command`, where no kind holds the value
   and the operator is the only one who can state it. That asymmetry is the rule of this ADR read
   twice: a key exists where there is a decision *and nobody better placed to make it*.

6. **A rolled-out set fails the same way.** `Plugin::check()` performs this validation without side
   effects (ADR-0056), so a Supervisor set the Server delivers with a removed key is refused
   **before** a running process is touched, and the refusal reaches the Server as `FAILED`.

7. **A Client says which kinds it has.** Wrapping creates a fact the fleet did not have to know
   before: a `type` is now something a Client either carries or does not, and a Server that rolls a
   `glpi` set at a Client too old to have that plugin learns it from a `FAILED` afterwards rather
   than by not aiming there. So the Client's own Agent reports its compiled-in kinds as
   non-identifying attributes, **one key per kind** — `supervisor.kind.glpi = "true"` — and a
   Selector can then aim a Supervisor set at the Clients that can run it.

   One key per kind rather than one list, because of how matching works here and not because a list
   would read worse: a Selector is **equality over string values** (`configs.rs::matches`), so a
   list attribute could only be matched by spelling the whole list, and an Agent's set of kinds is
   exactly the thing one wants to ask about *one member* of. This is the shape that needs no change
   to the matching contract; the follow-up below is where it collapses back into one key.

   The Baseline's own home for this is `AvailableComponents` — *"metadata relating to the components
   included within the agent"*, with `ReportsAvailableComponents` — and this Client already carries
   that message through from a Collector that reports its own components. It is **not** used here
   yet for two reasons stated rather than glossed: the message is marked *Development* in the schema
   this project pins, and nothing matches against it — Selectors resolve over the description, so
   reporting kinds there would tell the fleet something it could not act on.

8. **A kind binds itself to one artifact's shape, and says so.** The derived paths are those of the
   artifact `opamp-package-fetch` builds. A tree somebody else packed differently no longer fits, and
   the answer to that is to repack it with the tool — not to reopen the key. This is the cost of the
   decision, taken deliberately.

9. **Every wrapped agent gets an artifact document, and two tests hold it to the code.** Clause 8
   makes the artifact's shape load-bearing on *both* ends: the tool packs it, the kind runs it, and
   neither end sees the other. So the shape is written down once, in `docs/artifacts/<agent>.md` —
   **for the wrapped kinds only**: `icinga2`, `glpi`, `telegraf`.

   A `collector` needs none, and that is not an omission. Nothing about `otelcol` or
   `otelcol-contrib` is compiled into a kind: the artifact is installed **as published**, the program
   is found by the file name the operator writes in `binary`, and the Supervisor passes the
   Configurations it received as `--config` without knowing anything about their shape. There is no
   second end to keep in step, so a document would describe only what the release page already says
   and would go stale unread. The rule follows from what the decision actually creates: a document
   exists where a kind holds constants that an upstream release can invalidate.

   Each document carries the same eight sections, so a diff between two agents stays readable: the
   **source** (repository, tag form, how a version is read from it); the **assets** per platform and
   their naming; **integrity** (which checksum form, and where it lives); the **treatment** (as
   published, or which repack); the **shape in the delivered tree** (single file or tree, where the
   program sits, what lies beside it); **what the Client derives** from it, per platform, with the
   reasoning; the **Configurations** the tool uploads and what the agent reads them as; and **what
   can change** upstream, with the test that goes red when it does.

   The last part is the point, and it is a condition rather than a recommendation: each document is
   pinned by **two tests, one per side** — one against the `Plan` this tool builds (asset name,
   checksum source, action, output name), one against the kind's constants (program name,
   `program_path`, derived directories, arguments), `cfg`-gated per platform. Both name their
   document. An upstream change then surfaces as a red test on the side it touches, and the document
   says what the other side owes. Without that pairing this decision would replace one transcription
   — the operator's — with another nobody notices going stale.

## Alternatives considered

- **Leave the values in the file and fix the documentation instead** — ship the blocks as
  copy-paste templates, per platform. Rejected: it is what exists, and it is why GLPI has two blocks
  and why a moved path in an artifact becomes an edit on every host. A template is a default that
  cannot be updated.

- **Keep the keys, only stop documenting them.** Nothing breaks, and an override is always at hand.
  Rejected on the reviewer's decision and on precedent: two sources of truth for a value the Client
  computes is how a host quietly differs from what the fleet believes. `package` and
  `accepts_packages` were removed loudly for the same reason.

- **Split `collector` into `otelcol` and `otelcol-contrib`.** It would make every wrapped block two
  lines without exception. Rejected: the two share a lifecycle, a configuration mechanism and a
  supervision story, and what separates them — which distribution this host runs — is a decision.
  Moving a decision from `binary` into `type` does not remove it; it only costs a second plugin and
  makes "the same collector, other distribution" a different kind of Agent than it is.

- **A machine-readable manifest per agent, consumed by both ends** — one file the tool reads and the
  Client compiles in, so the artifact's shape has literally one source. The strongest form of clause
  9, and rejected for now: it needs a schema, a parser on each side, and a decision about what
  happens when a Client meets a manifest newer than itself — an ADR of its own, for a coupling that
  two tests already achieve. Worth revisiting if the number of wrapped agents grows past a handful.

- **Probe the artifact at runtime instead of compiling the layout in** — let a kind look for its
  program inside the unpacked tree rather than knowing where it is. Tempting, because it would
  survive a tree somebody else packed. Rejected: a wrong guess is silent and lands on a host, while
  a compiled-in constant is testable against the artifact this project builds and fails loudly when
  that artifact moves. The tool and the kinds are versioned together; a probe would only hide when
  they disagree.

- **A per-kind defaults *file* rather than code** — ship the constants as data the Client reads.
  Rejected: it re-creates the transcription problem one level down, and nothing would test that the
  data matches the artifact.

## Sources / Prior art

- **`opampsupervisor` (`cmd/opampsupervisor`, `supervisor.yaml`)** — `agent.executable` as an
  absolute path plus `agent.args`: the generic shape, and the right one for a supervisor that does
  not install what it supervises. The contrast this decision rests on.
- **Elastic Agent / Fleet integrations** — integrations whose defaults work unconfigured, with path
  resolution centralised in the agent's own `paths` package rather than carried in the policy.
- **Datadog Agent integrations** — `conf.d/<integration>.d/conf.yaml`: per-integration defaults, the
  operator writing only what differs.
- **Puppet providers** (named as the older form of the same pattern): a resource type selects a
  platform-specific implementation, so the manifest states intent and not location.
- **This repository, `crates/package-tools/src/bin/opamp-package-fetch.rs`** — the `AgentKind` table:
  per-agent `service_name`, source repository, and default Configuration names (`icinga2-conf`,
  `glpi-agent-conf`, `telegraf-conf`). The knowledge this ADR moves to the other end of the wire.
- **[ADR-0085](0085-the-client-manages-only-programs-it-installs.md)** — the Client installs what it
  supervises, which is what makes the layout knowable at all; **[ADR-0070](0070-repacked-vendor-packages-as-relocatable-icinga-2-trees.md)**
  — the tree whose shape the `icinga2` kind compiles in; **[ADR-0022](0022-supervisor-path-placeholders-in-process-arguments.md)**
  — the placeholders the derived defaults are expressed in, so a relocated `supervisor_dir` still moves everything with it.
- **`docs/manual/glpi-agent.md` and `docs/manual/icinga2.md`** — the two-block and twelve-key
  evidence quoted in the Context.

## Consequences

- **Positive: a host's file says what it runs, not how.** `type` and `name` for a wrapped agent; the
  same two lines on Linux and Windows, where GLPI needed two different blocks.
- **Positive: an artifact that moves is one edit.** A path that changes inside a repacked tree is
  fixed in the kind and reaches every host with the next Client version, instead of being an edit
  in every `supervisor.toml` and every rolled-out Supervisor set.
- **Positive: a delivered agent no longer needs a host prepared by hand.** The directories it
  writes into are made before it runs, so an installation cannot end in a crash loop over a missing
  directory — and one removed under a running fleet comes back on the next restart.
- **Positive: the platform branches become testable.** Compiled-in, `cfg`-gated constants can be
  asserted against the artifact this project builds; a Windows path written in a manual cannot.
- **Positive: a rollout can aim at Clients that can actually run it.** A Supervisor set naming
  `type = "telegraf"` reaches only Clients reporting `supervisor.kind.telegraf`, so a fleet upgraded
  in waves does not hand blocks to the half that would refuse them. What was a `FAILED` after the
  attempt becomes a Selector that never aimed there.

- **Negative: an Agent can no longer be tagged per block before its first message.** Retiring
  `[supervisor.attributes]` costs exactly one case: a host provisioned with a finished file, running
  several Agents, one of which had to carry an operator tag into its very first report. The
  Client-wide `[attributes]` still tag the host, and a label covers everything from the first message
  onward — but between those two there is now a gap of one exchange, and a fleet that provisions
  files rather than rolling out sets will feel it.

- **Negative: one attribute per kind is a shape chosen by the matcher, not by taste.**
  `supervisor.kind.glpi = "true"` is five keys on a Client with five plugins — a list spelled out
  one key at a time, because Selector matching is equality over strings. It reads worse than
  `supervisor.kinds = [...]` and it is the only form that works without changing what a Selector
  means for everything else.

- **Positive: an upstream change has an owner.** Before, a moved path in a release was noticed by
  whoever's rollout broke. With clause 9 it is a red test on the side it touches and a document
  saying what the other side owes — the packing tool and the kind can no longer drift apart quietly.
- **Negative: supporting a new agent well now means code — and a document, and two tests.** A recipe
  in the documentation was free; a wrapper is a change to this crate, a page under `docs/artifacts/`,
  a test on each side, a review and a release. That is the deliberate price of clause 9, and the
  `command` kind stays exactly so that "not yet wrapped" remains a usable state rather than a
  blocker.
- **Negative: a reloadable Foreign Agent loses its reload until somebody wraps it.** A `command`
  block that declares `SIGHUP` today keeps the process — and its in-flight state — across a
  configuration change; afterwards that agent is restarted like any other. The mechanism is
  untouched (ADR-0060's reload-or-restart still runs for the kinds that know their signal), only the
  way to declare it from a generic block is gone. For an agent where that matters, the answer this
  ADR gives everywhere is the same: write the wrapper.

- **Negative: a Foreign Agent's relative paths now resolve somewhere else.** Deriving the working
  directory replaces a default nobody chose — whatever directory the service manager left this
  Client in — with the directory the program lives in. That is the better answer, and it is a
  *silent* change for a `command` block that relied on the old one: a process writing `foo.log`
  relative writes it beside its program instead of wherever it landed before. The block that used to
  say `working_dir` no longer can, so the migration for those is to make the path absolute or to
  point the agent's own configuration at it.

- **Negative: the documents can go stale in the parts no test pins.** Prose about *why* a path is
  where it is has no assertion behind it. The eight sections are ordered so the pinned facts come
  first; the reasoning that follows them is worth having and is worth distrusting after a big
  upstream release.
- **Negative: every existing block for a wrapped agent must be edited once.** The failure is loud
  and names the derived value, and a Server-delivered set is refused before anything stops — but it
  is an intervention per host for hosts that are not fleet-managed in that half. Where the Client
  updates itself into the refusal, the rollback of ADR-0020 brings the host back on the version that
  could read its file **once the order fix above is in** — without it the same situation is a silent
  crash loop, which is why that fix ships first.
- **Negative: a kind is now coupled to one artifact's shape.** Clause 8 states it; what it costs is
  that "bring your own tree" stops being a configuration question and becomes a packaging one.
- **Follow-ups:** whether the kinds a Client carries move from attributes to `AvailableComponents`,
  once that message leaves *Development* in a Baseline this project pins and the Server can match
  against it — which would collapse clause 7's one-key-per-kind back into one message. Which agent
  gets the next wrapper — Fluent Bit is the documented candidate — which is an application of this
  rule rather than a new decision, and would be an ADR beside ADR-0092 to ADR-0094. And, if the
  number of wrapped agents grows, whether the artifact documents become the machine-readable
  manifest the alternatives section leaves on the table.
