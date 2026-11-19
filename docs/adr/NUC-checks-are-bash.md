# NUC-checks-are-bash: The checks and sensors the template ships are bash scripts under `scripts/`, each held by its self-test and run alike by the hooks and CI

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** the checks and sensors and their self-tests under `scripts/`, the scripts `scripts/check-all.sh` names as run on instruction, the root and the arguments of every script `scripts/*.sh` through `scripts/lib/root.sh`, the git hooks under `.githooks/`, the workflows under `.github/workflows/`, and the required checks of `CONTRIBUTING.md`

## Context

The template ships no language toolchain. A derived project picks its own, and some hosts run
macOS, Linux, or Windows under Git Bash. The checks that hold the decisions must run on every one
of them before the project has chosen anything.

Three more forces bound the answer. A git hook runs before any check can say which bash it found,
and macOS ships bash 3.2. A check that runs in CI and not in the local gate, or the other way
round, is a gate somebody believes in and does not have. Some settings live only in the GitHub
repository, and the workflow token cannot read all of them. Some sensors cost a full run of the
checks or more, and some scripts change the repository instead of reporting on it.

## Decision

We will ship every check and sensor of the template as a bash script under `scripts/`. The git
hooks and CI run them alike, but for the scripts of clause 7.

1. **Scripts.** The template ships every check and sensor as a bash script under `scripts/`. A
   script must need nothing beyond bash 4.4 or later, git, coreutils, grep, sed, awk, find, and
   tar. The shell lint may also need ShellCheck.
2. **Self-test.** Each script runs after its own self-test, which runs it on fixture repositories.
3. **Hooks.** A git hook under `.githooks/` must need no more than bash 3.2.
4. **Sensors.** A sensor is a script `scripts/sensor-<name>.sh` whose header says what it counts.
   It must not fail on what it counts until a decision turns it into a check. Its self-test
   `scripts/test-sensor-<name>.sh` must cite this ADR.
5. **Three lists.** `scripts/check-all.sh`, the `checks.yml` workflow, and the required checks of
   `CONTRIBUTING.md` must list the same checks. A job with a matrix is listed by the check runs it
   reports. A workflow that publishes rather than checks is no gate, and `scripts/check-all.sh`
   names it so. Its jobs may run their own commands, and the scripts it runs count.
6. **Local gate.** The git hooks run the checks before a commit, a merge, and a push. Their other
   refusals are [NUC-main-only-via-pr §6](NUC-main-only-via-pr.md). With `--changed`,
   `scripts/check-all.sh` runs a self-test only where its `# Inputs:` line names a changed path. CI
   must run every self-test.
7. **On instruction.** Three kinds of script run only on a human's instruction, never in
   `scripts/check-all.sh` or `checks.yml`:
   - a sensor that reads what only the GitHub repository holds, which may need the GitHub CLI,
     signed in as [NUC-main-only-via-pr §3](NUC-main-only-via-pr.md) says;
   - a sensor whose one run costs a full run of the checks or more;
   - a script that changes the repository instead of reporting on it, such as one that merges,
     commits, moves a branch, or deletes one.

   `scripts/check-all.sh` names each on an `# On instruction:` line. The self-test of each runs
   without the CLI, on fixture responses where it reads GitHub, and stays in the three lists.
8. **Binary.** A project may run a binary in place of the scripts only through an ADR that narrows
   clause 1 ([NUC-base-is-fixed §1](NUC-base-is-fixed.md)). The binary must pass
   the fixture tables under `scripts/fixtures/`.
9. **The root of a script.** Every script `scripts/*.sh` of a repository, the template's and
   a project's own, reads the repository it lives in, or another one only as `--root <dir>`.
   It must take the root through `enter_root` of `scripts/lib/root.sh`. It must refuse with
   exit 2 an argument it does not read, through `rest_args` of the same file. The self-tests
   `scripts/test-*.sh` and `scripts/check-all.sh` are exempt: they run on the repository they
   live in.
   Every tracked script `scripts/*.sh` and `.agents/skills/<name>/*.sh` must be tracked with mode
   100755, so that it runs when called by its path. A symlink, and a library in a subdirectory
   such as `scripts/lib/`, which is sourced and not run, are exempt.

