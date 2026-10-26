#!/usr/bin/env bash
#
# Sensor: review binding (AGENTS.md, section 5 — review in a fresh context before handing off;
# section 3 — the ADRs reviewed apart from their implementation).
#
# Whether the review ran in a fresh context no script can tell; the session that wrote a change
# can write any line the reviewer could. What it can tell is which commit the review saw. The
# hand-off of a change, the message of its merge into a feat/ branch, carries in its *Review*
# section the line 'Reviewed at: <commit>' (.github/PULL_REQUEST_TEMPLATE.md). For every merge on
# the first-parent line between a base and a head, this sensor reads that line and holds it
# against the merged branch, the merge's second parent:
#
#   1. No line, or one without a commit: the review is bound to nothing.
#   2. Anything but the hash of a commit of the merged branch that the line merged into did not
#      already have: a hash that does not resolve, a commit elsewhere, or a name such as the
#      branch's, which moves with the branch and so binds nothing.
#   3. Commits between the reviewed one and the branch tip: what landed unreviewed, usually the
#      fixes made after the findings. They are listed, with the diffstat they add up to.
#
# A commit on the first-parent line that is no merge landed outside any merge, so no hand-off
# names it. Those commits are listed too, as named by no review.
#
# A merge whose second parent the base already has, such as the feat/ branch taking up main,
# lands no branch and is passed over. A merge reviewed at its tip is counted and not listed. The
# third and later parents of an octopus merge are not read. The resolution of a conflicting merge
# lies past the tip and is not read, and a merge is read only once it is made, so the pre-merge
# hook does not see the one it gates. It reports and never fails on what it finds; the human who
# decides the merge, or the reviewer of the pull request into main, reads it.
#
# A rework of the feat/ branch, a rebase or a regroup, rewrites the commits these lines name, or
# drops the merges that carry them. The review that binds after one is the review of the whole
# branch, named on the same line in the hand-off of its pull request, its description. Given as a
# file, it is held against the head first: reviewed at the head, it covers every merge the branch
# carries and every commit outside one, and nothing else is read; otherwise it is reported like a
# merge, and the merges follow. Reviewed at an earlier commit of the branch, its list of what came
# after names the commits outside a merge that are unreviewed, so they are not listed again;
# bound to nothing, it leaves them listed.
#
# The review of an ADR comes before its implementation, and the commits keep the two apart
# (AGENTS.md, section 3, rules 3 and 4). Every commit between the base and the head, those of the
# merged branches included, that changes the text of an ADR and a path outside docs/adr/ is
# listed too. Its Status, Date, and Deciders lines are not its text here: the flip to accepted,
# which moves the date, may land with the implementation, and a human adds the project's decider.
# The index under docs/adr/ goes with its ADRs either way. The commits of the exceptions the rules
# name, a normalization and the template repository's correction of its own ADRs, are listed like
# any other, and the human reads them as such.
#
# Pure bash + git. Usage:
#     scripts/sensor-review-binding.sh [--hand-off <file>] [base [head]]
# The head defaults to HEAD, the feat/ branch checked out. Without a base, the merge base of head
# with origin/main (else main) is used: the merges of the branch not yet on the remote's main. CI
# passes the pull request's base and head, never its synthetic merge commit, and its description
# as the hand-off. A base of zeros, which a push event reports for a new branch, counts as none.
# Exit code 0 always, except 2 on a base or head that does not resolve, or a hand-off that is no
# file.

set -uo pipefail

hand_off=""
if [[ "${1:-}" == "--hand-off" ]]; then
  hand_off="${2:-}"
  shift 2 2>/dev/null || shift $#
  [[ -f "$hand_off" ]] \
    || { echo "Review binding sensor: hand-off '$hand_off' is no file." >&2; exit 2; }
fi

base="${1:-}"
head="${2:-HEAD}"

[[ "$base" =~ ^0+$ ]] && base=""
if [[ -z "$base" ]]; then
  for candidate in origin/main main; do
    if git rev-parse --verify -q "$candidate^{commit}" >/dev/null; then
      base="$(git merge-base "$candidate" "$head" 2>/dev/null)" && break
    fi
  done
