#!/usr/bin/env bash
#
# GitHub Actions reference check for this repository (ADR-0007).
#
# ADR-0007 references every action by its major version tag and lets Dependabot raise the pull
# request for a new major. This check reads the files the decision names and fails on what would
# break it:
#
#   1. References: every 'uses:' on an uncommented line of .github/workflows/*.yml|*.yaml names a
#      major version tag — 'owner/action@vN', or 'owner/action/path@vN' for an action inside a
#      repository. A commit SHA, a branch, a full version, and an untagged reference fail. A
#      local action ('./…') is exempt; a 'docker://' image carries an explicit ':<tag>'.
#   2. Dependabot: .github/dependabot.yml exists and has an entry with
#      'package-ecosystem: github-actions', the one that raises the major bump.
#
# A 'uses:' inside a full-line '#' comment is not read: a comment is not a reference. A trailing
# comment after the reference is dropped, and quotes around the value are stripped. Only the
# block form is read, 'uses:' at the start of a line or list item; a flow-style step
# ('- {uses: …}') is not, and none of this repository's workflows is written that way.
#
# Pure bash + coreutils/grep/sed. Usage:
#     scripts/check-action-refs.sh [repository root]
# The root defaults to this repository; scripts/test-check-action-refs.sh passes fixtures.
# Exit code 0 when the check passes, 1 otherwise.

set -uo pipefail

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
WORKFLOWS=".github/workflows"
DEPENDABOT=".github/dependabot.yml"
MAJOR_TAG_RE='^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(/[A-Za-z0-9_./-]+)?@v[0-9]+$'

errors=()
add_error() { errors+=("$1"); }

shopt -s nullglob
workflows=("$ROOT/$WORKFLOWS"/*.yml "$ROOT/$WORKFLOWS"/*.yaml)
shopt -u nullglob

if ((${#workflows[@]} == 0)); then
  add_error "$WORKFLOWS: no workflow found — ADR-0007 governs the action references there"
fi

# 1. References.
refs=0
for wf in "${workflows[@]}"; do
  rel="${wf#"$ROOT"/}"
  while IFS= read -r hit; do
    n="${hit%%:*}"
    value="$(sed -E "s/^[^:]*:[[:space:]]*(- )?uses:[[:space:]]*//; s/[[:space:]]+#.*$//; s/^[\"']//; s/[\"']$//; s/[[:space:]]+$//" <<< "$hit")"
    refs=$((refs + 1))
    case "$value" in
      ./*) ;;
      docker://*)
        # The tag sits on the last path segment; a registry port ('host:5000/image') is not one.
        image="${value#docker://}"
        [[ "${image##*/}" == *:* ]] \
          || add_error "$rel:$n: '$value' carries no tag — an image is referenced with an explicit ':<tag>' (ADR-0007)"
        ;;
      *)
        if [[ "$value" != *@* ]]; then
          add_error "$rel:$n: '$value' names no ref — reference the action by its major version tag, 'owner/action@vN' (ADR-0007)"
        elif ! [[ "$value" =~ $MAJOR_TAG_RE ]]; then
          add_error "$rel:$n: '$value' is not a major version tag — a SHA never receives updates, a branch is not a release, and a full version is as mutable as the major tag without its updates; write 'owner/action@vN' (ADR-0007)"
        fi
        ;;
    esac
  done < <(grep -nE '^[[:space:]]*(- )?uses:' "$wf")
done

# 2. Dependabot.
if [[ ! -f "$ROOT/$DEPENDABOT" ]]; then
  add_error "$DEPENDABOT: not found — Dependabot raises the pull request for a new major of an action (ADR-0007)"
elif ! grep -vE '^[[:space:]]*#' "$ROOT/$DEPENDABOT" \
       | grep -qE "^[[:space:]]*(- )?package-ecosystem:[[:space:]]*[\"']?github-actions[\"']?[[:space:]]*(#.*)?$"; then
  add_error "$DEPENDABOT: has no 'package-ecosystem: github-actions' entry — without it no pull request arrives when an action publishes a new major (ADR-0007)"
fi

if ((${#errors[@]})); then
  echo "Action reference check FAILED:" >&2
  printf '  - %s\n' "${errors[@]}" >&2
  exit 1
fi

echo "Action reference check passed ($refs references on major version tags, Dependabot configured)."
exit 0
