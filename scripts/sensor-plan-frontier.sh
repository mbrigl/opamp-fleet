#!/usr/bin/env bash
#
# Sensor: the frontier of the plan (ADR-0006).
#
# Reads every ticket under docs/plan/ — a file docs/plan/<spec>/<NN>-<slug>.md — and sorts it
# into one of three lists: claimed, when the branch ticket/<spec>/<NN>-<slug> exists, locally or
# on a remote; blocked, when a path its 'Blocked by' section names is still a file; the frontier
# otherwise, the tickets a session may claim. Each frontier ticket carries the first word of its
# 'Mode' section, so a session dispatching parallel work sees which may run unattended. Paths in
# HTML comments are not read. A blocker is closed once its file is gone, so a path that never
# existed blocks nothing. It reports and never fails on what it counts; it fails when a ticket
# has a name the claim refuses (scripts/lib/ticket.sh), no 'Blocked by' or no 'Mode' section, or
# a blocker that is neither a path under docs/plan/ nor 'None', such as a link, because then it
# cannot place or claim that ticket.
#
# The frontier is read on a base, as the claim takes tickets from it (ADR-0006): without --base
# the plan of the checkout, which is main's where the dispatcher works; with --base, the plan as
# that branch holds it, main or the feat/ branch that carries a plan, whatever is checked out.
#
# Pure bash + git + coreutils/awk. Usage:
#     scripts/sensor-plan-frontier.sh [repository root] [--base <branch>]
# The root defaults to this repository; scripts/test-sensor-plan-frontier.sh passes fixtures.
# Exit code 0 when every ticket was read, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/ticket.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/ticket.sh"

root_arg="" base_name=""
while (($#)); do
  case "$1" in
    --base)
      base_name="${2:-}"
      shift 2 || { echo "Plan frontier sensor: --base needs a branch" >&2; exit 1; }
      ;;
    *) root_arg="$1"; shift ;;
  esac
