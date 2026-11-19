#!/usr/bin/env bash
#
# Sensor: review binding (AGENTS.md, section 5 — review in a fresh context before handing off;
# section 3 — the ADRs reviewed apart from their implementation).
#
# Whether the review ran in a fresh context no script can tell; the session that wrote a change
# can write any line the reviewer could. What it can tell is which commit the review saw. A
# branch integrates linearly (NUC-main-only-via-pr §5), so no merge message carries a hand-off:
# the hand-off of a branch is the description of its pull request. Its *Review* section carries
# the line 'Reviewed at: <commit>' (.github/PULL_REQUEST_TEMPLATE.md). Given that description as
# a file with --hand-off, this sensor holds the line against the commits between a base and a
# head:
#
#   1. No line, or one without a commit: the review is bound to nothing.
#   2. Anything but the hash of a commit of the branch since the base: a hash that does not
#      resolve, a commit elsewhere, or a name such as the branch's, which moves with the branch
#      and so binds nothing.
#   3. Commits between the reviewed one and the head: what landed unreviewed, usually the fixes
#      made after the findings, or a rework. They are listed, with the diffstat they add up to.
#
# Reviewed at the head, the hand-off covers every commit since the base. Without --hand-off, as
# scripts/check-all.sh runs it, the sensor counts the commits and says that only a hand-off binds
# them. A rework, a rebase or a regroup, rewrites the commit the line names; the review that binds
# after one is a review of the whole branch at its head.
#
# A merge between the base and the head is listed, since a feat/ branch carries none
# (NUC-main-only-via-pr §5); the commits it brought are counted with the others, and its message
# is not read. A synthetic merge of CI is no such merge: CI passes the pull request's head. It
# reports and never fails on what it finds; the human who decides the merge, or the reviewer of
# the pull request into main, reads it.
#
# The review of an ADR comes before its implementation, and the commits keep the two apart
# (NUC-decisions-are-proposed-then-accepted §3, §4). Every commit between the base and the head,
# those of the merged branches included, that changes the text of an ADR and a path outside
# docs/adr/ is listed too. Its Status, Date, and Deciders lines are not its text here: the flip to
# accepted, which moves the date, may land with the implementation, and the Deciders line is the
# human's record of who decided, not the decision. The index under docs/adr/ goes with its ADRs
# either way. The commits of the exceptions the rules name, a normalization and the template
# repository's correction of its own ADRs, are listed like any other, and the human reads them as
# such.
#
# Pure bash + git. Usage:
#     scripts/sensor-review-binding.sh [--root <dir>] [--hand-off <file>] [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture.
# The head defaults to HEAD, the branch checked out. Without a base, the merge base of head with
# origin/main (else main) is used: the commits of the branch not yet on the remote's main. CI
# passes the pull request's base and head, never its synthetic merge commit, and its description
# as the hand-off. A base of zeros, which a push event reports for a new branch, counts as none.
# Exit code 0 always, except 2 on a base or head that does not resolve, a hand-off that is no
# file, an argument it does not read, or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"
# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"

caller="$PWD"
# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/sensor-review-binding.sh [--root <dir>] [--hand-off <file>] [base [head]]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"

