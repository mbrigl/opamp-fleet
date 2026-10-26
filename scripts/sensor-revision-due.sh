#!/usr/bin/env bash
#
# Sensor: design revision due (ADR-0006).
#
# Reads the 'Last design revision' line of docs/ARCHITECTURE.md — the date of the last revision,
# or 'none yet', and the number of changes after which the next is due — counts the commits that
# touch anything outside docs/, merges excluded, since the commit that set that date, and reports
# the count and whether the revision is due. That commit is the oldest one whose diff of the
# overview adds the line with this date; the history, not the clock, says where the count starts,
# so a commit made later on the same day counts and a change of the number alone does not reset
# it. A date not yet committed counts nothing. When the revision is due, the directories those
# commits touched are listed with the number of changes each, the scope the design-revision
# skill takes; a path that no longer exists is no scope and is left out. It reports and never
# fails on the count; it fails when the line is missing or unreadable, because then it measures
# nothing.
#
# The line:  **Last design revision:** <YYYY-MM-DD or none yet>, due after <N> changes.
#
# Pure bash + git + coreutils. Usage:
#     scripts/sensor-revision-due.sh [repository root]
# The root defaults to this repository; scripts/test-sensor-revision-due.sh passes fixtures.
# Exit code 0 when the line was read, 1 otherwise.

set -uo pipefail

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
OVERVIEW="docs/ARCHITECTURE.md"
LINE_RE='\*\*Last design revision:\*\*[[:space:]]+([0-9]{4}-[0-9]{2}-[0-9]{2}|none yet),[[:space:]]+due after[[:space:]]+([1-9][0-9]*)[[:space:]]+changes\.'

if [[ ! -f "$ROOT/$OVERVIEW" ]]; then
  echo "Design revision sensor: $OVERVIEW not found — the 'Last design revision' line lives there (ADR-0006)." >&2
  exit 1
fi
line="$(grep -m1 -F '**Last design revision:**' "$ROOT/$OVERVIEW")"
if [[ -z "$line" ]]; then
  echo "Design revision sensor: $OVERVIEW has no '**Last design revision:**' line — add one: '**Last design revision:** none yet, due after 20 changes.' (ADR-0006)" >&2
  exit 1
fi
if ! [[ "$line" =~ $LINE_RE ]]; then
  echo "Design revision sensor: the 'Last design revision' line of $OVERVIEW is unreadable — it reads '<YYYY-MM-DD or none yet>, due after <N> changes.' (ADR-0006)" >&2
  exit 1
fi
date="${BASH_REMATCH[1]}"
number="${BASH_REMATCH[2]}"

# range — the commits the count reads: all of HEAD for 'none yet', otherwise those after the
# commit that set the date; empty when there is no history or the date is not committed yet.
range=()
if git -C "$ROOT" rev-parse --verify -q HEAD >/dev/null; then
  if [[ "$date" == "none yet" ]]; then
    range=(HEAD)
  else
    anchor="$(git -C "$ROOT" log --reverse --format=%H -G"Last design revision:.. *$date" HEAD -- "$OVERVIEW" | head -n1)"
    [[ -n "$anchor" ]] && range=("$anchor..HEAD")
  fi
fi
count=0
((${#range[@]})) && count="$(git -C "$ROOT" rev-list --count --no-merges "${range[@]}" -- . ':(exclude)docs')"

from="the first commit"
[[ "$date" == "none yet" ]] || from="$date"
if ((count < number)); then
  echo "Design revision sensor: $count of $number changes outside docs/ since $from — not yet due."
  exit 0
fi
echo "Design revision sensor: $count changes outside docs/ since $from, due after $number — DUE: run the design-revision skill over the directories they touched, then move the line (ADR-0006)."
# One line per top-level entry and commit, so the number beside it counts changes, not files.
while read -r n entry; do
  [[ -e "$ROOT/${entry%/}" ]] && printf '  - %s (%s change%s)\n' "$entry" "$n" "$( ((n == 1)) || echo s)"
done < <(git -C "$ROOT" log --no-merges --name-only --format='%x01' "${range[@]}" -- . ':(exclude)docs' \
  | awk -F/ -v sep=$'\001' '$0 == sep { split("", seen); next } NF { e = (NF > 1 ? $1 "/" : $1); if (!(e in seen)) { seen[e]; print e } }' \
  | sort | uniq -c | sort -rn)
exit 0
