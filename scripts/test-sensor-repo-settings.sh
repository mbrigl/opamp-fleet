#!/usr/bin/env bash
#
# Self-test of scripts/sensor-repo-settings.sh.
#
# The sensor reads GitHub through gh; here a stand-in named by GH answers instead, with the lines
# gh's --jq would print for each endpoint, kept in a fixture directory. Each case writes those
# answers, runs the sensor, and asserts the exit code and a line the reviewer would read. The
# settings README.md lists report nothing; every other case is one setting that differs. What
# the --jq expressions make of GitHub's JSON is not run here, since it needs gh; that is review.
#
# Verifies: ADR-0002
# Inputs: scripts/sensor-repo-settings.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-repo-settings.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-repo-settings.sh"

# The stand-in for gh: 'repo view' prints the repository, 'api <path>' the fixture file of that
# path, and fails where there is none, as gh does on an error.
fake="$work/gh"
cat > "$fake" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  "repo view") [[ -f "$FIX/repo" ]] && cat "$FIX/repo" ;;
  "api --paginate") f="$FIX/${3//\//_}"; [[ -f "$f" ]] && cat "$f" ;;
  "api "*) f="$FIX/${2//\//_}"; [[ -f "$f" ]] && cat "$f" ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$fake"

# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
CHECKS='`docs`, `traceability`, and `shell`'
T=$'\t'
# The lines the sensor's --jq prints for the rules of one ruleset that matches the README.
RULES="pull_request${T}merge${T}true
strict${T}true
check${T}docs
check${T}shell
check${T}traceability
non_fast_forward
deletion"

# repo <name> — a fixture whose README names three required checks and whose answers match it:
# the rules above, push protection enabled, private vulnerability reporting on. Printed as its
# path; a case edits the answers under <path>/fix.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/fix"
  printf '## Repository settings\n<!-- required-checks begin -->\n%s\n<!-- required-checks end -->\n' \
    "$CHECKS" > "$dir/README.md"
  echo "owner/name" > "$dir/fix/repo"
  echo "$RULES" > "$dir/fix/repos_owner_name_rules_branches_main"
  echo "enabled" > "$dir/fix/repos_owner_name"
  echo "true" > "$dir/fix/repos_owner_name_private-vulnerability-reporting"
  echo "$dir"
}

