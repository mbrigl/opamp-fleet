#!/usr/bin/env bash
#
# Self-test of scripts/check-shell.sh.
#
# Each case builds a throwaway git repository with one shell script that ShellCheck flags, runs
# the lint against it, and asserts the exit code: a tracked script and a new one git does not
# ignore fail, a script under an ignored directory, a git hook in .githooks/, and a maintainer
# script under packaging/linux/ are read as git lists them, and a directory that is no repository
# fails rather than passing empty. GNU sed's in-place flag fails and names the line, while a
# temporary file and a comment pass. A pipe into grep with -q fails and names the line, while a
# here-string, a comment, grep -c, grep -q after '||', and a POSIX sh script without pipefail
# pass; the forms that read the input whole report the match of a producer that writes on after
# it, where the pipe reads none. Where ShellCheck is not on PATH the lint skips ShellCheck, and so
# does this test with the cases that need it. The rules shell/root, shell/bash-floor, and
# shell/mode run the cases of scripts/fixtures/check-shell.txt, which says its format; inline
# beside them, the root.sh a script reads refuses a --root given twice or standing as the value of
# an option, and a script the rules hold is started only when a line asks for it.
#
# Verifies: NUC-checks-are-bash
# Inputs: scripts/check-shell.sh scripts/lib/ scripts/fixtures/check-shell.txt scripts/check-devcontainer.sh scripts/check-docs.sh scripts/sensor-action-refs.sh scripts/sensor-binding-adrs.sh scripts/sensor-review-binding.sh
#
# Usage:
#     scripts/test-check-shell.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
CHECK="$ROOT/scripts/check-shell.sh"

# The cases of ShellCheck's own findings need ShellCheck; the sed rule's cases run without it, as
# the rule does.
have_shellcheck=1
if ! command -v shellcheck >/dev/null 2>&1; then
  echo "Shell lint self-test: shellcheck is not installed — its cases skipped (CI runs them)."
  have_shellcheck=0
fi

# A script ShellCheck flags: an unquoted expansion (SC2086).
FLAGGED=$'#!/usr/bin/env bash\necho $1\n'

# repo <name> — an empty fixture repository with a .gitignore for build/, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  echo "build/" > "$dir/.gitignore"
  echo "$dir"
}