done
ROOT="$(cd "${root_arg:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
PLAN="docs/plan"

# PLAN_ROOT holds the plan the sensor reads: the checkout, or a copy of the base's.
PLAN_ROOT="$ROOT"
if [[ -n "$base_name" ]]; then
  if ! git -C "$ROOT" rev-parse --verify -q "$base_name^{commit}" >/dev/null; then
    echo "Plan frontier sensor: there is no branch '$base_name' to read the plan on; one only fetched is named by its remote-tracking ref, as origin/$base_name" >&2
    exit 1
  fi
  PLAN_ROOT="$(mktemp -d)"
  trap 'rm -rf "$PLAN_ROOT"' EXIT
  # Each file is read as the base holds it, by ls-tree and cat-file rather than an archive, which
  # .gitattributes could make leave a file out or rewrite it.
  while IFS= read -r -d '' path; do
    mkdir -p "$PLAN_ROOT/${path%/*}"
    git -C "$ROOT" cat-file blob "$base_name:$path" > "$PLAN_ROOT/$path" || {
      echo "Plan frontier sensor: cannot read $path on '$base_name'" >&2
      exit 1
    }
  done < <(git -C "$ROOT" ls-tree -r -z --name-only "$base_name" -- "$PLAN")
fi

# section <file> <heading> — the lines of a '## <heading>' section, HTML comments removed;
# prints nothing and returns 1 when the section is missing.
section() {
  awk -v want="## $2" '
    /^## / { inside = ($0 == want); if (inside) found = 1; next }
    !inside { next }
    {
      line = $0; out = ""
      while (line != "") {
        if (comment) {
          end = index(line, "-->"); if (!end) { line = ""; break }
          line = substr(line, end + 3); comment = 0
        } else {
          start = index(line, "<!--"); if (!start) { out = out line; line = ""; break }
          out = out substr(line, 1, start - 1); line = substr(line, start + 4); comment = 1
        }
      }
      print out
    }
    END { exit !found }' "$1"
}

# branches — every ticket branch, local or remote, as ticket/<spec>/<NN>-<slug>, one per line.
branches() {
  git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || return 0
  git -C "$ROOT" for-each-ref --format='%(refname)' refs/heads/ticket refs/remotes \
    | sed -n -e 's|^refs/heads/\(ticket/.*\)$|\1|p' -e 's|^refs/remotes/[^/]*/\(ticket/.*\)$|\1|p'
}

tickets=()
if [[ -d "$PLAN_ROOT/$PLAN" ]]; then
  while IFS= read -r f; do tickets+=("${f#"$PLAN_ROOT"/}"); done \
    < <(find "$PLAN_ROOT/$PLAN" -mindepth 2 -maxdepth 2 -type f -name '[0-9][0-9]-*.md' | LC_ALL=C sort)
fi
if ((${#tickets[@]} == 0)); then
  echo "Plan frontier sensor: no open tickets under $PLAN/${base_name:+ on $base_name}."
  exit 0
fi

claimed_by="$(branches)"
frontier=() claimed=() blocked=() errors=()
for t in "${tickets[@]}"; do
  if [[ ! "$t" =~ $TICKET_PATH_RE ]]; then
    errors+=("$t: a name the claim refuses — use letters, digits, '.', '_', and '-' only, as docs/plan/<spec>/<NN>-<slug>.md")
    continue
  fi
  branch="$(ticket_branch "$t")"
  if ! blockers="$(section "$PLAN_ROOT/$t" "Blocked by")"; then
    errors+=("$t: no '## Blocked by' section — name the blocking tickets by path, or 'None (can start immediately)'")
    continue
  fi
  # A line that names neither a path under docs/plan/ nor 'None' is a blocker the sensor cannot
  # resolve, such as a link or a bare file name; read as no blocker, it would free the ticket.
  odd="$(grep -vE "^[[:space:]]*([-*][[:space:]]+)?None([^A-Za-z]|$)|$PLAN/[A-Za-z0-9._/-]+\.md|^[[:space:]]*$" <<< "$blockers" | head -n1)"
  if [[ -n "$odd" ]]; then
    errors+=("$t: '## Blocked by' names '${odd#"${odd%%[![:space:]]*}"}', which is no path under $PLAN/ — write each blocker as its path, or 'None (can start immediately)'")
    continue
  fi
  if ! mode="$(section "$PLAN_ROOT/$t" "Mode")"; then
    errors+=("$t: no '## Mode' section — say 'unattended' or 'with the human', and why")
    continue
  fi
  mode="$(awk 'NF { print; exit }' <<< "$mode")"
  case "$mode" in
    unattended*) mode="unattended" ;;
    *) mode="with the human" ;;
  esac
  if grep -qxF "$branch" <<< "$claimed_by"; then
    claimed+=("$t — $branch")
    continue
  fi
  open=()
  while IFS= read -r p; do
    [[ -n "$p" && "$p" != "$t" && -f "$PLAN_ROOT/$p" ]] && open+=("$p")
  done < <(grep -oE "$PLAN/[A-Za-z0-9._/-]+\.md" <<< "$blockers" | LC_ALL=C sort -u)
  if ((${#open[@]})); then
    blocked+=("$t — by ${open[*]}")
  else
    frontier+=("$t — $mode")
  fi
done

if ((${#errors[@]})); then
  echo "Plan frontier sensor: $((${#errors[@]})) ticket(s) unreadable (ADR-0006):" >&2
  printf '  - %s\n' "${errors[@]}" >&2
  exit 1
fi

echo "Plan frontier sensor${base_name:+ on $base_name}: ${#tickets[@]} open, ${#frontier[@]} on the frontier, ${#claimed[@]} claimed, ${#blocked[@]} blocked."
# list <heading> <items...> — one block of the report, left out while it is empty.
list() {
  local heading="$1"
  shift
  (($#)) || return 0
  echo "  $heading"
  printf '  - %s\n' "$@"
}
list "Frontier (claim by creating the branch):" ${frontier[@]+"${frontier[@]}"}
list "Claimed:" ${claimed[@]+"${claimed[@]}"}
list "Blocked:" ${blocked[@]+"${blocked[@]}"}
exit 0