# answer <dir> <path> [text] — the answer of one endpoint; without text the endpoint fails.
answer() {
  local f="$1/fix/${2//\//_}"
  if (($# > 2)); then printf '%s\n' "$3" > "$f"; else rm -f "$f"; fi
}

# expect <name> <dir> <rc> <needle> [absent] — run the sensor; it must exit <rc> and its output
# contain the needle, and not the second needle when one is given.
expect() {
  local name="$1" dir="$2" want="$3" needle="$4" absent="${5:-}" out rc
  out="$(GH="${GH_CMD:-$fake}" FIX="$dir/fix" bash "$SENSOR" "$dir" 2>&1)"; rc=$?
  if ((rc == want)) && [[ "$out" == *"$needle"* ]] && [[ -z "$absent" || "$out" != *"$absent"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

d="$(repo match)"
expect "settings as the README lists them report nothing" "$d" 0 "nothing to report"

d="$(repo no-ruleset)"
answer "$d" repos/owner/name/rules/branches/main ""
expect "no ruleset on main" "$d" 0 "main: no ruleset is in effect"

d="$(repo rules-unreadable)"
answer "$d" repos/owner/name/rules/branches/main
expect "rules that cannot be read are not reported as missing" "$d" 0 \
  "the rules in effect could not be read" "no ruleset is in effect"

d="$(repo no-pull-request)"
answer "$d" repos/owner/name/rules/branches/main "$(grep -v '^pull_request' <<< "$RULES")"
expect "no pull request required" "$d" 0 "main: no pull request is required"

d="$(repo squash)"
answer "$d" repos/owner/name/rules/branches/main "${RULES/merge${T}true/merge,squash${T}true}"
expect "squash allowed beside merge commits" "$d" 0 "the merge methods are 'merge,squash'"

d="$(repo no-codeowners)"
answer "$d" repos/owner/name/rules/branches/main "${RULES/merge${T}true/merge${T}false}"
expect "Code Owners review off" "$d" 0 "the Code Owners review is not required"

d="$(repo no-checks)"
answer "$d" repos/owner/name/rules/branches/main "$(grep -vE '^(strict|check)' <<< "$RULES")"
expect "no required status checks" "$d" 0 "no status checks are required — README names: docs shell traceability"

d="$(repo checks-differ)"
answer "$d" repos/owner/name/rules/branches/main "${RULES/check${T}traceability/check${T}build}"
expect "a check README names is not required" "$d" 0 "the required checks lack 'traceability'"
expect "a required check README does not name" "$d" 0 "the required check 'build' is not among those"

d="$(repo check-with-space)"
answer "$d" repos/owner/name/rules/branches/main "$RULES
check${T}build (ubuntu-latest)"
expect "a check whose name holds a space is one check" "$d" 0 \
  "the required check 'build (ubuntu-latest)' is not among those" "'(ubuntu-latest)'"

d="$(repo not-strict)"
answer "$d" repos/owner/name/rules/branches/main "${RULES/strict${T}true/strict${T}false}"
expect "a branch need not be up to date" "$d" 0 "need not be up to date with main"

# Several rulesets on main: GitHub applies the strictest of their rules, so the merge methods
# all of them allow, the Code Owners review and an up-to-date branch where one requires them,
# and the checks of all of them.
d="$(repo two-rulesets)"
answer "$d" repos/owner/name/rules/branches/main "pull_request${T}merge,squash${T}false
pull_request${T}merge${T}true
strict${T}false
check${T}docs
strict${T}true
check${T}shell
check${T}traceability
non_fast_forward
deletion"
expect "two rulesets whose strictest rules match the README report nothing" "$d" 0 "nothing to report"
d="$(repo two-rulesets-loose)"
answer "$d" repos/owner/name/rules/branches/main "pull_request${T}merge,squash${T}false
pull_request${T}merge,rebase,squash${T}false
$(grep -v '^pull_request' <<< "$RULES")"
expect "two rulesets that both allow squash" "$d" 0 "the merge methods are 'merge,squash'"

d="$(repo force-push)"
answer "$d" repos/owner/name/rules/branches/main "$(grep -v '^non_fast_forward' <<< "$RULES")"
expect "force pushes not blocked" "$d" 0 "force pushes are not blocked"

d="$(repo deletion)"
answer "$d" repos/owner/name/rules/branches/main "$(grep -v '^deletion' <<< "$RULES")"
expect "deletion not blocked" "$d" 0 "the branch's deletion is not blocked"

d="$(repo linear-history)"
answer "$d" repos/owner/name/rules/branches/main "$RULES
required_linear_history"
expect "a linear history refuses merge commits" "$d" 0 "a linear history is required"

d="$(repo merge-queue-squash)"
answer "$d" repos/owner/name/rules/branches/main "$RULES
merge_queue${T}SQUASH"
expect "a merge queue that squashes" "$d" 0 "the merge queue merges by 'SQUASH'"
d="$(repo merge-queue-merge)"
answer "$d" repos/owner/name/rules/branches/main "$RULES
merge_queue${T}MERGE"
expect "a merge queue that merges by merge commit reports nothing" "$d" 0 "nothing to report"

d="$(repo push-protection)"
answer "$d" repos/owner/name "disabled"
expect "push protection disabled" "$d" 0 "push protection is disabled"

d="$(repo not-admin)"
answer "$d" repos/owner/name "unreadable"
expect "push protection hidden from a non-admin" "$d" 0 "push protection could not be read"

d="$(repo no-pvr)"
answer "$d" repos/owner/name/private-vulnerability-reporting "false"
expect "private vulnerability reporting disabled" "$d" 0 "private vulnerability reporting is disabled"
d="$(repo pvr-unreadable)"
answer "$d" repos/owner/name/private-vulnerability-reporting
expect "private vulnerability reporting that cannot be read" "$d" 0 "private vulnerability reporting could not be read"

d="$(repo unresolved)"
rm "$d/fix/repo"
expect "a repository gh cannot resolve exits 2" "$d" 2 "gh cannot resolve the repository"

d="$(repo no-gh)"
GH_CMD="$work/no-such-gh" expect "no gh exits 2" "$d" 2 "is not installed"

expect "a root that does not exist exits 2" "$work/missing" 2 "is not a directory"

summary "Repository settings sensor self-test"
