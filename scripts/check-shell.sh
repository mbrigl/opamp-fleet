#!/usr/bin/env bash
#
# Shell lint for this repository.
#
# Runs ShellCheck over every shell script of the repository: every '*.sh' file git does not
# ignore, the git hooks under .githooks/, which git requires to be named without an extension,
# and the Linux packages' maintainer scripts under packaging/linux/, which run as root inside
# every `apt` and `dnf` transaction. Their `#!/bin/sh` line makes ShellCheck hold them to POSIX sh.
# The files come from git, as in every other check, so a script under .scratch/ or under a build
# directory the project ignores is no part of the gate. The scripts under scripts/ and the hooks
# are the repository's entire enforcement layer, so they are held to a lint gate themselves.
#
# ShellCheck is the one check dependency beyond bash + coreutils/git. CI does not install it — the
# GitHub-hosted runner image ships it preinstalled, and the Dev Container installs it on creation
# (.devcontainer/devcontainer.json). Elsewhere, when shellcheck is not on PATH, the
# check skips itself with a notice instead of failing (CI remains the enforcing gate); in a CI
# run (CI=true) a missing shellcheck is an error, never a silent skip.
#
# Beyond ShellCheck it refuses GNU sed's in-place flag, `sed -i`, which BSD sed on macOS reads as
# taking a suffix: check-all.sh, and with it the pre-push hook, runs on any host. Comment lines are
# not read. The rule reads the options right after `sed`, so it misses a flag after a script
# (`sed -e S -i f`), and it reads text, so it flags the words inside a quoted string or after a
# trailing comment; spell those differently.
#
# Usage:
#     scripts/check-shell.sh [repository root]        (or: bash scripts/check-shell.sh)
# The root defaults to this repository; scripts/test-check-shell.sh passes fixtures.
# Exit code 0 when all scripts pass (ShellCheck skipped when unavailable outside CI), 1 otherwise,
# 2 on a root that is no directory.

set -uo pipefail

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" 2>/dev/null && pwd)" \
  || { echo "Shell lint: '${1:-}' is not a directory." >&2; exit 2; }

have_shellcheck=1
if ! command -v shellcheck >/dev/null 2>&1; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: shellcheck not found in a CI run — the runner image is expected to ship it." >&2
    exit 1
  fi
  have_shellcheck=0
fi

# A list git cannot give is an error, not an empty gate.
if ! listed="$(git -C "$ROOT" ls-files --cached --others --exclude-standard -- '*.sh' '.githooks/*' 'packaging/linux/*')"; then
  echo "Shell lint: $ROOT is not a git repository — git lists the files the lint reads." >&2
  exit 1
fi
scripts=()
while IFS= read -r f; do
  [[ -n "$f" && -f "$ROOT/$f" ]] && scripts+=("$ROOT/$f")
done < <(sort -u <<< "$listed")

if ((${#scripts[@]} == 0)); then
  echo "No shell scripts found."
  exit 0
fi

# GNU-only: sed -i, with or without a suffix argument after it, and sed --in-place.
IN_PLACE='(^|[;&|(`[:space:]])sed([[:space:]]+-[a-zA-Z]+)*[[:space:]]+(-[a-zA-Z]*i([[:space:]]|$)|--in-place)'

failed=0
if ((have_shellcheck)); then
  shellcheck "${scripts[@]}" || failed=1
else
  echo "shellcheck is not installed — ShellCheck skipped (CI enforces it); the sed rule runs."
  echo "Install it to run locally: https://www.shellcheck.net"
fi

for f in "${scripts[@]}"; do
  while IFS=: read -r line text; do
    echo "${f#"$ROOT"/}:$line: GNU sed's in-place flag does not run on macOS;" \
      "write through a temporary file instead, in place so FILE keeps its mode:" \
      "sed 'SCRIPT' FILE > FILE.tmp && cat FILE.tmp > FILE && rm FILE.tmp"
    echo "    $text"
    failed=1
  done < <(grep -nE "$IN_PLACE" "$f" | grep -vE '^[0-9]+:[[:space:]]*#')
done

if ((failed)); then
  echo
  echo "Shell lint FAILED (see findings above)."
  exit 1
fi

echo "Shell lint passed (${#scripts[@]} scripts)."
exit 0
