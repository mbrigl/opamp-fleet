# shellcheck shell=bash
# The bash floor of the checks (NUC-checks-are-bash §1), sourced as the first act of every script
# a person or an agent runs: below bash 4.4, such as the bash macOS ships, the script stops with a
# message that names the version it needs, instead of failing on a construct its bash lacks. It is
# written for bash 3.2. BASH_GUARD_VERSION stands in for the version found, for the self-tests.
#
# Usage, from scripts/<name>.sh:
#     # shellcheck source-path=SCRIPTDIR source=lib/bash.sh
#     source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

bash_guard_found="${BASH_GUARD_VERSION:-${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}}"
bash_guard_major="${bash_guard_found%%.*}"
bash_guard_minor="${bash_guard_found#*.}"
bash_guard_minor="${bash_guard_minor%%.*}"
if ((bash_guard_major < 4 || (bash_guard_major == 4 && bash_guard_minor < 4))); then
  echo "${0##*/}: the checks need bash 4.4 or later (NUC-checks-are-bash §1); this is bash $bash_guard_found — run them in the Dev Container, or install a current bash" >&2
  exit 2
fi
unset bash_guard_found bash_guard_major bash_guard_minor
