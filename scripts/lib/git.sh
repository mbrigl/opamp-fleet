# shellcheck shell=bash
# What the checks and sensors ask git the same way. Sourced by scripts/check-all.sh and by the
# sensors that compare a change with main.

# main_merge_base <head> — the merge base of head with origin/main, else with main, in the
# repository of the current directory; prints nothing and returns 1 when neither exists. The
# remote's main comes first: it is what a pull request merges into, and a local main may lag.
main_merge_base() {
  local candidate base
  for candidate in origin/main main; do
    git rev-parse -q --verify "$candidate^{commit}" >/dev/null || continue
    base="$(git merge-base "$candidate" "$1" 2>/dev/null)" && { echo "$base"; return 0; }
  done
  return 1
}

# enter_root <name> <script> [--root <dir>] — change to the repository the sensor reads: the one
# the script lives in, as the checks find theirs, or the one '--root' names, as a self-test does
# with its fixtures. Sets ROOT_TAKEN to the number of arguments the option took, 0 or 2, for the
# caller to shift; exits 2 when the directory does not exist.
# shellcheck disable=SC2034 # ROOT_TAKEN is the caller's
enter_root() {
  local name="$1" script="$2" root
  root="$(cd "$(dirname "$script")/.." && pwd)"
  ROOT_TAKEN=0
  if [[ "${3:-}" == "--root" ]]; then
    root="${4:-}"
    ROOT_TAKEN=2
  fi
  if [[ -z "$root" ]] || ! cd "$root" 2>/dev/null; then
    echo "$name: '$root' is not a directory." >&2
    exit 2
  fi
}