fi
if [[ -z "$base" ]]; then
  echo "Review binding sensor: nothing to compare — no base given and no main to compare with."
  exit 0
fi
for ref in "$base" "$head"; do
  git rev-parse --verify -q "$ref^{commit}" >/dev/null \
    || { echo "Review binding sensor: '$ref' does not resolve to a commit." >&2; exit 2; }
done

at_tip=0
findings=()

# bind <label> <message> <tip> <before> <what> — hold the message's 'Reviewed at:' line against
# the commits after <before> up to <tip>, <what> naming them in a finding. Returns 0 for a
# review at the tip, 2 with the commits after the review added as a finding, 1 with a finding
# that it binds nothing.
bind() {
  local label="$1" message="$2" tip="$3" before="$4" what="$5" line reviewed sha n finding c
  line="$(grep -m1 -E '^[[:space:]]*([-*][[:space:]]+)?Reviewed at:[[:space:]]*[^[:space:]]' <<< "$message")"
  reviewed="$(sed -E 's/^[^:]*:[[:space:]]*//; s/[[:space:]].*$//; s/`//g' <<< "$line")"
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
  git diff-tree -r --root --no-commit-id -p -U0 --no-color "$1" \
    -- ':(glob)docs/adr/[0-9][0-9][0-9][0-9]-*.md' | awk '
    /^diff --git / { file = $NF; sub(/^b\//, "", file); next }
    /^(\+\+\+|---) / { next }
    /^[-+]/ && !/^[-+]- \*\*(Status|Date|Deciders):\*\*/ && !(file in seen) { seen[file]; print file }
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
  echo "Review binding sensor: ${#mixed[@]} commit$( ((${#mixed[@]} == 1)) || echo s) mixing ADR text with other paths, which AGENTS.md section 3 keeps in commits apart:"
  printf '  - %s\n' "${mixed[@]}"
}

mapfile -t all < <(git rev-list --first-parent --merges --reverse "$base..$head")
merges=()
for m in "${all[@]}"; do
  git merge-base --is-ancestor "$m^2" "$base" || merges+=("$m")
done

# The commits that landed on the first-parent line outside any merge: no merge's line names them.
mapfile -t linear < <(git rev-list --first-parent --no-merges --reverse "$base..$head")

# The hand-off of the whole branch first: a review of its head covers every merge it carries and
# every commit outside one, whatever a rework did to the lines in their messages.
hand_off_bound=1
if [[ -n "$hand_off" ]]; then
  bind "the hand-off" "$(cat "$hand_off")" "$head" "$base" "the branch since the base"
  hand_off_bound=$?
fi
if ((hand_off_bound == 0)); then
  covered=""
  ((${#merges[@]})) && covered=", which covers the ${#merges[@]} merge$( ((${#merges[@]} == 1)) || echo s) it carries"
  echo "Review binding sensor: the hand-off was reviewed at the head, $(git rev-parse --short "$head")$covered."
  report_mixed
  exit 0
fi

for m in "${merges[@]}"; do
  if bind "$(git log -1 --format='%h %s' "$m")" "$(git log -1 --format=%B "$m")" "$m^2" "$m^1" \
    "the merged branch"; then
    at_tip=$((at_tip + 1))
  fi
done

# A hand-off reviewed at a commit of the branch has listed what came after it, the commits
# outside a merge among them.
if ((${#linear[@]} && hand_off_bound != 2)); then
  finding="${#linear[@]} commit$( ((${#linear[@]} == 1)) || echo s) landed outside a merge, named by no review:"
  for c in "${linear[@]}"; do finding+=$'\n'"    $(git log -1 --format='%h %s' "$c")"; done
  findings+=("$finding")
fi

if ((${#merges[@]} == 0)); then
  echo "Review binding sensor: no merge since $(git rev-parse --short "$base")$( ((${#findings[@]} + ${#mixed[@]})) || echo " — nothing to read")."
else
  echo "Review binding sensor: ${#merges[@]} merge$( ((${#merges[@]} == 1)) || echo s) since $(git rev-parse --short "$base"), $at_tip reviewed at the tip of its branch."
fi
for f in "${findings[@]}"; do
  printf '  - %s\n' "$f"
done
report_mixed
exit 0
