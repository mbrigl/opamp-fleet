#!/usr/bin/env bash
#
# Sensor: repository settings.
#
# Some rules of this project only the GitHub repository can enforce; README.md lists them under
# 'Repository settings'. This sensor reads them through the GitHub CLI and reports each one that
# differs from that list (ADR-0002, sensors of the GitHub repository):
#
#   1. The rules in effect on main, from every ruleset that targets it: a pull request required,
#      merge commit the only merge method, with no linear history required and a merge queue,
#      if any, merging by merge commit, the Code Owners review, the required status checks
#      against the job names between the 'required-checks' markers of README.md, a branch up to
#      date before it merges, and force pushes and the branch's deletion blocked.
#   2. Secret scanning with push protection. GitHub shows it to an admin of the repository only;
#      to anyone else the sensor says it could not read it.
#   3. Private vulnerability reporting.
#
# A project with a single maintainer leaves the Code Owners review off, since nobody could give
# it; the sensor reports it all the same, and the reviewer reads it against the maintainers the
# harness ADR names.
#
# It runs on a human's instruction, not in scripts/check-all.sh or checks.yml: the workflow
# token cannot read every setting, and a gate on the network would refuse work for a reason the
# work does not have. It reports and never fails. scripts/test-sensor-repo-settings.sh runs it
# on fixture responses: GH names the command that stands in for gh.
#
# Bash + coreutils + grep + sed, plus the GitHub CLI, authenticated through its web flow (AGENTS.md,
# section 6); the JSON is read by gh's own --jq. Usage:
#     scripts/sensor-repo-settings.sh [repository root]
# The repository is the one gh resolves from the root's git remote. Exit code 0 always, except
# 2 when the root does not exist, gh is missing, or gh cannot resolve the repository.

set -uo pipefail

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" 2>/dev/null && pwd)" \
  || { echo "Repository settings sensor: '${1:-}' is not a directory." >&2; exit 2; }
GH="${GH:-gh}"
NAME="Repository settings sensor"

command -v "$GH" >/dev/null 2>&1 \
  || { echo "$NAME: the GitHub CLI ($GH) is not installed." >&2; exit 2; }
repo="$(cd "$ROOT" && "$GH" repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null)"
[[ -n "$repo" ]] \
  || { echo "$NAME: gh cannot resolve the repository of $ROOT — authenticate with 'gh auth login', or add the remote." >&2; exit 2; }

findings=()
finding() { findings+=("$1"); }

# The required checks README.md names, between its 'required-checks' markers.
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
expected="$(sed -n '/required-checks begin/,/required-checks end/p' "$ROOT/README.md" 2>/dev/null \
  | grep -oE '`[a-z][a-z0-9-]*`' | tr -d '`' | sort -u | tr '\n' ' ')"

# 1. The rules in effect on main, one line per rule of every ruleset, fields split by a tab:
# 'pull_request <methods> <code owners>', 'strict <true|false>' and one 'check <name>' per
# required check of a required_status_checks rule, 'merge_queue <method>', and the bare type of
# any other rule.
tab=$'\t'
rules="$("$GH" api --paginate "repos/$repo/rules/branches/main" --jq '.[] |
  if .type == "pull_request" then
    "pull_request\t\((.parameters.allowed_merge_methods // ["merge", "rebase", "squash"]) | sort | join(","))\t\(.parameters.require_code_owner_review)"
  elif .type == "required_status_checks" then
    ("strict\t\(.parameters.strict_required_status_checks_policy)",
     (.parameters.required_status_checks[] | "check\t\(.context)"))
  elif .type == "merge_queue" then
    "merge_queue\t\(.parameters.merge_method)"
  else .type end' 2>/dev/null)"
rules_rc=$?
if ((rules_rc)); then
  finding "main: the rules in effect could not be read"
elif [[ -z "$rules" ]]; then
  finding "main: no ruleset is in effect — set up the ruleset under README → Repository settings"
else
  # GitHub applies the strictest of several rules: the merge methods every one allows, the Code
  # Owners review and an up-to-date branch where any requires them, and every check any names.
  if ! grep -q "^pull_request$tab" <<< "$rules"; then
    finding "main: no pull request is required"
  else
    methods="merge,rebase,squash" codeowners=false
    while IFS="$tab" read -r _ allowed owners; do
      kept=""
      for m in ${methods//,/ }; do [[ ",$allowed," == *",$m,"* ]] && kept+="${kept:+,}$m"; done
      methods="$kept"
      [[ "$owners" == true ]] && codeowners=true
    done < <(grep "^pull_request$tab" <<< "$rules")
    [[ "$methods" == merge ]] \
      || finding "main: the merge methods are '${methods:-none}', not merge commit only"
    [[ "$codeowners" == true ]] \
      || finding "main: the Code Owners review is not required (expected without a second maintainer)"
  fi
  mapfile -t actual < <(grep "^check$tab" <<< "$rules" | cut -f2- | sort -u)
  if ! grep -q "^strict$tab" <<< "$rules"; then
    finding "main: no status checks are required — README names: ${expected% }"
  else
    grep -qx "strict${tab}true" <<< "$rules" \
      || finding "main: a branch need not be up to date with main before it merges"
    for name in $expected; do
      printf '%s\n' "${actual[@]}" | grep -qxF -- "$name" \
        || finding "main: the required checks lack '$name', which README names"
    done
    for name in "${actual[@]}"; do
      [[ " $expected" == *" $name "* ]] || finding "main: the required check '$name' is not among those README names"
    done
  fi
  # Two rules that make a merge commit impossible whatever the pull request rule allows.
  grep -qx 'required_linear_history' <<< "$rules" \
    && finding "main: a linear history is required, which refuses merge commits"
  while IFS="$tab" read -r _ method; do
    [[ "$method" == MERGE ]] || finding "main: the merge queue merges by '$method', not by merge commit"
  done < <(grep "^merge_queue$tab" <<< "$rules")
  grep -qx 'non_fast_forward' <<< "$rules" || finding "main: force pushes are not blocked"
  grep -qx 'deletion' <<< "$rules" || finding "main: the branch's deletion is not blocked"
fi

# 2. Push protection, null to anyone who is not an admin.
push="$("$GH" api "repos/$repo" --jq '.security_and_analysis.secret_scanning_push_protection.status // "unreadable"' 2>/dev/null)"
case "$push" in
  enabled) ;;
  unreadable | "") finding "secret scanning: push protection could not be read — GitHub shows it to an admin of the repository only" ;;
  *) finding "secret scanning: push protection is $push" ;;
esac

# 3. Private vulnerability reporting.
pvr="$("$GH" api "repos/$repo/private-vulnerability-reporting" --jq '.enabled' 2>/dev/null)"
case "$pvr" in
  true) ;;
  false) finding "private vulnerability reporting is disabled" ;;
  *) finding "private vulnerability reporting could not be read" ;;
esac

if ((${#findings[@]} == 0)); then
  echo "$NAME: $repo matches README → Repository settings — nothing to report."
  exit 0
fi
echo "$NAME: $repo differs from README → Repository settings in ${#findings[@]} point(s):"
printf '  - %s\n' "${findings[@]}"
exit 0
