#!/usr/bin/env bash
#
# Gives every draft ADR its number (ADR-0002). A draft is named ADR-draft-<slug>, in the file
# docs/adr/draft-<slug>.md, until the pull request that brings it to main; on the branch of that
# pull request, once it has taken up the current main, this script gives the drafts the next free
# numbers, in the order their files were added on the branch, and commits twice:
#
#   1. Only docs/adr/: each draft renamed to NNNN-<slug>.md, its heading, its index row, and every
#      reference the ADRs make to it, Supersedes fields and 'superseded by' lines included.
#      The rows of each table of the index then run in the order of their numbers.
#   2. Only what lies outside docs/adr/: every other reference, Verifies: markers and links
#      included. Its body carries the map from each draft identifier to its number.
#
# Neither commit changes the text of an ADR together with another path, so rule 4 of AGENTS.md,
# section 3 holds for both. Between the two, the rest of the tree cites draft identifiers no file
# carries; the checks fail on the first commit and pass on the second, and they run on the tip.
#
# It refuses the checkout of main, a working tree with changes or untracked files, since it
# commits, and a branch that has not taken up the tip of origin/main, else main: a number free on
# an older main may be taken. A commit that fails leaves what it would have held in the working
# tree, and says so.
#
# Pure bash + coreutils/grep/sed/find, plus git.
#
# Usage:
#     scripts/number-adrs.sh [--root <dir>]
# It numbers the drafts of the repository it lives in; --root names another, as its self-test
# does with each fixture. Exit code 0 when the drafts are numbered or there are none, 1 when it
# refuses, 2 on an unknown argument.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

enter_root "number-adrs" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"
if (($#)); then
  echo "number-adrs: unknown argument '$1' — usage: scripts/number-adrs.sh [--root <dir>]" >&2
  exit 2
fi

refuse() { echo "number-adrs: refused — $1" >&2; exit 1; }

mapfile -t drafts < <(find docs/adr -maxdepth 1 -type f -name 'draft-*.md' 2>/dev/null | LC_ALL=C sort)
if ((${#drafts[@]} == 0)); then
  echo "number-adrs: no draft under docs/adr/, nothing to number."
  exit 0
fi

[[ "$(git symbolic-ref --short -q HEAD)" != main ]] \
  || refuse "the checkout is on main; number on the branch of the pull request into main"
[[ -z "$(git status --porcelain)" ]] \
  || refuse "the working tree has changes or untracked files; commit or stash them first"

main=""
for candidate in origin/main main; do
  if git rev-parse -q --verify "$candidate^{commit}" > /dev/null; then main="$candidate"; break; fi
done
[[ -n "$main" ]] || refuse "neither origin/main nor main exists, so the next free number is unknown"
git merge-base --is-ancestor "$main" HEAD \
  || refuse "the branch has not taken up $main; merge or rebase onto it first, then number"
base="$(main_merge_base HEAD)"

# The drafts in the order their files were added on the branch, a draft renamed there keeping the
# place of its first name; one the branch did not add, such as one main already carried, follows
# in name order.
added=()
while IFS=$'\t' read -r status path renamed; do
  case "$status" in
    A) added+=("$path") ;;
    R*) for i in "${!added[@]}"; do [[ "${added[i]}" == "$path" ]] && added[i]="$renamed"; done ;;
  esac
done < <(git log --reverse -M --diff-filter=AR --format= --name-status "$base..HEAD" -- 'docs/adr/draft-*.md')
ordered=()
for f in "${added[@]}"; do
  [[ -f "$f" && " ${ordered[*]} " != *" $f "* ]] && ordered+=("$f")
done
for f in "${drafts[@]}"; do
  [[ " ${ordered[*]} " == *" $f "* ]] || ordered+=("$f")
done

for f in "${ordered[@]}"; do
  [[ "$(adr_key "$f")" =~ ^draft-$ADR_SLUG_RE$ ]] \
    || refuse "$f: a draft's slug is lower case letters and digits joined by single hyphens"
done

n="$(find docs/adr -maxdepth 1 -type f -name '[0-9][0-9][0-9][0-9]-*.md' | wc -l | tr -d ' ')"
slugs=() numbers=() map=()
for f in "${ordered[@]}"; do
  n=$((n + 1))
  slug="$(adr_key "$f")"
  slug="${slug#draft-}"
  slugs+=("$slug")
  numbers+=("$(printf '%04d' "$n")")
  map+=("ADR-draft-$slug → ADR-$(printf '%04d' "$n")")
done

# rewrite <file> — every reference to a draft in the file, moved to its number. A slug ends where
# no letter, digit, or hyphen follows, so a draft whose slug starts another's is not touched.
rewrite() {
  local file="$1" script="" i
  for i in "${!slugs[@]}"; do
    script+="s/ADR-draft-${slugs[$i]}([^a-z0-9-]|\$)/ADR-${numbers[$i]}\\1/g;"
    script+="s/\\[draft-${slugs[$i]}\\]/[${numbers[$i]}]/g;"
    script+="s/(^|[^a-z0-9-])draft-${slugs[$i]}\\.md/\\1${numbers[$i]}-${slugs[$i]}.md/g;"
  done
  # Written back over the file only when it changed, so its mode stays.
  sed -E "$script" "$file" > "$file.number-adrs" || return 1
  cmp -s "$file.number-adrs" "$file" || cat "$file.number-adrs" > "$file"
  rm -f "$file.number-adrs"
}

# sort_rows <index> — the rows of each table in the index in the order of their numbers, which a
# draft numbered after main gained an ADR would otherwise break.
sort_rows() {
  [[ -f "$1" ]] || return 0
  awk '
    function flush() { if (n) { close(cmd); n = 0 } }
    /^\| \[/ { if (!n) fflush(); print | cmd; n++; next }
    { flush(); print }
    END { flush() }
  ' cmd="LC_ALL=C sort -s -k2,2" "$1" > "$1.number-adrs" || return 1
  cmp -s "$1.number-adrs" "$1" || cat "$1.number-adrs" > "$1"
  rm -f "$1.number-adrs"
}

# text_files <pathspec...> — the tracked text files under the pathspecs that name a draft.
text_files() {
  local f
  while IFS= read -r -d '' f; do
    [[ -f "$f" ]] && grep -Iq 'draft-' "$f" && printf '%s\n' "$f"
  done < <(git ls-files -z -- "$@")
}

subject_numbers="ADR-${numbers[0]}"
((${#numbers[@]} > 1)) && subject_numbers+=" to ADR-${numbers[-1]}"
body="$(printf '%s\n' "${map[@]}")"

# 1. The record.
for i in "${!slugs[@]}"; do
  git mv "docs/adr/draft-${slugs[$i]}.md" "docs/adr/${numbers[$i]}-${slugs[$i]}.md"
done
while IFS= read -r f; do rewrite "$f"; done < <(text_files docs/adr/)
sort_rows docs/adr/README.md
git add -A -- docs/adr/
git commit -q -m "docs(adr): number the drafts as $subject_numbers" -m "$body" \
  || refuse "the commit of the record failed; the renames and rewrites stand uncommitted (git status)"

# 2. What cites it.
while IFS= read -r f; do rewrite "$f"; done < <(text_files . ':(exclude)docs/adr/')
git add -u -- . ':(exclude)docs/adr/'
git commit -q --allow-empty -m "chore: cite $subject_numbers by number, no longer as drafts" -m "$body" \
  || refuse "the commit of the references failed; the record is committed, the rewrites outside docs/adr/ stand uncommitted (git status)"

echo "number-adrs: numbered in two commits:"
printf '  %s\n' "${map[@]}"
