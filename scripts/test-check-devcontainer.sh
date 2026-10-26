#!/usr/bin/env bash
#
# Self-test of scripts/check-devcontainer.sh. Verifies: ADR-0004
#
# Each case builds a fixture under a temporary directory — a devcontainer.json with comments and
# one pinned Feature and the postStartCommand that enables the git hooks, the lock that resolves
# it, and the editor settings that pin the container-management extension to the host — runs the
# check against it, and asserts the exit code and, for a failing case, the message that names the
# violation. The baseline passes; every other case breaks one thing ADR-0004 decides, or drops a
# git setting from the postStartCommand.
#
# Inputs: scripts/check-devcontainer.sh scripts/lib/
#
# Usage:
#     scripts/test-check-devcontainer.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
CHECK="$ROOT/scripts/check-devcontainer.sh"

# The postStartCommand that enables the git hooks and the merge settings, as the repository's own
# writes it; config writes the value of post_start, none when it is empty.
DRIVER="merge.adr-index.driver '.githooks/merge-adr-index %O %A %B %S %X %Y'"
POST_START='{ "hooks": "git config core.hooksPath .githooks", "merge": "git config merge.ff false && git config pull.ff only", "index": "git config '"$DRIVER"'" }'
post_start="$POST_START"

# config <dir> <feature id> [extra json lines...] — write devcontainer.json with one Feature, the
# postStartCommand post_start holds, and, before the features block, any extra top-level lines
# given.
config() {
  local dir="$1" feature="$2"; shift 2
  {
    echo '{'
    echo '  "name": "fixture",'
    echo '  // A comment: the container has no socket mount — see ADR-0004.'
    echo '  "image": "mcr.microsoft.com/devcontainers/base:debian",'
    [[ -z "$post_start" ]] || echo "  \"postStartCommand\": $post_start,"
    printf '  %s\n' "$@"
    echo '  "features": {'
    echo '    // The one Feature.'
    echo "    \"$feature\": {}"
    echo '  },'
    echo '  "customizations": { "vscode": { "extensions": [] } }'
    echo '}'
  } > "$dir/.devcontainer/devcontainer.json"
}

# lock <dir> <feature ids...> — write devcontainer-lock.json resolving the given Features.
lock() {
  local dir="$1"; shift
  local id
  {
    echo '{'
    echo '  "features": {'
    for id in "$@"; do
      echo "    \"$id\": {"
      echo '      "version": "1.0.0",'
      echo '      "resolved": "example@sha256:0000",'
      echo '      "integrity": "sha256:0000"'
      echo '    },'
    done
    echo '  }'
    echo '}'
  } > "$dir/.devcontainer/devcontainer-lock.json"
}

# settings <dir> <extension kind json> — write .vscode/settings.json with the given block.
settings() {
  local dir="$1" kind="$2"
  {
    echo '{'
    echo '  // Keep the container-management extension on the host side.'
    echo "  \"remote.extensionKind\": $kind"
    echo '}'
  } > "$dir/.vscode/settings.json"
}

# repo <name> — a fixture that passes every check, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/.devcontainer" "$dir/.vscode"
  config "$dir" "ghcr.io/devcontainers/features/github-cli:1"
  lock "$dir" "ghcr.io/devcontainers/features/github-cli:1"
  settings "$dir" '{ "ms-azuretools.vscode-containers": ["ui"] }'
  echo "$dir"
}