# The hand-off is a path from where the sensor was called.
hand_off=""
if [[ "${1:-}" == "--hand-off" ]]; then
  hand_off="${2:-}"
  shift 2 2>/dev/null || shift $#
  [[ -z "$hand_off" || "$hand_off" == /* ]] || hand_off="$caller/$hand_off"
  [[ -f "$hand_off" ]] \
    || { echo "Review binding sensor: hand-off '$hand_off' is no file." >&2; exit 2; }
fi

rest_args "${BASH_SOURCE[0]}" "$usage" 2 "$@"
resolve_change "Review binding sensor" "${1:-}" "${2:-HEAD}" || exit 0
base="$CHANGE_BASE" head="$CHANGE_HEAD"

findings=()

# bind <label> <message> <tip> <before> <what> — hold the message's 'Reviewed at:' line against
# the commits after <before> up to <tip>, <what> naming them in a finding. Returns 0 for a
# review at the tip, 2 with the commits after the review added as a finding, 1 with a finding
# that it binds nothing.
bind() {
  local label="$1" message="$2" tip="$3" before="$4" what="$5" line reviewed sha n finding c
  line="$(grep -m1 -E '^[[:space:]]*([-*][[:space:]]+)?Reviewed at:[[:space:]]*[^[:space:]]' <<< "$message")"
  reviewed="$(sed -E 's/^[^:]*:[[:space:]]*//; s/[[:space:],].*$//; s/`//g' <<< "$line")"
  if [[ -z "$reviewed" ]]; then
    findings+=("$label: no 'Reviewed at:' commit in its message — the review is bound to nothing")
    return 1
  fi
  # A hash only: a branch name or 'HEAD~1' resolves to wherever it points now, not to what the
  # review saw, and so does a ref whose name looks like one, which git prefers to the hash.
  sha=""
  [[ "$reviewed" =~ ^[0-9a-fA-F]{7,40}$ ]] && sha="$(git rev-parse --verify -q "$reviewed^{commit}" 2>/dev/null)"
  [[ "$sha" == "${reviewed,,}"* ]] || sha=""
  if [[ -z "$sha" ]] || ! git merge-base --is-ancestor "$sha" "$tip" \
    || git merge-base --is-ancestor "$sha" "$before"; then
    findings+=("$label: reviewed at '$reviewed', which is no commit of $what")
    return 1
  fi
  if [[ "$sha" == "$(git rev-parse "$tip")" ]]; then
    return 0
  fi
  n="$(git rev-list --count "$sha..$tip")"
  finding="$label: $n commit$( ((n == 1)) || echo s) after the review at $(git rev-parse --short "$sha"), unreviewed:"
  while IFS= read -r c; do finding+=$'\n'"    $c"; done < <(git log --format='%h %s' "$sha..$tip")
  while IFS= read -r c; do finding+=$'\n'"   $c"; done < <(git diff --stat "$sha" "$tip")
  findings+=("$finding")
  return 2
}

# adr_text <commit> — the ADR files whose text the commit changes, Status, Date, and Deciders
# lines aside.
adr_text() {
  git diff-tree -r --root --no-commit-id -p -U0 --no-color "$1" -- ":(glob)docs/adr/$ADR_GLOB" \
    | awk -v flip="^[-+]${ADR_ITEM_RE#^}(Status|Date|Deciders):[*][*]" '
    /^diff --git / { file = $NF; sub(/^b\//, "", file); next }
    /^(\+\+\+|---) / { next }
    /^[-+]/ && $0 !~ flip && !(file in seen) { seen[file]; print file }
  '
}

mixed=()
while IFS= read -r c; do
  mapfile -t texts < <(adr_text "$c")
  ((${#texts[@]})) || continue
  mapfile -t others < <(git diff-tree -r --root --no-commit-id --name-only "$c" -- . ':(exclude)docs/adr/')
  ((${#others[@]})) || continue
  more=""
  ((${#others[@]} > 3)) && more=" and $((${#others[@]} - 3)) more"
  mixed+=("$(git log -1 --format='%h %s' "$c"): changes the text of ${texts[*]} together with ${others[*]:0:3}$more")
done < <(git rev-list --no-merges --reverse "$base..$head")

# report_mixed — list the commits that change ADR text together with other paths.
report_mixed() {
  ((${#mixed[@]})) || return 0
  echo "Review binding sensor: ${#mixed[@]} commit$( ((${#mixed[@]} == 1)) || echo s) mixing ADR text with other paths, which NUC-decisions-are-proposed-then-accepted §4 keeps in commits apart:"
  printf '  - %s\n' "${mixed[@]}"
}

mapfile -t merges < <(git rev-list --merges --reverse "$base..$head")
n="$(git rev-list --count --no-merges "$base..$head")"
commits="$n commit$( ((n == 1)) || echo s)"
since="$(git rev-parse --short "$base")"

if ((${#merges[@]})); then
  finding="${#merges[@]} merge$( ((${#merges[@]} == 1)) || echo s) between the base and the head; a feat/ branch carries none (NUC-main-only-via-pr §5), and no review is read from $( ((${#merges[@]} == 1)) && echo its || echo their) message$( ((${#merges[@]} == 1)) || echo s):"
  for m in "${merges[@]}"; do finding+=$'\n'"    $(git log -1 --format='%h %s' "$m")"; done
  findings+=("$finding")
fi

# below — what the headline says of the findings listed under it, the mixed commits as one, so
# that a reader of the headline alone, such as check-all.sh, learns that there are some.
below() {
  local k=$(( ${#findings[@]} + (${#mixed[@]} > 0) ))
  ((k)) && echo "; $k finding$( ((k == 1)) || echo s) below"
}

# report — print the findings and the mixed commits under the headline, and stop.
report() {
  local f
  for f in "${findings[@]}"; do printf '  - %s\n' "$f"; done
  report_mixed
  exit 0
}

if ((n == 0 && ${#merges[@]} == 0)); then
  echo "Review binding sensor: no commit since $since — nothing to read."
  exit 0
fi

if [[ -z "$hand_off" ]]; then
  echo "Review binding sensor: $commits since $since, bound only by a hand-off: name the pull request's description with --hand-off <file>$(below)."
  report
fi

if bind "the hand-off" "$(cat "$hand_off")" "$head" "$base" "the branch since the base"; then
  echo "Review binding sensor: the hand-off was reviewed at the head, $(git rev-parse --short "$head"), which covers the $commits since $since$(below)."
else
  echo "Review binding sensor: $commits since $since, bound only by the hand-off$(below)."
fi
report