**Out of scope:**

- the language and build of a project's own code, which its `toolchain` role ADR decides
  ([NUC-base-is-fixed §3](NUC-base-is-fixed.md));
- which test cites a decision ([NUC-decisions-cite-tests](NUC-decisions-cite-tests.md));
- whether the template's own checks move to a binary.

## Latitude

A project may choose:

- a binary in place of the bash scripts, under clause 8. Whether the template itself switches
  stays open;
- further checks and sensors of its own, in the language of its toolchain, from the entry
  `toolchain` of the [catalogue](../../.agents/skills/propose-adr/concepts/toolchain.md). A
  sensor of its own in another language falls outside clause 4.

The catalogue has no entry for the binary, and choosing it needs an ADR that narrows clause 1.

## Alternatives considered

- **Every check on bash 3.2, the one macOS ships.** It spares a contributor on macOS the install
  of a current bash. It costs a rewrite of the `mapfile` calls, the associative arrays, and the
  case conversions of the checks. The Dev Container and CI carry a current bash, and a check that
  refuses an old one says so at once.
- **A binary as the template's own checks.** This ADR leaves the switch open and lets a project
  take the binary under clause 8.
- **Run the sensor of the repository settings in CI.** The workflow token reads the rules of a
  branch, but the security settings only with admin rights. CI would need a stored token, which
  [NUC-main-only-via-pr §3](NUC-main-only-via-pr.md) forbids, or would report half of them.
- **A pre-push hook that reads the GitHub repository.** It refuses work for a reason the work
  does not have, whenever the network fails.

## Sources / Prior art

- Birgitta Böckeler, *Harness engineering* —
  <https://martinfowler.com/articles/harness-engineering.html>: a harness is guides that feed
  forward and sensors that feed back.

## Consequences

- Positive: every check runs on every host with bash and git, before a project picks a toolchain.
- Positive: the hooks and CI run the same checks, so a green push predicts a green pull request.
- Positive: each script carries a self-test, so a change to a check shows what it changes.
- Negative: a contributor on macOS installs a current bash, or works in the Dev Container.
- Negative: the scripts of clause 7 run only when a human remembers them, such as the sensors of
  the repository settings and of red commits.
- Negative: a project's own script under `scripts/` takes its root as the template's do, a
  binding a take-up carries to it.
- Follow-ups: whether the template's own checks move to a binary.

## Enforcement

- The rule `docs/check-lists` of `scripts/check-docs.sh` holds the three lists and the scripts run
  on instruction (clauses 5 and 7).
- `scripts/check-shell.sh` lints every script, and `scripts/check-all.sh` refuses a bash below 4.4
  by name (clause 1). The rule `shell/bash-floor` of `scripts/check-shell.sh` holds each script
  that sources the floor to stop below it with its name.
- `scripts/test-check-all.sh` holds `--changed`, and `scripts/test-git-hooks.sh` every refusal of
  the hooks (clause 6).
- `scripts/check-traceability.sh` fails on a sensor without its self-test, or a self-test that does
  not cite this ADR (clause 4). It names this ADR's identifier, which no normalization moves.
- The rule `shell/root` of `scripts/check-shell.sh` holds every script `scripts/*.sh` to clause 9,
  but the self-tests and `scripts/check-all.sh`. Each must call `enter_root`, and exit 2 on
  `--verbose` and on a root that is no directory. An option whose code reads a value after it
  must stand in its usage string as `--<name> <value>`, since `enter_root` reads that string.
- The rule `shell/mode` of `scripts/check-shell.sh` holds each such script to mode 100755 in the
  index (clause 9).

The self-tests of these checks cite this ADR.

**Not mechanically decidable:** the review holds these judgements:

- whether a script needs a further tool (clause 1);
- whether a script runs after its self-test (clause 2);
- whether a hook stays within bash 3.2 (clause 3);
- whether a sensor's header says what it counts, and whether it never fails on that (clause 4);
- whether a script named on instruction is one of the three kinds, and whether each of them
  runs on instruction only (clause 7);
- whether a binary passes the fixture tables, which the narrowing ADR holds (clause 8);
- whether a script refuses every argument it does not read, beyond the `--verbose` the check
  passes (clause 9).
