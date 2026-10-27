#!/usr/bin/env bash
#
# Runs every consistency check this repository enforces, in one command.
#
# AGENTS.md §5 requires these checks to pass before pushing. This script is the local equivalent
# of .github/workflows/checks.yml; check 9 of check-docs.sh holds the two lists, the required
# status checks named in README.md, and the scripts under scripts/ together. A script that runs
# on a human's instruction only is named on an '# On instruction:' line below.
#
# Every check runs even after an earlier one fails, so a single run reports everything that is
# wrong instead of the first thing. The exit code is what a pre-push decision is made on.
#
# With --changed, a self-test (scripts/test-*.sh) runs only where the branch changed what it
# tests: the self-test itself or a path its '# Inputs:' header line names, changed in the working
# tree against the merge base with origin/main, else main, untracked files included. A path in
# that line is a file, or with a trailing '/' a directory. A self-test without the line or naming
# a path that does not exist runs, and so does everything on main or without such a merge base.
# The checks, the sensors, and a project's build, test, and lint always run. The hooks run this
# mode before a push and on the conclusion of a conflicted merge; CI runs every self-test, so what
# is skipped here is still decided before anything reaches main.
#
# Pure bash + coreutils/git, and the Rust toolchain for scripts/check-rust.sh. The individual checks
# state their own dependencies — the shell lint needs ShellCheck and skips itself with a notice
# where it is missing.
#
# Usage:
#     scripts/check-all.sh [--changed]        (or: bash scripts/check-all.sh [--changed])
# CHECK_ALL_JOBS=<n> runs at most n checks at once, 1 one after another; the default is the
# number of processors.
# Exit code 0 when every check passes, 1 when any of them fails, 2 on an unknown argument or a
# CHECK_ALL_JOBS that is no number from 1 up.

set -uo pipefail

# The checks need bash 4.4 or later (ADR-0002). This guard runs first and is written for bash
# 3.2, so an older bash, such as the one macOS ships, gets a message instead of a check failing
# on a construct it lacks. CHECK_ALL_BASH_VERSION stands in for the version found, for the
# self-test.
bash_found="${CHECK_ALL_BASH_VERSION:-${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}}"
bash_major="${bash_found%%.*}" bash_minor="${bash_found#*.}"
bash_minor="${bash_minor%%.*}"
if ((bash_major < 4 || (bash_major == 4 && bash_minor < 4))); then
  echo "check-all: the checks need bash 4.4 or later (ADR-0002); this is bash $bash_found — run them in the Dev Container, or install a current bash" >&2
  exit 2
fi

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

changed_mode=0
case "${1:-}" in
  "") ;;
  --changed) changed_mode=1 ;;
  *) echo "check-all: unknown argument '$1' — usage: scripts/check-all.sh [--changed]" >&2; exit 2 ;;
esac

# The paths changed against the merge base with origin/main, else main, read once: the local
# main may lag the remote's, or a clone without the hooks may have moved it. On main, or without
# a merge base, changed mode is off and everything runs.
changed=()
if ((changed_mode)); then
  branch="$(git -C "$ROOT" symbolic-ref --short -q HEAD 2>/dev/null || true)"
  base="$(cd "$ROOT" && main_merge_base HEAD)"
  if [[ "$branch" == "main" || -z "$base" ]]; then
    changed_mode=0
  else
    # NUL-separated: on lines, git quotes a path with a special character, which then matches no
    # input.
    mapfile -d '' -t changed < <(
      git -C "$ROOT" diff -z --name-only --no-renames "$base" --
      git -C "$ROOT" ls-files -z --others --exclude-standard
    )
  fi
fi

