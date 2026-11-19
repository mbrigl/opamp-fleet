# shellcheck shell=bash
# The root a script reads and the arguments it refuses, the same way in every script that can run
# on a repository other than its own. Sourced after scripts/lib/bash.sh and the other libraries,
# since enter_root changes the directory and a library is found beside the script.
#
# Usage, from scripts/<name>.sh:
#     # shellcheck source-path=SCRIPTDIR source=lib/root.sh
#     source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
#     usage="scripts/<name>.sh [--root <dir>] [--own-option] [--own-valued <value>]"
#     enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
#     set -- "${ARGS[@]}"
#     ... the script's own options, each shifted off ...
#     rest_args "${BASH_SOURCE[0]}" "$usage" <positional arguments it reads> "$@"
#
# The usage string is the one place that says which of a script's options take a value: each
# written '--<name> <value>' in it, the value in angle brackets, does. The rule shell/root of
# scripts/check-shell.sh fails on an option whose branch in the script reads a value, '$2', that
# the usage does not write so.

# usage_valued <usage> — sets USAGE_VALUED to the options the usage writes with a value,
# '--<name> <value>', but --root, each with a space before and after it. A positional argument
# after a flag is written outside the flag's brackets, '[--check] <file>', or it reads as the
# flag's value.
usage_valued() {
  local rest="$1"
  USAGE_VALUED=" "
  while [[ "$rest" =~ (--[a-z][a-z0-9-]*)\ \<[^\>]+\>(.*) ]]; do
    [[ "${BASH_REMATCH[1]}" == --root ]] || USAGE_VALUED+="${BASH_REMATCH[1]} "
    rest="${BASH_REMATCH[2]}"
  done
}

# enter_root <script> <usage> [args...] — change to the repository the script reads: the one
# it lives in, as the checks find theirs, or the one '--root <dir>' names, wherever it stands but
# as the value of the script's own option, as a self-test does with its fixtures. Sets ROOT to
# that directory, absolute, and ARGS to the arguments other than '--root' and its value. Exits 2
# with the script's file name when '--root' has no value, is given twice, or names no directory:
# a script that read another tree than the one asked for would pass on a tree it never read. A
# script that needs a git repository tests that itself. It also exits 2 on a '--root' that stands
# where an option the usage writes with a value takes it: read as the root, it would leave the
# option without its value and the directory after it as an argument the script might ignore.
# shellcheck disable=SC2034 # ROOT and ARGS are the caller's
enter_root() {
  local script="$1" usage="$2" name="${1##*/}" root="" given=0 valued
  shift 2
  usage_valued "$usage"; valued="$USAGE_VALUED"
  ARGS=()
  while (($#)); do
    if [[ -n "$1" && "$1" != --root && "$valued" == *" $1 "* && "${2:-}" == --root ]]; then
      echo "$name: '$1' needs a value, not '--root' — usage: $usage" >&2
      exit 2
    elif [[ "$1" == --root ]]; then
      (($# >= 2)) || { echo "$name: '--root' needs a directory — usage: $usage" >&2; exit 2; }
      ((given == 0)) || { echo "$name: '--root' is given twice — usage: $usage" >&2; exit 2; }
      root="$2" given=1
      shift 2
    else
      ARGS+=("$1")
      shift
    fi
  done
  ((given)) || root="$(dirname "$script")/.."
  if [[ -z "$root" ]] || ! cd -- "$root" 2>/dev/null; then
    echo "$name: '$root' is not a directory — usage: $usage" >&2
    exit 2
  fi
  ROOT="$(pwd)"
}

# rest_args <script> <usage> <count> [args...] — the arguments left once the script has read its
# own options: exits 2 with the script's file name and the argument on one that starts with '-',
# an option the script does not read, and on more than <count> others, the positional arguments
# it reads, such as a base and a head. A root given the old way, as a first argument, is refused
# so, not read as something else.
rest_args() {
  local name="${1##*/}" usage="$2" count="$3" arg n=0
  shift 3
  for arg in "$@"; do
    if [[ "$arg" == -* ]] || ((++n > count)); then
      echo "$name: unknown argument '$arg' — usage: $usage" >&2
      exit 2
    fi
  done
}
