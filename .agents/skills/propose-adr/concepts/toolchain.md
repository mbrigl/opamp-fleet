# Toolchain

The language, its build, test, and lint commands, and the sensors that check what an agent
wrote. Every project decides it at setup; the template ships none. The toolchain ADR is the one
the review skill, the pull request template, and the shape entries of this catalogue name when
they speak of "the sensors the toolchain ADR names".

The sensors come in three kinds:

- **Behaviour:** the tests, and the `Verifies:` markers that tie them to the decisions and
  criteria they protect ([ADR-0003](../../../../docs/adr/0003-decisions-verified-by-tests.md)).
- **Maintainability:** what agents get wrong most: a module or function grown too large or too
  complex, logic written a second time, code nothing calls, a test removed or skipped, and a
  suite that passes without asserting much, which mutation testing measures.
- **Architecture fitness:** the structural test of the shape where the project chose one, and a
  dependency in the wrong direction.

## Signals

- A new project takes its founding decisions.
- The first code lands, or a second language joins the first.
- A design revision finds a mistake that a sensor of one of the three kinds would have caught.

## Fits when

- **The linter's own rules first**: the sensors that ship with the linter and the test runner,
  configured; one is enough to start, and the design revision adds what the mistakes call for.
  A tool, a library, or a one-person project starts here.
- **One tool per kind**: a row of the table below for each kind, the fast ones before every push,
  the slow ones on a schedule.
- **Mutation testing on a schedule**: a quality goal asks for tests that decide, not tests that
  run; mutation testing is too slow to gate a push and runs as a workflow of its own.

Whichever fits, the ADR names the build, test, and lint commands of the README; the one script
under `scripts/` that runs them, which `scripts/check-all.sh` invokes with a `run` line and a job
of `checks.yml` runs after setting up the toolchain; and, for each column of the table below, a
tool or "none" and why, and where it runs.

## Does not fit

- Commands written inline in the workflow: they run in CI and never in the pre-push hook; check 9
  of `scripts/check-docs.sh` fails on such a job.
- A sensor that runs only in someone's head: it is a rule in prose.
- A slow suite as a job of `checks.yml`: it gates every push; it is a workflow of its own on a
  schedule.

## The rule a test decides

The golden path's tests cite the toolchain ADR with a `Verifies:` marker, and the ADR flips to
accepted with them. Check 9 of `scripts/check-docs.sh` holds the build job, the local runner, and
the required checks of the README together.

## Sensors

The tools the ADR names. Two ship with the template because no toolchain has them:
`scripts/sensor-tests-kept.sh` reports the test files deleted and the skip markers added since
the base, and `scripts/sensor-action-refs.sh` the forms of the action references. The checks also
run once a week against `main`, which catches what changes without a commit: the runner image, a
retagged action, a toolchain download.

### Sensors by toolchain

One or two tools per kind, each verified against its own releases or documentation in September
2026: existence, purpose, and a release or commit in 2025 or 2026. A list like this ages; check a
tool before the toolchain ADR names it. No verified tool in any toolchain notices a **removed**
test; the template's tests-kept sensor does, for every toolchain, and knows the skip markers of
this table.

| Toolchain | Dependency direction | Size and complexity | Duplication | Dead code | Skipped tests | Mutation testing |
|---|---|---|---|---|---|---|
| Java / Kotlin | [ArchUnit](https://github.com/TNG/ArchUnit); [Konsist](https://github.com/LemonAppDev/konsist) for Kotlin | [PMD](https://docs.pmd-code.org/latest/pmd_rules_java_design.html) `CyclomaticComplexity`, `NcssCount`; [detekt](https://detekt.dev/docs/rules/complexity/) `complexity` rule set | [PMD CPD](https://pmd.github.io/pmd/pmd_userdocs_cpd.html) | [detekt](https://detekt.dev/docs/rules/style/) `UnusedPrivateFunction` | diff rule (`@Disabled`, `@Ignore`) | [PIT](https://github.com/hcoles/pitest) |
| TypeScript / JavaScript | [dependency-cruiser](https://github.com/sverweij/dependency-cruiser); [eslint-plugin-boundaries](https://github.com/javierbrea/eslint-plugin-boundaries) | ESLint [`complexity`](https://eslint.org/docs/latest/rules/complexity), [`max-lines-per-function`](https://eslint.org/docs/latest/rules/max-lines-per-function) | [jscpd](https://github.com/kucherenko/jscpd) | [knip](https://github.com/webpro-nl/knip) | [eslint-plugin-jest](https://github.com/jest-community/eslint-plugin-jest/blob/main/docs/rules/no-disabled-tests.md) `no-disabled-tests` | [StrykerJS](https://github.com/stryker-mutator/stryker-js) |
| Python | [import-linter](https://github.com/seddonym/import-linter); [tach](https://github.com/gauge-sh/tach) | [ruff `C901`](https://docs.astral.sh/ruff/rules/complex-structure/) | [pylint `R0801`](https://pylint.readthedocs.io/en/stable/user_guide/messages/refactor/duplicate-code.html); jscpd | [vulture](https://github.com/jendrikseipp/vulture) | diff rule (`pytest.mark.skip`, `pytest.skip(`) | [mutmut](https://github.com/boxed/mutmut) |
| Go | [go-arch-lint](https://github.com/fe3dback/go-arch-lint); [depguard](https://github.com/OpenPeeDeeP/depguard) via golangci-lint | [golangci-lint](https://golangci-lint.run/docs/linters/) `gocyclo`, `gocognit`, `funlen` | [dupl](https://github.com/mibk/dupl) via golangci-lint; PMD CPD | [staticcheck `U1000`](https://github.com/dominikh/go-tools); [`deadcode`](https://pkg.go.dev/golang.org/x/tools/cmd/deadcode) | diff rule (`t.Skip(`) | [gremlins](https://github.com/go-gremlins/gremlins) |
| Rust | [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) `bans` for forbidden crates; no verified linter for module direction inside a crate | clippy [`too_many_lines`](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/functions/mod.rs), [`cognitive_complexity`](https://github.com/rust-lang/rust-clippy/blob/master/clippy_lints/src/cognitive_complexity.rs) | jscpd | rustc [`dead_code`](https://doc.rust-lang.org/rustc/lints/listing/warn-by-default.html) | diff rule (`#[ignore]`) | [cargo-mutants](https://github.com/sourcefrog/cargo-mutants) |
| C# / .NET | [ArchUnitNET](https://github.com/TNG/ArchUnitNET) | [`CA1502`](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/quality-rules/ca1502) | jscpd; PMD CPD | [`IDE0051`](https://learn.microsoft.com/en-us/dotnet/fundamentals/code-analysis/style-rules/ide0051) | [xUnit1004](https://xunit.net/xunit.analyzers/rules/xUnit1004) | [Stryker.NET](https://github.com/stryker-mutator/stryker-net) |

## Alternatives

The three forms above grow into each other. A sensor written by hand under `scripts/` is a fourth,
for what no tool of the toolchain notices, as the two that ship with the template.

## Sources

- Birgitta Böckeler, *Sensors for coding agents* —
  <https://martinfowler.com/articles/sensors-for-coding-agents.html>: computational sensors next
  to the inferential review, and that coverage says a line ran, not that its effect was verified.
- The tools of the table, each at the link in its cell.