# expect <exit code> <name> <dir> [pattern] — run the lint against the fixture; the output matches
# the glob pattern, when one is given.
expect() {
  local want="$1" name="$2" dir="$3" out rc
  out="$(bash "$CHECK" --root "${dir:?}" 2>&1)"; rc=$?
  if ((rc == want)) && [[ "$out" == *${4:-}* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want${4:+ and output $4}, got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

if ((have_shellcheck)); then
  d="$(repo tracked)"
  printf '%s' "$FLAGGED" > "$d/run.sh"
  git -C "${d:?}" add run.sh
  expect 1 "a tracked script with a finding fails" "$d"

  d="$(repo untracked)"
  printf '%s' "$FLAGGED" > "$d/new.sh"
  expect 1 "a new script git does not ignore fails" "$d"

  d="$(repo ignored)"
  mkdir -p "$d/build"
  printf '%s' "$FLAGGED" > "$d/build/generated.sh"
  expect 0 "a script under an ignored directory is no part of the gate" "$d"

  d="$work/no-repository"
  mkdir -p "$d"
  printf '%s' "$FLAGGED" > "$d/run.sh"
  expect 1 "a directory git does not know fails rather than passing with nothing to lint" "$d"

  d="$(repo hook)"
  mkdir -p "$d/.githooks"
  printf '%s' "$FLAGGED" > "$d/.githooks/pre-commit"
  expect 1 "a git hook without an extension is linted" "$d"

  d="$(repo maintainer-script)"
  mkdir -p "$d/packaging/linux"
  # shellcheck disable=SC2016  # the fixture's $1 is its own
  printf '#!/bin/sh\necho $1\n' > "$d/packaging/linux/postinst"
  git -C "${d:?}" add packaging/linux/postinst
  expect 1 "a Linux package's maintainer script without an extension is linted" "$d"
fi

# GNU sed's in-place flag, spelled without the literal text so this file passes its own lint.
IN_PLACE="$(printf '%s %s' sed -i)"

d="$(repo sed-in-place)"
# shellcheck disable=SC2016  # the fixture's "$1" is its own
printf '#!/usr/bin/env bash\n%s %s "$1"\n' "$IN_PLACE" "'s/a/b/'" > "$d/edit.sh"
git -C "${d:?}" add edit.sh
expect 1 "a script that edits in place with sed fails, named by line with a portable spelling" \
  "$d" "edit.sh:2:*temporary file"

d="$(repo sed-temporary-file)"
# shellcheck disable=SC2016  # the fixture's "$1" is its own
printf '#!/usr/bin/env bash\nsed %s "$1" > "$1.tmp" && mv "$1.tmp" "$1"\n' "'s/a/b/'" > "$d/edit.sh"
git -C "${d:?}" add edit.sh
expect 0 "a script that writes through a temporary file passes" "$d"

d="$(repo sed-comment)"
printf '#!/usr/bin/env bash\n# Never %s here.\necho ok\n' "$IN_PLACE" > "$d/edit.sh"
git -C "${d:?}" add edit.sh
expect 0 "a comment naming the flag passes" "$d"

# A pipe into grep with -q, spelled without the literal text so this file passes its own lint.
PIPE_Q="$(printf '%s %s' '|' 'grep -qxF')"

d="$(repo grep-quiet-pipe)"
printf '#!/usr/bin/env bash\nset -o pipefail\nprintf "%%s\\n" a b %s a && echo found\n' "$PIPE_Q" > "$d/find.sh"
git -C "${d:?}" add find.sh
expect 1 "a script that pipes into grep -q fails, its finding names the line and a whole read" \
  "$d" "find.sh:3:*read the input whole"

d="$(repo grep-quiet-posix-sh)"
printf '#!/bin/sh\nprintf "%%s\\n" a b %s a && echo found\n' "$PIPE_Q" > "$d/find.sh"
git -C "${d:?}" add find.sh
expect 0 "a POSIX sh script without pipefail pipes into grep -q: grep's status is the pipe's" "$d"
for head in '#!/bin/sh\nset -o pipefail' '#!/usr/bin/env bash'; do  # either condition alone fails
  d="$(repo "grep-quiet-${head//[^a-z]/}")"
  printf '%b\nprintf "%%s\\n" a b %s a\n' "$head" "$PIPE_Q" > "$d/find.sh"
  expect 1 "a pipe into grep -q fails under '$head'" "$d" "*read the input whole"
done

# A pipe into grep -m1 and into adr_field_line, which is grep -m1, spelled apart as above.
PIPE_M="$(printf '%s %s' '|' 'grep -m1 x')"
PIPE_ADR="$(printf '%s %s' '|' 'adr_field_line - Status')"
for pipe in "$PIPE_M" "$PIPE_ADR"; do
  d="$(repo "grep-max-${pipe//[^a-z]/}")"
  # shellcheck disable=SC2016  # the fixture's "$(...)" and "$l" are its own
  printf '#!/usr/bin/env bash\nset -o pipefail\nif l="$(cat big %s)"; then echo "$l"; fi\n' "$pipe" > "$d/find.sh"
  git -C "${d:?}" add find.sh
  expect 1 "a pipe that ends early ('$pipe') fails" "$d" "find.sh:3:*read the input whole"
done

d="$(repo grep-quiet-whole)"
# shellcheck disable=SC2016  # the fixture's "$(...)" is its own
{
  printf '#!/usr/bin/env bash\nset -o pipefail\n'
  printf '# Never %s here.\n' "$PIPE_Q"
  printf 'grep -qxF a <<< "$(printf "%%s\\n" a b)" && echo found\n'
  printf '(( $(printf "%%s\\n" a b | grep -cxF a) )) && echo found\n'
  printf 'true || grep -q a /dev/null\n'
} > "$d/find.sh"
git -C "${d:?}" add find.sh
expect 0 "a here-string, a comment, grep -c, and grep -q after || pass" "$d"

# The forms the fixed sites use, under pipefail, against a producer that writes its second line
# after a delay, after grep has matched the first: each reports the match every time. The pipe
# beside them shows the producer does what it is meant to: its last line is longer than a pipe
# holds, so whenever grep runs, it exits with the rest unwritten, the producer dies of SIGPIPE,
# and the pipe reads no match.
# case_line <form> <condition> — one line of the fixture that says whether the form matched.
case_line() { printf 'if %s; then echo "%s match"; else echo "%s none"; fi\n' "$2" "$1" "$1"; }
d="$work/forms"
mkdir -p "$d"
# shellcheck disable=SC2016  # the fixture's "$(...)" is its own
{
  printf '#!/usr/bin/env bash\nset -o pipefail\n'
  printf 'producer() { echo docs; sleep 0.3; echo shell; printf "%%0200000d\\n" 0; }\n'
  case_line here-string 'grep -qxF docs <<< "$(producer)"'
  case_line variable 'text="$(producer)"; grep -qxF docs <<< "$text"'
  case_line count '(( $(producer | grep -cxF docs) ))'
  case_line pipe "producer $PIPE_Q docs"
} > "$d/forms.sh"
out="$(bash "$d/forms.sh" 2>&1)"
for form in here-string variable count; do
  if [[ "$out" == *"$form match"* ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: the $form form misses the match of a producer that writes on after it" >&2
    printf '    %s\n' "$out" >&2
  fi
done
if [[ "$out" == *"pipe none"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: the pipe into grep -q reads the match, so the producer does not test the forms" >&2
  printf '    %s\n' "$out" >&2
fi

# --- the root a script reads: scripts/lib/root.sh, as the rule shell/root holds it ---------

# --root given twice is refused, not read as the last one, and a directory whose name starts with
# '-' is a root, not an option of cd.
out="$(bash "$ROOT/scripts/check-devcontainer.sh" --root "$work/no-such-root" --root "$ROOT" 2>&1)"; rc=$?
if ((rc == 2)) && [[ "$out" == "check-devcontainer.sh: '--root' is given twice"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1)); echo "FAIL: --root given twice is refused (exit $rc): $out" >&2
fi
# A '--root' that stands where an option of the script takes its value is refused with exit 2
# and the option's name, not read as the root: the option would be left without its value and
# the directory as an argument the script may ignore, so that it read its own tree and passed.
for pair in sensor-binding-adrs.sh:--path sensor-review-binding.sh:--hand-off \
  sensor-action-refs.sh:--require; do
  s="${pair%%:*}" opt="${pair#*:}"
  out="$(cd "$work" && bash "$ROOT/scripts/$s" "$opt" --root "$work" 2>&1)"; rc=$?
  if ((rc == 2)) && [[ "$out" == "$s: '$opt' needs a value, not '--root'"* ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1)); echo "FAIL: $s refuses '$opt --root <dir>' (exit $rc): $out" >&2
  fi
done
out="$(cd "$work" && bash "$ROOT/scripts/check-docs.sh" "" --root "$work" 2>&1)"; rc=$?
check "an empty argument before --root is no option that takes a value" \
  test "$rc" = 2 -a "${out%% — *}" = "check-docs.sh: unknown argument ''"
mkdir -p "$work/dash/-root" && git -C "$work/dash/-root" init -q
out="$(cd "$work/dash" && bash "$ROOT/scripts/check-shell.sh" --root -root 2>&1)"; rc=$?
if ((rc == 0)); then
  passed=$((passed + 1))
else
  failed=$((failed + 1)); echo "FAIL: a root whose name starts with '-' is read (exit $rc): $out" >&2
fi

# A script the rules hold is started only when its own line asks for it: one that sources no
# floor and calls no enter_root, or names them only in a comment, is not run.
d="$(repo not-started)"
mkdir -p "$d/scripts"
printf '#!/usr/bin/env bash\ntouch "%s/ran"\n' "$d" > "$d/scripts/own.sh"
bash "$CHECK" --root "$d" > /dev/null 2>&1
check "a project's own script without the floor or enter_root is not started" test ! -e "$d/ran"
# shellcheck disable=SC2016 # the fixture's lines are its own
printf '#!/usr/bin/env bash\n#     source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"\n# enter_root "${BASH_SOURCE[0]}" "$usage" "$@"\ntouch "%s/ran"\n' \
  "$d" > "$d/scripts/own.sh"
bash "$CHECK" --root "$d" > /dev/null 2>&1
check "a project's own script that names them in a comment is not started" test ! -e "$d/ran"

# A script the root rule holds reads no tree when started, not even its own: one without
# rest_args, which would take --verbose and run on, marks its root, and so does one whose floor
# does not stop it.
d="$(repo started-away)"
mkdir -p "$d/scripts" && cp -R "$ROOT/scripts/lib" "$d/scripts/"
# shellcheck disable=SC2016 # the fixture's lines are its own
printf '#!/usr/bin/env bash\nsource "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"\nusage="scripts/own.sh [--root <dir>]"\nenter_root "${BASH_SOURCE[0]}" "$usage" "$@"\ntouch "$ROOT/ran"\n' \
  > "$d/scripts/own.sh"
out="$(bash "$CHECK" --root "$d" 2>&1)"
check "a held script without rest_args, started by the rule, marks no file of its own tree" \
  test ! -e "$d/ran" -a -n "$(grep -F "[shell/root:verbose]" <<< "$out")"
: > "$d/scripts/lib/bash.sh"
# shellcheck disable=SC2016 # the fixture's lines are its own
printf '#!/usr/bin/env bash\nsource "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"\nsource "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"\nusage="scripts/own.sh [--root <dir>]"\nenter_root "${BASH_SOURCE[0]}" "$usage" "$@"\ntouch "$ROOT/ran"\n' \
  > "$d/scripts/own.sh"
out="$(bash "$CHECK" --root "$d" 2>&1)"
check "a held script whose floor does not stop it, started by the rule, marks no file of its own tree" \
  test ! -e "$d/ran" -a -n "$(grep -F "[shell/bash-floor:silent]" <<< "$out")"

# The command a finding of shell/mode names fixes the mode, though the path holds a space.
d="$(repo mode-command)"
mkdir -p "$d/scripts"
printf '#!/usr/bin/env bash\necho build\n' > "$d/scripts/test-my build.sh"
git -C "$d" add -- "scripts/test-my build.sh"
git -C "$d" update-index --chmod=-x -- "scripts/test-my build.sh"
out="$(bash "$CHECK" --root "$d" 2>&1)"
cmd="$(grep -F "[shell/mode:exec]" <<< "$out")"; cmd="${cmd##*: git }"; cmd="git ${cmd% \[*}"
(cd "$d" && eval "$cmd") > /dev/null 2>&1
check "the command a shell/mode finding names sets the mode of a path with a space" \
  test "$(git -C "$d" ls-files -s -- "scripts/test-my build.sh" | cut -c1-6)" = 100755

# --- the rules shell/root, shell/bash-floor, and shell/mode, read from the fixture table -----

TABLE="$ROOT/scripts/fixtures/check-shell.txt"

# kind <path> <kind> — the text of a script of the kind at the path (see the table's header).
# shellcheck disable=SC2016  # the fixture's lines are its own, expanded when it runs
kind() {
  local up="${1%/*}" lib="" head='#!/usr/bin/env bash\nset -uo pipefail\n'
  while [[ "$up" == */* ]]; do lib+="../"; up="${up%/*}"; done
  local src='source "$(dirname "${BASH_SOURCE[0]}")/'"../${lib}scripts/lib"
  local enter='usage="'"$1"' [--root <dir>]"\nenter_root "${BASH_SOURCE[0]}" "$usage" "$@"\n'
  local rest='rest_args "${BASH_SOURCE[0]}" "$usage" 0 "${ARGS[@]}"\n'
  case "$2" in
    rooted) printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n$enter$rest"'echo "$ROOT"\n' ;;
    unrooted) printf '%b' '#!/usr/bin/env bash\necho build\n' ;;
    noargs) printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n$enter"'echo "${ARGS[*]}"\n' ;;
    ownroot) printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n"'enter_root() { ARGS=("${@:3}"); }\n'"$enter$rest" ;;
    floor) printf '%b' "$head$src/bash.sh\"\necho ok\n" ;;
    floorless) printf '%b' "$head"'source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"\necho ok\n' ;;
    cased | caseless)
      local opts='[--path <path>] [--flag]'
      [[ "$2" == cased ]] || opts='[--path] [--flag]'
      printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n"'usage="'"$1"' [--root <dir>] '"$opts"'"\n'
      printf '%b' 'enter_root "${BASH_SOURCE[0]}" "$usage" "$@"\nset -- "${ARGS[@]}"\nwhile (($#)); do\n'
      printf '%b' '  case "$1" in\n    --flag) shift ;;\n    --path)\n      echo "${2:-}"\n      shift 2\n      ;;\n'
      printf '%b' '    *) break ;;\n  esac\ndone\n'"$rest" ;;
    tested | flagged)
      printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n"'usage="'"$1"' [--root <dir>] [--require] [base]"\n'
      printf '%b' 'enter_root "${BASH_SOURCE[0]}" "$usage" "$@"\nset -- "${ARGS[@]}"\n'
      if [[ "$2" == tested ]]; then
        printf '%b' 'if [[ "${1:-}" == "--require" ]]; then\n  echo "${2:-}"\n  shift 2\nfi\n'
      else
        printf '%b' '[[ "${1:-}" == "--require" ]] && { echo required; shift; }\n'
      fi
      printf '%b' 'rest_args "${BASH_SOURCE[0]}" "$usage" 1 "$@"\necho "${2:-}"\n' ;;
    flaggedif)
      printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n"'usage="'"$1"' [--root <dir>] [--sections] [base [head]]"\n'
      printf '%b' 'enter_root "${BASH_SOURCE[0]}" "$usage" "$@"\nset -- "${ARGS[@]}"\nsections=0\n'
      printf '%b' 'if [[ "${1:-}" == "--sections" ]]; then\n  sections=1\n  shift\nfi\n'
      printf '%b' 'base="${1:-HEAD~1}" head="${2:-HEAD}"\nrest_args "${BASH_SOURCE[0]}" "$usage" 2 "$@"\necho "$sections $base $head"\n' ;;
    setflag | fiflag)
      printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n"'usage="'"$1"' [--root <dir>] [--check] [base [head]]"\n'
      printf '%b' 'enter_root "${BASH_SOURCE[0]}" "$usage" "$@"\nset -- "${ARGS[@]}"\ncheck=0\n'
      if [[ "$2" == setflag ]]; then
        printf '%b' 'if [[ "${1:-}" == "--check" ]]; then check=1; set -- "${@:2}"; fi\n'
        printf '%b' 'base="${1:-HEAD~1}" head="${2:-HEAD}"\nrest_args "${BASH_SOURCE[0]}" "$usage" 2 "$@"\necho "$check $base $head"\n'
      else
        printf '%b' 'if [[ "${1:-}" == "--check" ]]; then\n  check=1\nfi\n'
        printf '%b' 'base="${1:-HEAD~1}"; ((check == 0)) || base="${2:-HEAD~1}"\nshift "$check"\n'
        printf '%b' 'rest_args "${BASH_SOURCE[0]}" "$usage" 1 "$@"\necho "$check $base"\n'
      fi ;;
    quoted)
      printf '%b' "$head$src/bash.sh\"\n$src/root.sh\"\n"'usage="'"$1"' [--root <dir>] [--path|-p]"\n'
      printf '%b' 'enter_root "${BASH_SOURCE[0]}" "$usage" "$@"\nset -- "${ARGS[@]}"\nwhile (($#)); do\n'
      printf '%b' '  case "$1" in\n    "--path" | -p)\n      echo "${2:-}"\n      shift 2\n      ;;\n'
      printf '%b' '    *) break ;;\n  esac\ndone\n'"$rest" ;;
    *) echo "fixture table: unknown kind '$2'" >&2; exit 2 ;;
  esac
}

n=0
while IFS= read -r line; do
  [[ "$line" =~ ^(pass|fail) ]] || continue
  n=$((n + 1))
  IFS=$'\x1f' read -r want needle name tree <<< "${line// | /$'\x1f'}"
  d="$(repo "table-$n")"
  cp -R "$ROOT/scripts/lib" "$d/scripts-lib" && mkdir -p "$d/scripts" && mv "$d/scripts-lib" "$d/scripts/lib"
  printf 'disable=SC1091,SC2154\n' > "$d/.shellcheckrc"
  [[ "$tree" != - ]] || tree=""
  while IFS= read -r s; do
    [[ -n "$s" ]] || continue
    read -r op path rest <<< "$s"
    mkdir -p "$(dirname "$d/$path")"
    case "$op" in
      script) kind "$path" "$rest" > "$d/$path" ;;
      write) printf '%s\n' "$rest" > "$d/$path" ;;
      mode)
        git -C "$d" add -- "$path"
        case "$rest" in
          100755) git -C "$d" update-index --chmod=+x -- "$path" ;;
          100644) git -C "$d" update-index --chmod=-x -- "$path" ;;
          *) echo "fixture table: unknown mode '$rest'" >&2; exit 2 ;;
        esac ;;
      link) ln -s "$rest" "$d/$path" && git -C "$d" add -- "$path" ;;
      conflict)
        blob="$(git -C "$d" hash-object -w -- "$path")"
        printf '100644 %s %s\t%s\n' "$blob" 1 "$path" "$blob" 2 "$path" "$blob" 3 "$path" \
          | git -C "$d" update-index --index-info ;;
      *) echo "fixture table: unknown step '$op'" >&2; exit 2 ;;
    esac
  done <<< "${tree//; /$'\n'}"
  out="$(bash "$CHECK" --root "$d" 2>&1)"
  table_case "$name" "$want" "$needle" "$?" "$out"
done < "$TABLE"
((n > 0)) || { echo "FAIL: no case read from $TABLE" >&2; failed=$((failed + 1)); }

summary "Shell lint self-test"
