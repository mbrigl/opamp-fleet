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

# resolve_change <name> <base> [head] — the change a sensor reads, from the base and head it was
# given. Sets CHANGE_HEAD to the head, or HEAD when none is given, and CHANGE_WORKTREE to 1 when
# none is given, for a change that ends in the working tree, else 0. Sets CHANGE_BASE to the base,
# or the merge base of the head with main when none is given; a base of zeros, which a push event
# reports for a new branch, counts as none. With no base and no main it prints that there is
# nothing to compare and returns 1, for the sensor to end as it ends; a base or head that does
# not resolve to a commit exits 2. <name> opens each message.
# shellcheck disable=SC2034 # CHANGE_* are the caller's
resolve_change() {
  local name="$1" ref
  CHANGE_BASE="${2:-}" CHANGE_HEAD="${3:-}" CHANGE_WORKTREE=0
  [[ -n "$CHANGE_HEAD" ]] || { CHANGE_HEAD=HEAD; CHANGE_WORKTREE=1; }
  [[ "$CHANGE_BASE" =~ ^0+$ ]] && CHANGE_BASE=""
  [[ -n "$CHANGE_BASE" ]] || CHANGE_BASE="$(main_merge_base "$CHANGE_HEAD")"
  if [[ -z "$CHANGE_BASE" ]]; then
    echo "$name: nothing to compare — no base given and no main to compare with."
    return 1
  fi
  for ref in "$CHANGE_BASE" "$CHANGE_HEAD"; do
    git rev-parse --verify -q "$ref^{commit}" >/dev/null \
      || { echo "$name: '$ref' does not resolve to a commit." >&2; exit 2; }
  done
}

# change_file <head> <path> — the file as the change has it: in the working tree when <head> is
# empty, else at the commit <head> names. Fails as cat or git show fails, for a file the change
# does not have.
change_file() { if [[ -z "$1" ]]; then cat -- "$2"; else git show "$1:$2"; fi; }
