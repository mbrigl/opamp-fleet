#!/usr/bin/env bash
#
# Sensor: what a derived project acts on to take up the next release.
#
# The tag annotation and the GitHub release must say what a derived project does to take a
# release up (NUC-releases-are-semver-tags §2). Every hand-off says it under Doubts, in the field
# 'A derived project acts on' of .github/PULL_REQUEST_TEMPLATE.md. The hand-off of a branch is the
# description of its pull request into main; a feat/ branch integrates linearly, so no other
# merge carries one (NUC-main-only-via-pr §5). For each merge of a pull request on the
# first-parent line of main since the last release tag, 'Merge pull request #<n> from …', the
# oldest first, this sensor prints that field, the lines that follow it indented included, and
# where it came from: the description, read through gh; where gh cannot read it, the merge
# message, if it carries the field. The subject and the gh call are those of
# scripts/sensor-work-measures.sh, its twin. Any other merge is not read.
#
# A field that says none, nothing, or '-' stands on one line, as do an empty field and a
# description without the field. The headline counts the merges listed. Whoever cuts the release
# writes the notes from this list (NUC-releases-are-semver-tags §2). This project tags its
# releases version/X.Y.Z (README.md → Releases), so it runs with --since version/X.Y.Z.
#
# It runs on a human's instruction, not in scripts/check-all.sh or checks.yml, since it reads
# GitHub (NUC-checks-are-bash §7). It reports and never fails. scripts/test-sensor-release-notes.sh
# runs it on fixture repositories: GH names the command that stands in for gh.
#
# Bash + coreutils + git + grep + sed + awk, plus the GitHub CLI, authenticated through its web
# flow (NUC-main-only-via-pr §3). Usage:
#     scripts/sensor-release-notes.sh [--root <dir>] [--since <tag>]
# The root defaults to this repository. The merges read are those in <tag>..HEAD; the tag
# defaults to the tag v* of the highest version reachable from HEAD. Exit code 0 always, except 2
# on an unknown argument, a root that does not exist or is no git repository, a missing gh, or no
# tag.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"

NAME="Release notes sensor"
usage="scripts/sensor-release-notes.sh [--root <dir>] [--since <tag>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"
since=""
if [[ "${1:-}" == --since ]]; then
  (($# >= 2)) || { echo "sensor-release-notes.sh: '--since' needs a tag — usage: $usage" >&2; exit 2; }
  since="$2"
  shift 2
fi
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "$@"

git -C "$ROOT" rev-parse --git-dir > /dev/null 2>&1 \
  || { echo "$NAME: '$ROOT' is no git repository." >&2; exit 2; }
GH="${GH:-gh}"
command -v "$GH" > /dev/null 2>&1 \
  || { echo "$NAME: the GitHub CLI ($GH) is not installed." >&2; exit 2; }
if [[ -n "$since" ]]; then
  git -C "$ROOT" rev-parse -q --verify "refs/tags/$since" > /dev/null \
    || { echo "$NAME: '$since' is no tag of $ROOT." >&2; exit 2; }
else
  since="$(git -C "$ROOT" tag --merged HEAD --list 'v*' --sort=-v:refname 2>/dev/null | head -n 1)"
  [[ -n "$since" ]] \
    || { echo "$NAME: no tag v* is reachable from HEAD in $ROOT — name one with --since <tag>." >&2; exit 2; }
fi

# field — the field 'A derived project acts on' of the hand-off on stdin: the text after its
# colon and each indented line after it, up to a blank or unindented line, one line each with
# the indent removed. Prints nothing, and fails, where the hand-off has no such field.
field() {
  tr -d '\r' | awk '
    found && (/^[^ \t]/ || /^[ \t]*$/) { exit }
    found { sub(/^[ \t]+/, ""); print; next }
    /^- A derived project acts on:/ {
      found = 1
      sub(/^- A derived project acts on:[ \t]*/, "")
      if ($0 != "") print
    }
    END { exit !found }'
}

# Every merge on the first-parent line since the tag, the oldest first: '<hash> <subject>'.
mapfile -t merges < <(git -C "$ROOT" log --reverse --first-parent --merges --format='%H %s' "$since..HEAD" --)

# The field of each merge of a pull request.
declare -A text from pr_of
for entry in "${merges[@]}"; do
  hash="${entry%% *}" subject="${entry#* }"
  message="$(git -C "$ROOT" log -1 --format=%B "$hash")"
  if [[ "$subject" =~ ^Merge\ pull\ request\ \#([0-9]+)\ from\ [^/]+/(.+)$ ]]; then
    pr_of[$hash]="#${BASH_REMATCH[1]} ${BASH_REMATCH[2]}"
    if body="$(cd "$ROOT" && "$GH" pr view "${BASH_REMATCH[1]}" --json body --jq .body 2>/dev/null)"; then
      from[$hash]=description
      text[$hash]="$(field <<< "$body")" || from[$hash]=no-field
    else
      from[$hash]=message
      text[$hash]="$(field <<< "$message")" || from[$hash]=unread
    fi
  fi
done

lines=()
for entry in "${merges[@]}"; do
  hash="${entry%% *}"
  [[ -n "${from[$hash]:-}" ]] || continue
  name="${pr_of[$hash]}"
  case "${from[$hash]}" in
    no-field) lines+=("  - $name: its pull request description has no field 'A derived project acts on'."); continue ;;
    unread) lines+=("  - $name: its pull request description was not read, and its merge message has no field 'A derived project acts on'."); continue ;;
    description) source_of="from its pull request description" ;;
    message) source_of="from its merge message, since its pull request description was not read" ;;
  esac
  value="${text[$hash]}"
  if [[ -z "$value" ]]; then
    lines+=("  - $name, $source_of: empty")
  elif [[ "$value" != *$'\n'* ]] && grep -qiE '^(none|nothing|-)\.?$' <<< "$value"; then
    lines+=("  - $name, $source_of: $value")
  else
    lines+=("  - $name, $source_of:")
    while IFS= read -r l; do lines+=("      $l"); done <<< "$value"
  fi
done

count="$(grep -c '^  - ' < <(printf '%s\n' "${lines[@]}"))"
if ((${#lines[@]} == 0)); then
  echo "$NAME: no merge since $since — nothing to report."
  exit 0
fi
echo "$NAME: what a derived project acts on, from the $count merge$( ((count == 1)) || echo s) listed since $since:"
printf '%s\n' "${lines[@]}"
exit 0
