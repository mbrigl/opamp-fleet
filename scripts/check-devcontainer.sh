#!/usr/bin/env bash
#
# Dev Container check for this repository (ADR-0004).
#
# ADR-0004 keeps the host container engine out of the Dev Container's reach and pins its Features.
# This check reads the files the decision names and fails on what would break it, and on a
# container that would not enable the repository's git hooks:
#
#   1. Socket: .devcontainer/devcontainer.json mounts no host Docker socket — no 'docker.sock'
#      and no '/var/run/docker' on any line that is not a full-line comment — and adds no
#      Feature whose id names 'docker-outside-of-docker', the one that mounts it. An engine
#      inside the container, such as the Docker-in-Docker Feature, talks to no host engine and
#      is the project's choice (ADR-0004, out of scope).
#   2. Pinning: every Feature is referenced by its major version tag alone ('<ref>:N'), never
#      untagged, ':latest', a full version, or a digest in the hand-edited file.
#   3. Lock: .devcontainer/devcontainer-lock.json exists and its 'features' block names exactly
#      the Features of devcontainer.json — a Feature added without a lock entry builds from a
#      mutable tag, and a stale entry says the lock was not regenerated.
#   4. Host side: .vscode/settings.json pins the container-management extension to the host
#      ('"ms-azuretools.vscode-containers": ["ui"]' inside the 'remote.extensionKind' object),
#      which is how host containers are managed without the socket.
#   5. Git settings: the postStartCommand of devcontainer.json runs 'git config core.hooksPath
#      .githooks', 'git config merge.ff false', 'git config pull.ff only', and 'git config
#      merge.adr-index.driver' with the driver's command as the README writes it. The git hooks of
#      the README and SECURITY.md hold in the container because it enables them on every start,
#      the container's share of the local gate (ADR-0002, ADR-0004). The value is read from its
#      key to the next top-level key, whatever its form: a string, an array, or an object.
#
# Only the default definition is read; a variant under a subdirectory of .devcontainer/ is not.
# devcontainer.json allows comments, so full-line '//' comments are dropped before reading; a
# trailing comment is treated as content. A Feature id is a key directly inside the 'features'
# object, whatever its value — an options object or a version string — found by tracking brace
# depth over the text split at braces and commas, so a block written on one line reads the same
# as one written out. The files are small and hand-written, and no JSON parser is guaranteed on
# PATH.
#
# Pure bash + coreutils/grep/awk. Usage:
#     scripts/check-devcontainer.sh [repository root]
# The root defaults to this repository; scripts/test-check-devcontainer.sh passes fixtures.
# Exit code 0 when the check passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
CONFIG=".devcontainer/devcontainer.json"
LOCK=".devcontainer/devcontainer-lock.json"
SETTINGS=".vscode/settings.json"

# uncommented <file> — the file without its full-line '//' comments.
uncommented() { grep -vE '^[[:space:]]*//' "$1"; }

# feature_ids <file> — the keys directly inside the "features" object, one per line.
feature_ids() {
  uncommented "$1" | awk '
    {
      gsub(/\{/, "{\n"); gsub(/\}/, "\n}"); gsub(/,/, ",\n")
      n = split($0, fragments, "\n")
      for (i = 1; i <= n; i++) {
        line = fragments[i]
        if (line ~ /"features"[[:space:]]*:[[:space:]]*\{/) { base = depth; in_features = 1 }
        else if (in_features && depth == base + 1 && line ~ /^[[:space:]]*"[^"]+"[[:space:]]*:/) {
          key = line
          sub(/^[[:space:]]*"/, "", key); sub(/".*$/, "", key)
          print key
        }
        m = split(line, chars, "")
        for (j = 1; j <= m; j++) {
          if (chars[j] == "{") depth++
          else if (chars[j] == "}") { depth--; if (in_features && depth == base) in_features = 0 }
        }
      }
    }'
}

