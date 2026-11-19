#!/usr/bin/env bash
#
# Sensor: a change a deployment sees, without a CHANGELOG.md entry (NUC-checks-are-bash §4, the sensors the
# template ships).
#
# Lists the commits between a base and a head that a running deployment may have to be told
# about, when no commit of the change touches CHANGELOG.md. Such a commit has a Conventional
# Commits subject of type fix or feat, or marks a breaking change with a '!' after its type or a
# 'BREAKING CHANGE' line, and it touches the shipped code: of a crate the products are built
# from, crates/{opamp,fleet-core,fleet-util,fleet-server,fleet-agent,fleet-tools}/, its src/,
# proto/ or static/, its build.rs or its Cargo.toml; Cargo.lock, which a dependency's fix moves;
# or packaging/. A merge commit counts for the CHANGELOG.md it touches, never as such a commit.
# A change no deployment sees answers the sensor in its pull request, on the line
# 'CHANGELOG.md:' under Documents and decisions (.github/PULL_REQUEST_TEMPLATE.md). It reports
# and never fails.
#
# Pure bash + git + grep. Usage:
#     scripts/sensor-changelog.sh [--root <dir>] [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture.
# Without a base, the merge base of head with origin/main (else main) is used; CI passes the
# pull request's base. A base of zeros, which a push event reports for a new branch, counts as
# none. With nothing to compare — on main without a remote, or base and head the same — it says
# so. Exit code 0 always, except 2 on a base or head that does not resolve, an argument it does
# not read, or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/sensor-changelog.sh [--root <dir>] [base [head]]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"
rest_args "${BASH_SOURCE[0]}" "$usage" 2 "$@"

resolve_change "Changelog sensor" "${1:-}" "${2:-HEAD}" || exit 0
base="$CHANGE_BASE" head="$CHANGE_HEAD"
if [[ "$(git rev-parse "$base^{commit}")" == "$(git rev-parse "$head^{commit}")" ]]; then
  echo "Changelog sensor: nothing to compare — base and head are the same commit."
  exit 0
fi

range="$(git rev-parse --short "$base")..$(git rev-parse --short "$head")"
shipped='^(Cargo\.lock$|crates/(opamp|fleet-core|fleet-util|fleet-server|fleet-agent|fleet-tools)/(src/|proto/|static/|build\.rs$|Cargo\.toml$)|packaging/)'
# A subject 'fix: …', 'feat(scope): …', or any type with '!' before the colon.
visible='^((fix|feat)(\([^)]*\))?!?|[A-Za-z]+(\([^)]*\))?!):'

seen=()
while read -r commit; do
  subject="$(git log -1 --format=%s "$commit")"
  if ! [[ "$subject" =~ $visible ]]; then
    body="$(git log -1 --format=%b "$commit")"
    grep -qE '^BREAKING[ -]CHANGE:' <<< "$body" || continue
  fi
  files="$(git diff-tree --root --no-commit-id --name-only -r "$commit")"
  grep -qE "$shipped" <<< "$files" || continue
  seen+=("$(git log -1 --format='%h %s' "$commit")")
done < <(git rev-list --reverse --no-merges "$base..$head")

if [[ -n "$(git rev-list -1 "$base..$head" -- CHANGELOG.md)" ]]; then
  echo "Changelog sensor ($range): ${#seen[@]} commit(s) a deployment may see;" \
    "the change touches CHANGELOG.md."
  exit 0
fi
echo "Changelog sensor ($range): ${#seen[@]} commit(s) a deployment may see;" \
  "the change does not touch CHANGELOG.md."
if ((${#seen[@]})); then
  echo "Add an entry to CHANGELOG.md, or answer 'CHANGELOG.md: none' and why in the pull request:"
  printf '  - %s\n' "${seen[@]}"
fi
exit 0