# needs_run <script> — whether a script runs in changed mode: it is no self-test, it names no
# inputs or one that does not exist, or a changed path is the self-test or one of its inputs.
needs_run() {
  local script="$1" line path c
  local -a inputs
  [[ "$script" == scripts/test-*.sh ]] || return 0
  line="$(grep -m1 '^# Inputs:' "$ROOT/$script")" || return 0
  read -r -a inputs <<< "${line#"# Inputs:"}"
  ((${#inputs[@]})) || return 0
  for path in "${inputs[@]}"; do
    [[ -e "$ROOT/$path" ]] || return 0
  done
  for c in "${changed[@]}"; do
    [[ "$c" == "$script" ]] && return 0
    for path in "${inputs[@]}"; do
      if [[ "$path" == */ ]]; then
        [[ "$c" == "$path"* ]] && return 0
      else
        [[ "$c" == "$path" ]] && return 0
      fi
    done
  done
  return 1
}

# The checks run side by side, as many at once as the machine has processors, or as
# CHECK_ALL_JOBS says; 1 runs them one after another. Each writes to a file of its own, and the
# files are printed in the order of the list below once all have finished, so the output reads as
# a run one after another would. A check therefore depends on no other and writes nowhere another
# reads: the self-tests build their fixtures in a directory of their own, and a run line added
# for a project's build keeps to the same. A runner stopped by a signal stops the checks it
# started and waits for them; what a check started in turn may outlive it.
max_jobs="${CHECK_ALL_JOBS:-$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
if [[ ! "$max_jobs" =~ ^[1-9][0-9]*$ ]]; then
  echo "check-all: CHECK_ALL_JOBS is '$max_jobs', not a number of checks from 1 up" >&2
  exit 2
fi
out="$(mktemp -d)"
labels=() running=0
# stop — on every exit, a signal's included: stop every job the runner still has, the one started
# an instant before included, wait for them, and remove their files.
# shellcheck disable=SC2317 # run by the trap
stop() {
  local running_jobs
  running_jobs="$(jobs -p)"
  # shellcheck disable=SC2086 # one process id per word
  [[ -n "$running_jobs" ]] && kill $running_jobs 2>/dev/null
  wait
  rm -rf "$out"
}
trap stop EXIT

# run <label> <script> [args...] — start one check, its output and exit code kept under $out. The
# subshell passes a TERM on to the check and waits for it, so stop() returns once each has ended;
# its trap stands before the check starts, so a TERM never finds the check without it.
run() {
  local label="$1" script="$2" i="${#labels[@]}"
  shift 2
  labels+=("$label")
  if ((changed_mode)) && ! needs_run "$script"; then
    echo "skipped — neither it nor its inputs changed against main; CI runs it" > "$out/$i"
    echo 0 > "$out/$i.rc"
    return
  fi
  while ((running >= max_jobs)); do wait -n; running=$((running - 1)); done
  {
    check=""
    trap '[[ -n "$check" ]] && kill "$check" 2>/dev/null && wait "$check"; exit 143' TERM
    bash "$ROOT/$script" "$@" > "$out/$i" 2>&1 &
    check=$!
    wait "$check"
    echo $? > "$out/$i.rc"
  } &
  running=$((running + 1))
}

run "Documentation self-test" scripts/test-check-docs.sh
run "Protocol Baseline self-test" scripts/test-check-protocol-baseline.sh
run "Documentation consistency" scripts/check-docs.sh
# It reads the network, and says so and skips without one.
run "Protocol Baseline" scripts/check-protocol-baseline.sh
run "Manual check self-test" scripts/test-check-manual.sh
run "Manual cites no ADR" scripts/check-manual.sh
run "Normalize map self-test" scripts/test-normalize-remap.sh
# The numbering script commits and runs on a human's instruction, before the pull request that
# brings drafts to main (ADR-0002); its self-test runs here.
# On instruction: scripts/number-adrs.sh
run "Numbering script self-test" scripts/test-number-adrs.sh
# The readiness script merges and commits on a human's instruction, as step 7 of the README's
# Integration branches; its self-test runs here.
# On instruction: scripts/ready-for-main.sh
run "Readiness script self-test" scripts/test-ready-for-main.sh
# The drop script moves main and deletes a branch on a human's instruction, as step 8 of the
# README's Integration branches; its self-test runs here.
# On instruction: scripts/drop-merged-branch.sh
run "Drop script self-test" scripts/test-drop-merged-branch.sh
run "Traceability self-test" scripts/test-check-traceability.sh
run "Traceability" scripts/check-traceability.sh
run "Dev Container self-test" scripts/test-check-devcontainer.sh
run "Dev Container" scripts/check-devcontainer.sh
run "Tests-kept sensor self-test" scripts/test-sensor-tests-kept.sh
run "Tests-kept sensor" scripts/sensor-tests-kept.sh
run "Binding ADRs sensor self-test" scripts/test-sensor-binding-adrs.sh
run "Binding ADRs sensor" scripts/sensor-binding-adrs.sh
run "Revision-due sensor self-test" scripts/test-sensor-revision-due.sh
run "Revision-due sensor" scripts/sensor-revision-due.sh
run "Plan frontier sensor self-test" scripts/test-sensor-plan-frontier.sh
run "Plan frontier sensor" scripts/sensor-plan-frontier.sh
run "Action references sensor self-test" scripts/test-sensor-action-refs.sh
run "Action references sensor" scripts/sensor-action-refs.sh
run "Review binding sensor self-test" scripts/test-sensor-review-binding.sh
run "Review binding sensor" scripts/sensor-review-binding.sh
run "Action reference self-test" scripts/test-check-action-refs.sh
run "Action references" scripts/check-action-refs.sh
run "Build hardening self-test" scripts/test-check-build-hardening.sh
run "Build hardening" scripts/check-build-hardening.sh
run "Fixture isolation" scripts/test-fixture-isolation.sh
# The repository settings sensor reads GitHub and runs on a human's instruction (ADR-0002); its
# self-test runs here.
# On instruction: scripts/sensor-repo-settings.sh
run "Repository settings sensor self-test" scripts/test-sensor-repo-settings.sh
run "Check runner self-test" scripts/test-check-all.sh
run "Git hooks self-test" scripts/test-git-hooks.sh
run "Shell lint self-test" scripts/test-check-shell.sh
run "Shell lint" scripts/check-shell.sh
run "Build, test and lint" scripts/check-rust.sh
# The development tools a maintainer runs by hand: the development PKI, the toolchain's extra
# tools, the test Configurations of a development Server, and the vendored Shiki bundle.
# On instruction: scripts/dev-pki.sh
# On instruction: scripts/install_tools.sh
# On instruction: scripts/seed_test_configs.sh
# On instruction: scripts/vendor-shiki.sh
# The gates the other workflows run: each says so and skips where this machine lacks what it
# needs — Windows or macOS, a store named in the environment, cargo-deny, cargo-fuzz, Go, a
# throwaway machine — and fails instead in a CI run, where a skip would read as a pass.
run "Client on its platform" scripts/check-client-platform.sh
run "S3 store" scripts/check-store.sh s3
run "PostgreSQL store" scripts/check-store.sh postgres
run "Release build of the Client" scripts/check-release-build.sh fleet-agent
run "Release build of the Server" scripts/check-release-build.sh fleet-server
run "Supply chain" scripts/check-deny.sh
run "Fuzzing" scripts/check-fuzz.sh
run "Interop with opamp-go" scripts/check-interop.sh
run "Service smoke test" scripts/check-service-smoke.sh
# The release publishes; it is no gate, and check-build-hardening.sh holds its steps.
# Not a gate: .github/workflows/release.yml

wait

failed=()
for i in "${!labels[@]}"; do
  printf '\n==> %s\n' "${labels[i]}"
  cat "$out/$i"
  [[ "$(cat "$out/$i.rc" 2>/dev/null)" == 0 ]] || failed+=("${labels[i]}")
done

printf '\n'
if ((${#failed[@]})); then
  echo "FAILED:" >&2
  printf '  - %s\n' "${failed[@]}" >&2
  exit 1
fi
echo "All checks passed."