# top_level_value <file> <key> — the text after the top-level key up to the next top-level key or
# the end, whatever its value holds. A top-level key is a string at depth 1 that opens the file's
# object or follows a comma there; strings are skipped with their escapes, so a brace inside one,
# such as '${containerWorkspaceFolder}', counts for nothing.
top_level_value() {
  uncommented "$1" | awk -v want="$2" '
    { text = text $0 "\n" }
    END {
      n = length(text)
      for (i = 1; i <= n; i++) {
        c = substr(text, i, 1)
        if (in_string) {
          if (c == "\\") { s = s c substr(text, i + 1, 1); i++ }
          else if (c == "\"") {
            in_string = 0
            if (is_key) { capture = (s == want); is_key = 0; continue }
          } else s = s c
          if (capture && !is_key) printf "%s", (c == "\\" ? substr(text, i - 1, 2) : c)
          continue
        }
        if (c == "\"") { in_string = 1; s = ""; is_key = (depth == 1 && expect_key); expect_key = 0 }
        else if (c == "{" || c == "[") { depth++; if (depth == 1) expect_key = 1 }
        else if (c == "}" || c == "]") depth--
        else if (c == "," && depth == 1) expect_key = 1
        if (capture && !is_key) printf "%s", c
      }
    }'
}

features=()
if [[ ! -f "$ROOT/$CONFIG" ]]; then
  add_error "$CONFIG: not found — the Dev Container ADR-0004 governs has nothing to check"
else
  # 1. Socket.
  if uncommented "$ROOT/$CONFIG" | grep -qE 'docker\.sock|/var/run/docker'; then
    add_error "$CONFIG: mounts the host Docker socket — the Dev Container never reaches the host engine (ADR-0004); manage host containers from the host-side extension instead"
  fi

  mapfile -t features < <(feature_ids "$ROOT/$CONFIG")
  for id in "${features[@]}"; do
    if [[ "$id" == *docker-outside-of-docker* ]]; then
      add_error "$CONFIG: Feature '$id' would mount the host Docker socket — it is not added (ADR-0004)"
    fi
    # 2. Pinning.
    [[ "$id" =~ :[0-9]+$ ]] \
      || add_error "$CONFIG: Feature '$id' is not pinned by its major version tag alone ('<ref>:N') — an untagged, mutable, or fully versioned reference is not what the lock resolves (ADR-0004)"
  done

  # 3. Lock.
  if [[ ! -f "$ROOT/$LOCK" ]]; then
    add_error "$LOCK: not found — the lock is committed so that the same commit yields the same Features (ADR-0004)"
  else
    mapfile -t locked < <(feature_ids "$ROOT/$LOCK")
    for id in "${features[@]}"; do
      _contains "$id" "${locked[@]}" \
        || add_error "$LOCK: has no entry for Feature '$id' — regenerate the lock (rebuild the container) so the Feature builds from a resolved digest (ADR-0004)"
    done
    for id in "${locked[@]}"; do
      _contains "$id" "${features[@]}" \
        || add_error "$LOCK: lists Feature '$id', which $CONFIG no longer adds — the lock was not regenerated (ADR-0004)"
    done
  fi

  # 5. Git settings: the text from the postStartCommand key to the next top-level key, its
  # whitespace collapsed.
  post="$(top_level_value "$ROOT/$CONFIG" postStartCommand | tr '\t\n' '  ' | tr -s ' ')"
  for setting in "core.hooksPath .githooks" "merge.ff false" "pull.ff only" \
    "merge.adr-index.driver '.githooks/merge-adr-index %O %A %B %S %X %Y'"; do
    [[ "$post" == *"git config $setting"* ]] \
      || add_error "$CONFIG: postStartCommand does not run 'git config $setting' — the git hooks and the merge settings they rely on are enabled on every start (README, Git hooks)"
  done
fi

# 4. Host side: the pair has to sit inside the 'remote.extensionKind' object, so the text is
# flattened and matched from that key up to the first closing brace.
if [[ ! -f "$ROOT/$SETTINGS" ]]; then
  add_error "$SETTINGS: not found — it pins the container-management extension to the host side (ADR-0004)"
elif ! uncommented "$ROOT/$SETTINGS" | tr -d ' \t\n' \
       | grep -qE '"remote\.extensionKind":\{[^}]*"ms-azuretools\.vscode-containers":\["ui"\]'; then
  add_error "$SETTINGS: does not pin 'ms-azuretools.vscode-containers' to [\"ui\"] under 'remote.extensionKind' — without it the extension runs in the container and asks for the socket (ADR-0004)"
fi

if ((${#errors[@]})); then
  echo "Dev Container check FAILED:" >&2
  printf '  - %s\n' "${errors[@]}" >&2
  exit 1
fi

echo "Dev Container check passed (${#features[@]} Features pinned and locked, no host socket, git hooks enabled)."
exit 0
