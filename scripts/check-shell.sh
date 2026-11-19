#!/usr/bin/env bash
#
# Shell lint for this repository.
#
# Runs ShellCheck over every shell script of the repository: every '*.sh' file git does not
# ignore, the git hooks under .githooks/, which git requires to be named without an extension,
# and the Linux packages' maintainer scripts under packaging/linux/, which run as root inside
# every `apt` and `dnf` transaction. Their `#!/bin/sh` line makes ShellCheck hold them to POSIX sh.
# The files come from git, as in every other check, so a script under .scratch/ or under a build
# directory the project ignores is no part of the gate. The scripts under scripts/ and the hooks
# are the repository's entire enforcement layer, so they are held to a lint gate themselves.
#
# ShellCheck is the one check dependency beyond bash + coreutils/git. CI does not install it — the
# GitHub-hosted runner image ships it preinstalled, and the Dev Container installs it on creation
# (.devcontainer/devcontainer.json). Elsewhere, when shellcheck is not on PATH, the check skips
# it with a notice (CI remains the gate); in a CI run (CI=true) it is an error, never a silent skip.
#
# Beyond ShellCheck it refuses GNU sed's in-place flag, `sed -i`, which BSD sed on macOS reads as
# taking a suffix: check-all.sh, and with it the pre-push hook, runs on any host. Comment lines are
# not read. The rule reads the options right after `sed`, so it misses a flag after a script
# (`sed -e S -i f`), and it reads text, so it flags the words inside a quoted string or after a
# trailing comment; spell those differently.
#
# It refuses a pipe into grep with -q (or --quiet, --silent) as well. Under pipefail, grep exits
# at its first match, and a producer that writes on after that dies of SIGPIPE: the pipeline fails
# and reads as no match, but only when the scheduler runs grep between two writes, so under load.
# A here-string, a variable, or grep -c reads the input whole. A script whose shebang names sh and
# that sets no pipefail is spared: its pipe has grep's status, and sh has no here-string. The same
# limits as for sed apply: comment lines are not read, the rule reads text, and a pipe on the line
# before is not seen, nor is pipefail set in a file the script sources.
#
# Three rules carry rule keys; their cases are the table scripts/fixtures/check-shell.txt.
# The rules shell/bash-floor and shell/root start the scripts they hold, from an empty directory of
# their own and, for a script the root rule holds, with --root on it. A script is held only by a
# line of its own that asks for it, so a project's script without one is never started. One whose
# floor does not stop it, and that the root rule does not hold, such as a self-test, runs in full.
#
#   shell/mode
#      A tracked script scripts/*.sh, or a skill's .agents/skills/<name>/*.sh, has the mode 100755
#      in the index (NUC-checks-are-bash §9), '[shell/mode:exec]'. Each is run, and AGENTS.md calls
#      some directly, which a file without the executable bit refuses. The rule reads the mode with
#      git ls-files -s, so it holds on a file system that keeps no mode, as on Windows. A file below
#      scripts/ or below a skill's directory is not held: a library under scripts/lib/ is sourced,
#      not run, and stays 100644, and a fixture under scripts/fixtures/ is data. A file git does not
#      track has no mode in the index and is not held until it is added.
#      A symbolic link, mode 120000, is not held: git keeps no executable bit for it. A file in
#      conflict is not held: it has no entry at stage 0, and its mode is open until it is resolved.
#      A finding names the command that sets the bit, the path quoted for the shell.
#   shell/bash-floor
#      A script scripts/*.sh, or a skill's .agents/skills/<name>/*.sh, with a line that sources
#      lib/bash.sh or lib/selftest.sh stops below bash 4.4 with exit 2, its file name, and both
#      versions (NUC-checks-are-bash §1), as BASH_GUARD_VERSION=3.2 makes the floor believe.
#   shell/root
#      A script scripts/*.sh but the self-tests and check-all.sh calls enter_root of
#      scripts/lib/root.sh on a line of its own, and exits 2 with its file name on --verbose and
#      on a --root that is no directory (NUC-checks-are-bash §9). A finding names the call and the
#      Project Layout of CONTRIBUTING.md, where a project reads the rule. A script that declares
#      a positional argument and changes into it passes: the rule runs the script, and reads no
#      cd in its text.
#      An option whose branch in the script reads a value after it, '$2', stands in the usage
#      string as '--<name> <value>', the one place enter_root reads which options take a value,
#      '[shell/root:valued]'. The branch is a case label '--<name>)', quoted or with alternatives
#      after it, or a test '== "--<name>"', and the value is read on its line or the four after it,
#      before ';;', a shift alone, 'set -- "${@:2}"', or a 'fi', 'done', or 'esac'; an option
#      shifted alone is a flag. One read as
#      'shift; x="$1"' is not seen. The usage is read from the script's first line 'usage="', with
#      usage_valued of scripts/lib/root.sh, as enter_root reads it.
#
# Usage:
#     scripts/check-shell.sh [--root <dir>]        (or: bash scripts/check-shell.sh)
# The root defaults to this repository; scripts/test-check-shell.sh passes fixtures. The rules
# shell/bash-floor and shell/root start the scripts of the root they read.
# Exit code 0 when all scripts pass (ShellCheck skipped when unavailable outside CI), 1 otherwise,
# 2 on a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/check-shell.sh [--root <dir>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "${ARGS[@]}"

