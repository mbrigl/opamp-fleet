# shellcheck shell=bash
# A ticket's path and the branch that claims it, written once (ADR-0006). Sourced by
# scripts/sensor-plan-frontier.sh, which lists the tickets, and by
# .agents/skills/work-tickets/claim.sh, which claims them, so the frontier offers no ticket the
# claim refuses.

# shellcheck disable=SC2034 # TICKET_PATH_RE is the sourcing script's
# A ticket: docs/plan/<spec>/<NN>-<slug>.md, its names made of letters, digits, '.', '_', and
# '-', since each becomes part of a branch name.
TICKET_PATH_RE='^docs/plan/[A-Za-z0-9._-]+/[0-9][0-9]-[A-Za-z0-9._-]+\.md$'

# ticket_branch <ticket path> — the branch that claims it, ticket/<spec>/<NN>-<slug>.
ticket_branch() {
  local branch="ticket/${1#docs/plan/}"
  echo "${branch%.md}"
}