# expect <pass|fail> <name> <dir> [needle] — run the check; when a needle is given, its output
# must contain it.
expect() {
  local want="$1" name="$2" dir="$3" needle="${4:-}"
  local out rc
  out="$(bash "$CHECK" "$dir" 2>&1)"; rc=$?
  if [[ "$want" == "pass" && $rc -eq 0 && ( -z "$needle" || "$out" == *"$needle"* ) ]] \
     || [[ "$want" == "fail" && $rc -ne 0 && ( -z "$needle" || "$out" == *"$needle"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected $want${needle:+ with needle \"$needle\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases ---------------------------------------------------------------------------------

d="$(repo baseline)"
expect pass "a container without a socket, pinned and locked, passes" "$d" "1 Features pinned and locked"

d="$(repo socket-mount)"
config "$d" "ghcr.io/devcontainers/features/github-cli:1" \
  '"mounts": ["source=/var/run/docker.sock,target=/var/run/docker.sock,type=bind"],'
expect fail "a host socket mount" "$d" "mounts the host Docker socket"

d="$(repo socket-run-arg)"
config "$d" "ghcr.io/devcontainers/features/github-cli:1" \
  '"runArgs": ["-v", "/var/run/docker.sock:/var/run/docker.sock"],'
expect fail "a host socket passed as a run argument" "$d" "mounts the host Docker socket"

d="$(repo docker-feature)"
config "$d" "ghcr.io/devcontainers/features/docker-outside-of-docker:1"
lock "$d" "ghcr.io/devcontainers/features/docker-outside-of-docker:1"
expect fail "a Feature that mounts the socket" "$d" "would mount the host Docker socket"

d="$(repo engine-inside)"
config "$d" "ghcr.io/devcontainers/features/docker-in-docker:2"
lock "$d" "ghcr.io/devcontainers/features/docker-in-docker:2"
expect pass "an engine inside the container talks to no host engine and is the project's choice" "$d" "1 Features pinned and locked"

d="$(repo unpinned-feature)"
config "$d" "ghcr.io/devcontainers/features/github-cli:latest"
lock "$d" "ghcr.io/devcontainers/features/github-cli:latest"
expect fail "a Feature on a mutable tag" "$d" "major version tag alone"

d="$(repo untagged-feature)"
config "$d" "ghcr.io/devcontainers/features/github-cli"
lock "$d" "ghcr.io/devcontainers/features/github-cli"
expect fail "a Feature without a tag" "$d" "major version tag alone"

d="$(repo lock-missing)"
rm "$d/.devcontainer/devcontainer-lock.json"
expect fail "no lock committed" "$d" "devcontainer-lock.json: not found"

d="$(repo lock-behind)"
config "$d" "ghcr.io/devcontainers/features/node:1"
expect fail "a Feature the lock does not resolve" "$d" "has no entry for Feature 'ghcr.io/devcontainers/features/node:1'"

d="$(repo lock-stale)"
lock "$d" "ghcr.io/devcontainers/features/github-cli:1" "ghcr.io/devcontainers/features/node:1"
expect fail "a lock entry for a Feature no longer added" "$d" "lists Feature 'ghcr.io/devcontainers/features/node:1'"

d="$(repo full-version-feature)"
config "$d" "ghcr.io/devcontainers/features/github-cli:1.2.0"
lock "$d" "ghcr.io/devcontainers/features/github-cli:1.2.0"
expect fail "a Feature on a full version instead of the major tag" "$d" "major version tag alone"

d="$(repo one-line-features)"
printf '{ "image": "x", "features": { "ghcr.io/devcontainers/features/docker-outside-of-docker:1": {}, "ghcr.io/devcontainers/features/node:1": "lts" } }\n' \
  > "$d/.devcontainer/devcontainer.json"
lock "$d" "ghcr.io/devcontainers/features/docker-outside-of-docker:1" "ghcr.io/devcontainers/features/node:1"
expect fail "a features block on one line, with a string-valued Feature, is read like any other" "$d" \
  "Feature 'ghcr.io/devcontainers/features/docker-outside-of-docker:1' would mount"

d="$(repo extension-in-container)"
settings "$d" '{ "ms-azuretools.vscode-containers": ["workspace"] }'
expect fail "the container-management extension not pinned to the host" "$d" "does not pin 'ms-azuretools.vscode-containers'"

d="$(repo extension-under-other-key)"
{
  echo '{'
  echo '  "some.other.setting": { "ms-azuretools.vscode-containers": ["ui"] },'
  echo '  "remote.extensionKind": { "ms-azuretools.vscode-docker": ["ui"] }'
  echo '}'
} > "$d/.vscode/settings.json"
expect fail "the pair under a key other than remote.extensionKind" "$d" "does not pin 'ms-azuretools.vscode-containers'"

d="$(repo no-devcontainer)"
rm -r "$d/.devcontainer"
expect fail "no Dev Container at all" "$d" "devcontainer.json: not found"

d="$(repo post-start-string)"
post_start='"git config core.hooksPath .githooks && git config merge.ff false && git config pull.ff only && git config '"$DRIVER"'"'
config "$d" "ghcr.io/devcontainers/features/github-cli:1"
post_start="$POST_START"
expect pass "the git settings in a postStartCommand string pass" "$d" "git hooks enabled"

d="$(repo post-start-braces)"
# shellcheck disable=SC2016  # the variable is devcontainer.json's, not the shell's
post_start='{ "cd": "cd ${containerWorkspaceFolder}", "list": ["a]b"], "hooks": "git config core.hooksPath .githooks", "merge": "git config merge.ff false && git config pull.ff only", "index": "git config '"$DRIVER"'" }'
config "$d" "ghcr.io/devcontainers/features/github-cli:1"
post_start="$POST_START"
expect pass "a brace or bracket in an entry before the git settings" "$d" "git hooks enabled"

for setting in "core.hooksPath .githooks" "merge.ff false" "pull.ff only" "$DRIVER"; do
  d="$(repo "post-start-without-${setting%% *}")"
  post_start="${POST_START/"git config $setting"/}"
  config "$d" "ghcr.io/devcontainers/features/github-cli:1"
  post_start="$POST_START"
  expect fail "a postStartCommand without 'git config $setting'" "$d" \
    "postStartCommand does not run 'git config $setting'"
done

d="$(repo post-start-driver-args)"
post_start="${POST_START/" %O %A %B %S %X %Y"/}"
config "$d" "ghcr.io/devcontainers/features/github-cli:1"
post_start="$POST_START"
expect fail "the merge driver without the arguments git passes it" "$d" \
  "postStartCommand does not run 'git config $DRIVER'"

d="$(repo post-start-missing)"
post_start=""
config "$d" "ghcr.io/devcontainers/features/github-cli:1"
post_start="$POST_START"
expect fail "no postStartCommand at all" "$d" "postStartCommand does not run 'git config core.hooksPath .githooks'"

d="$(repo post-start-elsewhere)"
post_start='"echo started"'
config "$d" "ghcr.io/devcontainers/features/github-cli:1" \
  '"postCreateCommand": "git config core.hooksPath .githooks && git config merge.ff false && git config pull.ff only",'
post_start="$POST_START"
expect fail "the git settings in another command than postStartCommand" "$d" \
  "postStartCommand does not run 'git config core.hooksPath .githooks'"

# --- summary -------------------------------------------------------------------------------

summary "Dev Container self-test"