have_shellcheck=1
if ! command -v shellcheck >/dev/null 2>&1; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: shellcheck not found in a CI run — the runner image is expected to ship it." >&2
    exit 1
  fi
  have_shellcheck=0
fi

# A list git cannot give is an error, not an empty gate.
if ! listed="$(git -C "$ROOT" ls-files --cached --others --exclude-standard -- '*.sh' '.githooks/*' 'packaging/linux/*')"; then
  echo "Shell lint: $ROOT is not a git repository — git lists the files the lint reads." >&2
  exit 1
fi
scripts=()
while IFS= read -r f; do
  [[ -n "$f" && -f "$ROOT/$f" ]] && scripts+=("$ROOT/$f")
done < <(sort -u <<< "$listed")

if ((${#scripts[@]} == 0)); then
  echo "No shell scripts found."
  exit 0
fi

# GNU-only: sed -i, with or without a suffix argument after it, and sed --in-place.
IN_PLACE='(^|[;&|(`[:space:]])sed([[:space:]]+-[a-zA-Z]+)*[[:space:]]+(-[a-zA-Z]*i([[:space:]]|$)|--in-place)'
# A pipe, not '||', into grep with -q among its options, or --quiet or --silent.
QUIET_PIPE='(^|[^|])[|][[:space:]]*grep([[:space:]]+-[a-zA-Z]+)*[[:space:]]+(-[a-zA-Z]*q|--quiet|--silent)'

failed=0
if ((have_shellcheck)); then
  shellcheck "${scripts[@]}" || failed=1
else
  echo "shellcheck is not installed — ShellCheck skipped (CI enforces it); the sed and grep rules run."
  echo "Install it to run locally: https://www.shellcheck.net"
fi

for f in "${scripts[@]}"; do
  while IFS=: read -r line text; do
    echo "${f#"$ROOT"/}:$line: GNU sed's in-place flag does not run on macOS;" \
      "write through a temporary file instead, in place so FILE keeps its mode:" \
      "sed 'SCRIPT' FILE > FILE.tmp && cat FILE.tmp > FILE && rm FILE.tmp"
    echo "    $text"
    failed=1
  done < <(grep -nE "$IN_PLACE" "$f" | grep -vE '^[0-9]+:[[:space:]]*#')
  [[ "$(head -n 1 "$f")" =~ ^#!.*[/[:space:]]sh([[:space:]]|$) ]] \
    && ! grep -qE '^[^#]*pipefail' "$f" && continue
  while IFS=: read -r line text; do
    echo "${f#"$ROOT"/}:$line: a pipe into grep -q fails under pipefail when grep exits at its" \
      "match before the producer has written its last line; read the input whole instead:" \
      "grep -q PATTERN <<< \"\$(PRODUCER)\", or count with grep -c"
    echo "    $text"
    failed=1
  done < <(grep -nE "$QUIET_PIPE" "$f" | grep -vE '^[0-9]+:[[:space:]]*#')
done

# The rule shell/mode reads the mode of each tracked script it holds from the index, which git
# keeps on a file system without one, too. The list above has shown that git reads the root.
while IFS= read -r -d '' entry; do
  meta="${entry%%$'\t'*}" rel="${entry#*$'\t'}"
  mode="${meta%% *}" stage="${meta##* }"
  [[ "$stage" == 0 ]] || continue
  if [[ "$rel" == scripts/* ]]; then
    [[ "${rel#scripts/}" != */* ]] || continue
  else
    [[ "${rel#.agents/skills/*/}" != */* ]] || continue
  fi
  [[ "$mode" == 100755 || "$mode" == 120000 ]] && continue
  printf -v quoted '%q' "$rel"
  echo "$rel: is tracked with mode $mode, not 100755 (NUC-checks-are-bash §9)," \
    "so a direct call is refused —" \
    "a script is run, and git keeps its executable bit: git update-index --chmod=+x -- $quoted" \
    "[shell/mode:exec]"
  failed=1
done < <(git -C "$ROOT" ls-files -s -z -- 'scripts/*.sh' '.agents/skills/*/*.sh')

# The rules shell/bash-floor and shell/root start the scripts they hold from a directory of their
# own, and a script the root rule holds with --root on that empty directory: a script that does
# not stop where it should reads no tree past its enter_root line, not even its own, and
# check-shell.sh started by itself does not start itself again. What a script runs before its
# enter_root line runs as it is.
away="$(mktemp -d)" || { echo "Shell lint: no temporary directory." >&2; exit 1; }
trap 'rm -rf "$away"' EXIT
ROOT_RULE="a script under scripts/ reads its own repository, or another one only as --root <dir>,"
ROOT_RULE+=" through its own line 'enter_root \"\${BASH_SOURCE[0]}\" \"\$usage\" \"\$@\"', then"
ROOT_RULE+=" rest_args, of scripts/lib/root.sh (CONTRIBUTING.md, Project Layout;"
ROOT_RULE+=" NUC-checks-are-bash §9)"
# Each option a script's own code reads with a value, as the header says of the rule shell/root.
# shellcheck disable=SC2016  # the dollar signs are awk's
VALUE_READS='
BEGIN {
  value = "\\$\\{?2([^0-9]|$)"
  alone = "(^|[;{[:space:]])shift[[:space:]]*(;|}|$)|set -- \"?\\$\\{@:2\\}\"?"
  ends = ";;|(^|[;{[:space:]])(fi|done|esac)[[:space:]]*(;|}|$)"
}
/^[[:space:]]*#/ { next }
n > 0 { if ($0 ~ value) { print opt; n = 0 } else if ($0 ~ alone || $0 ~ ends) n = 0; else n-- }
match($0, /--[a-z][a-z0-9-]*"?[[:space:]]*(\|[^)]*)?\)|== "?--[a-z][a-z0-9-]*"?/) {
  opt = substr($0, RSTART, RLENGTH); sub(/^== "?/, "", opt); sub(/[" ]*(\|[^)]*)?\)?$/, "", opt); sub(/"$/, "", opt)
  if (opt == "--root") next
  rest = substr($0, RSTART + RLENGTH)
  if (rest ~ alone) next
  if (rest ~ value) print opt
  else if (rest !~ ends) n = 4
}'
for f in "${scripts[@]}"; do
  rel="${f#"$ROOT"/}" name="${f##*/}" own=0
  [[ "$rel" == scripts/* && "${rel#scripts/}" != */* ]] && own=1
  ((own)) || [[ "$rel" == .agents/skills/*/* && "${rel#.agents/skills/*/}" != */* ]] || continue
  held=""
  [[ "$own" == 1 && "$name" != test-*.sh && "$name" != check-all.sh ]] && held=1
  if grep -qE '^source .*lib/(bash|selftest)\.sh"' "$f"; then
    out="$(cd "$away" && BASH_GUARD_VERSION=3.2 bash "$f" ${held:+--root "$away"} 2>&1)"; rc=$?
    if ((rc != 2)) || [[ "$out" != "$name: the checks need bash 4.4 or later"*"this is bash 3.2"* ]]; then
      echo "$rel: sources the bash floor and does not stop below bash 4.4 with its name (exit $rc)" \
        "— scripts/lib/bash.sh, sourced as its first act, stops it (NUC-checks-are-bash §1)" \
        "[shell/bash-floor:silent]"
      failed=1
    fi
  fi
  [[ -n "$held" ]] || continue
  if ! grep -qE '^enter_root "\$\{BASH_SOURCE\[0\]\}" ' "$f"; then
    echo "$rel: calls no enter_root on a line of its own — $ROOT_RULE [shell/root:unrooted]"
    failed=1
    continue
  fi
  # The options its code reads a value after are those its usage writes '--<name> <value>'.
  usage_valued "$(grep -m1 -E '^usage="' "$f")"; valued="$USAGE_VALUED"
  while IFS= read -r opt; do
    [[ "$valued" == *" $opt "* ]] && continue
    echo "$rel: reads a value after '$opt', which its usage does not write as '$opt <value>' —" \
      "enter_root reads the options that take a value from the usage (scripts/lib/root.sh)" \
      "[shell/root:valued]"
    failed=1
  done < <(awk "$VALUE_READS" "$f" | sort -u)
  out="$(cd "$away" && bash "$f" --verbose --root "$away" 2>&1)"; rc=$?
  if ((rc != 2)) || [[ "$out" != "$name: unknown argument '--verbose'"* ]]; then
    echo "$rel: does not refuse --verbose with exit 2 and its name (exit $rc) — $ROOT_RULE" \
      "[shell/root:verbose]"
    failed=1
    continue
  fi
  out="$(cd "$away" && bash "$f" --root "$away/no-such-root" 2>&1)"; rc=$?
  if ((rc != 2)) || [[ "$out" != "$name: '$away/no-such-root' is not a directory"* ]]; then
    echo "$rel: does not refuse a --root that is no directory with exit 2 (exit $rc) — $ROOT_RULE" \
      "[shell/root:missing]"
    failed=1
  fi
done

if ((failed)); then
  echo
  echo "Shell lint FAILED (see findings above)."
  exit 1
fi

echo "Shell lint passed (${#scripts[@]} scripts)."
exit 0
