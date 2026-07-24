# Template setup

This repository is a scaffold. Work through this file to turn it into your own project. Then
delete it together with the note that points here from [`README.md`](README.md) and its row in the
README's Project Layout block.

## What only you can do

1. Give the project its identity. An agent asks the human for four values and applies them:
   - **Project name.** Replace **NUC** in the title of [`README.md`](README.md) and in `"name"`
     of [`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json), and rewrite the
     README's intro sentence, which explains the template's name. Keep `NUC maintainer`
     in the `Deciders` lines of the ADRs: it names who decided, and it is how check 12 of
     [`scripts/check-docs.sh`](scripts/check-docs.sh) tells an inherited ADR from your own.
   - **Repository.** The badge in [`README.md`](README.md) points at the template's repository,
     `hivevm/nuc`. Repoint it to this project's `owner/name` (`git remote get-url origin` says it
     where a remote exists), or delete the badge line.
   - **Maintainer.** The copyright holder in [`LICENSE`](LICENSE), the code owner in
     [`.github/CODEOWNERS`](.github/CODEOWNERS) (a GitHub handle or team with write access; then
     uncomment its two rules), and the security contact in [`SECURITY.md`](SECURITY.md).
   - **License.** MIT ships in [`LICENSE`](LICENSE) and in the **License** section of the README.
     For another license, replace the text of both.
   Check 13 of [`scripts/check-docs.sh`](scripts/check-docs.sh) fails on any of these placeholders
   still present once this file is gone.
2. Review the ADRs you inherit in [`docs/adr/`](docs/adr/). Each was decided by the NUC
   maintainer, not by you, and binds this project only once you accept it as your own decision:
   add yourself to its `Deciders` and flip its `Status` to `🟢 accepted` in the index and the
   file, or supersede it with an ADR of your own ([`AGENTS.md` §3](AGENTS.md#3-adr-rules)).
   Nothing in between: an inherited ADR nobody has accepted is a rule nobody decided, and check
   12 of [`scripts/check-docs.sh`](scripts/check-docs.sh) fails on one still `proposed` once this
   file is gone. The flip makes the traceability check demand a test for each. The template
   ships them, so the checks stay green; the traceability self-test proves it while this file
   exists. The one exception is [ADR-0006](docs/adr/0006-architecture-style.md), the shape of
   the code. Its test is yours to write, so it is accepted in step 7 together with that test, or
   superseded now if the shape is wrong for this project.
3. Write your specification in [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md). Each section of
   the scaffold says in a line what belongs in it. Write the **Problem** first; everything else
   rests on it. Record structural decisions as ADRs in [`docs/adr/`](docs/adr/).
4. Record the **harness** as the first ADR of your own: which agents and models work here, the
   permission mode they run under, and what may run unattended. It answers at least: which agent
   and model implements and which reviews, in a fresh context and with a stronger model where one
   exists; which parts get more than that review, from a second reviewer of a different kind and
   from the human who owns the part; how parallel sessions are kept apart, by a worktree or a
   container each; how an unattended session claims a ticket from the frontier
   ([ADR-0004](docs/adr/0004-feature-layer.md)); how much agent work may be open at once, capped
   at what the humans can review, because with agents the bottleneck moves from writing to
   reviewing, and a harness that outruns it produces a backlog, not software; and which parts of
   the system are never worked unattended however small the ticket: authentication and
   authorization, payment, public interfaces, data migrations, and whatever else the
   specification's *Threats & Forbidden Actions* protects. It is a framework choice under
   [`AGENTS.md` §3](AGENTS.md#3-adr-rules), rule 1, and the one every later session runs inside.
5. Fill in the **Overview**, **Build, Test & Run**, and **Usage** sections of
   [`README.md`](README.md).
6. Add your language toolchain (the base image ships none) and record it as an ADR
   ([`AGENTS.md` §3](AGENTS.md#3-adr-rules), rule 1). The build, test, and lint commands of the
   README run through **one script under [`scripts/`](scripts/)**, say `check-build.sh`.
   [`scripts/check-all.sh`](scripts/check-all.sh) invokes it with a `run` line, and a job of
   [`checks.yml`](.github/workflows/checks.yml) runs it after setting up the toolchain, the job
   named among the required checks in the README. That is what makes the pre-push hook and CI
   refuse a red change ([`AGENTS.md` §5](AGENTS.md#5-quality-bar--definition-of-done)). Check 9
   of [`scripts/check-docs.sh`](scripts/check-docs.sh) fails on a job whose commands are written
   inline instead, and while the three lists disagree. What is too slow to gate a push, such as
   a nightly suite or a long benchmark, is a workflow of its own on a schedule, not a job here.
   Beyond the commands, that ADR names the **sensors**, the mechanical checks that catch what
   agents get wrong most: a module or function grown too large or too complex, a dependency in
   the wrong direction, logic written a second time, code nothing calls, a test removed or
   skipped, and a test suite that passes without asserting much, which mutation testing measures
   where the toolchain offers it. Their output is what the reviewer of
   [`AGENTS.md` §5](AGENTS.md#5-quality-bar--definition-of-done) is handed; a sensor that runs
   only in someone's head is a rule in prose. One sensor ships with the template, because no
   toolchain has it: [`scripts/sensor-tests-kept.sh`](scripts/sensor-tests-kept.sh) reports the
   test files deleted and the skip markers added since the base. For each other row of
   [Sensors by toolchain](#sensors-by-toolchain) below, the ADR names a tool or says none, and
   why.
7. Lay down one golden path and one test pattern: a small, complete, idiomatic piece of the real
   system and the test that verifies it. Name both in
   [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). The specification says *what*; this is where
   an agent learns *how* this project does it, and what it copies from is what you get more of.
   Add one **structural rule as a test**: the dependency direction of
   [ADR-0006](docs/adr/0006-architecture-style.md), failing when the core depends on an adapter
   or a technology, cited with `Verifies: ADR-0006` and flipped to accepted in the same change
   ([ADR-0003](docs/adr/0003-decisions-verified-by-tests.md)). Once you have superseded that ADR,
   write whatever the superseding one names as its enforcement instead. A decision about the
   shape is the one entropy erodes first. What the golden path shows that no check decides and a
   reader could not guess, the why behind a shape, is the first entry in
   [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md).
8. Work through [**Repository settings**](README.md#repository-settings), the rules that only the
   GitHub repository itself can enforce.

Project-specific conventions belong in [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md), in checks,
and in ADRs; on what may and may not be edited, see [`AGENTS.md` §3](AGENTS.md#3-adr-rules).
Leave the **Dev Container**, **Coding Agents**, **Repository settings**, **Template**, and
**Project Layout** sections of the README as-is; they describe the scaffold. The Template
section names the release this project was created from and is the one link to the template
that stays. The Project Layout block only inventories the files, so a document you remove goes
out of it too, this file included.

## A small project

A tool, a library, or a one-person project pays for the steps above only where they buy
something. The chain stays intact as long as three inherited ADRs stand: ADR-0001 (the
documents the checks read), ADR-0003 (a `Verifies:` marker per accepted decision, one line of
cost), and ADR-0005 (the skills and their pointers, which cost nothing until invoked). Two more
cost nothing to accept: [ADR-0007](docs/adr/0007-action-references.md) is held by a check over
workflows every project has, and [ADR-0008](docs/adr/0008-template-releases.md) is one line of
the README. What can go, and how:

- **No Dev Container?** Supersede [ADR-0002](docs/adr/0002-dev-container-runtime.md) with an ADR
  that says where the work runs instead, delete [`.devcontainer/`](.devcontainer/), and remove
  the `devcontainer` job, its `run` lines in [`scripts/check-all.sh`](scripts/check-all.sh), and
  its name in the README's required checks in the same change; check 9 holds the three lists
  together.
- **No core to protect?** A script, a thin CLI, or a library without infrastructure supersedes
  [ADR-0006](docs/adr/0006-architecture-style.md) with the shape it does have and the test that
  decides it, or with `**Not mechanically decidable:**` and the reason.
- **Work never exceeds a session?** [ADR-0004](docs/adr/0004-feature-layer.md) stays accepted and
  costs nothing: it applies only above that threshold, and the threshold is a judgement under
  proportionality ([`AGENTS.md` §1](AGENTS.md#1-principles)).
- **The harness ADR** of step 4 can be five lines: one agent and model, the human reviews every
  change, nothing runs unattended, no parallel sessions. It grows when the harness does.
- **The toolchain ADR** of step 6 names the sensors it has, and one that ships with the linter
  is enough to start; the design revision adds what the mistakes call for.
- **A single maintainer** cannot approve their own pull request, so the Code Owners review in
  the repository settings cannot be met; keep the required checks and the force-push block, and
  let [`.github/CODEOWNERS`](.github/CODEOWNERS) name the owner without the requirement. The
  status flip and the specification are then guarded by the rule alone; say so where
  [`SECURITY.md`](SECURITY.md) describes the gate.

[`AGENTS.md`](AGENTS.md), the glossary, the conventions, and the overview stay, empty where
there is nothing to say; a scaffold section costs a reader a glance, a missing document costs a
check.

## Sensors by toolchain

One or two tools per sensor of step 6, each verified against its own releases or documentation
in September 2026: existence, purpose, and a release or commit in 2025 or 2026. A list like this
ages; check a tool before the toolchain ADR names it. No verified tool in any toolchain notices a
**removed** test; the template's tests-kept sensor does, for every toolchain, and knows the
skip markers of this table.

| Toolchain | Dependency direction | Size and complexity | Duplication | Dead code | Skipped tests | Mutation testing |
|---|---|---|---|---|---|---|
| Java / Kotlin | [ArchUnit](https://github.com/TNG/ArchUnit); [Konsist](https://github.com/LemonAppDev/konsist) for Kotlin | [PMD](https://docs.pmd-code.org/latest/pmd_rules_java_design.html) `CyclomaticComplexity`, `NcssCount`; [detekt](https://detekt.dev/docs/rules/complexity/) `complexity` rule set | [PMD CPD](https://pmd.github.io/pmd/pmd_userdocs_cpd.html) | [detekt](https://detekt.dev/docs/rules/style/) `UnusedPrivateFunction` | diff rule (`@Disabled`, `@Ignore`) | [PIT](https://github.com/hcoles/pitest) |
| TypeScript / JavaScript | [dependency-cruiser](https://github.com/sverweij/dependency-cruiser); [eslint-plugin-boundaries](https://github.com/javierbrea/eslint-plugin-boundaries) | ESLint [`complexity`](https://eslint.org/docs/latest/rules/complexity), [`max-lines-per-function`](https://eslint.org/docs/latest/rules/max-lines-per-function) | [jscpd](https://github.com/kucherenko/jscpd) | [knip](https://github.com/webpro-nl/knip) | [eslint-plugin-jest](https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-disabled-tests.md) `no-disabled-tests` | [StrykerJS](https://github.com/stryker-mutator/stryker-js) |
| Python | [import-linter](https://github.com/seddonym/import-linter); [tach](https://github.com/gauge-sh/tach) | [ruff `C901`](https://docs.astral.sh/ruff/rules/complex-structure/) | [pylint `R0801`](https://pylint.readthedocs.io/en/stable/user_guide/messages/refactor/duplicate-code.html); jscpd | [vulture](https://github.com/jendrikseipp/vulture) | diff rule (`pytest.mark.skip`, `pytest.skip(`) | [mutmut](https://github.com/boxed/mutmut) |
| Go | [go-arch-lint](https://github.com/fe3dback/go-arch-lint); [depguard](https://github.com/OpenPeeDeeP/depguard) via golangci-lint | [golangci-lint](https://golangci-lint.run/docs/linters/) `gocyclo`, `gocognit`, `funlen` | [dupl](https://github.com/mibk/dupl) via golangci-lint; PMD CPD | [staticcheck `U1000`](https://github.com/dominikh/go-tools); [`deadcode`](https://pkg.go.dev/golang.org/x/tools/cmd/deadcode) | diff rule (`t.Skip(`) | [gremlins](https://github.com/go-gremlins/gremlins) |
| Rust | [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) `bans` for forbidden crates; no verified linter for module direction inside a crate | clippy [`too_many_lines`](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/functions/mod.rs), [`cognitive_complexity`](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/cognitive_complexity.rs) | jscpd | rustc [`dead_code`](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html) | diff rule (`#[ignore]`) | [cargo-mutants](https://github.com/sourcefrog/cargo-mutants) |
| C# / .NET | [ArchUnitNET](https://github.com/TNG/ArchUnitNET) | [`CA1502`](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca1502) | jscpd; PMD CPD | [`IDE0051`](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/style-rules/ide0051) | [xUnit1004](https://xunit.net/xunit.analyzers/rules/xUnit1004) | [Stryker.NET](https://github.com/stryker-mutator/stryker-net) |
