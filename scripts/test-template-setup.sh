#!/usr/bin/env bash
#
# Self-test of the template setup (TEMPLATE-SETUP.md) against this repository's own files.
#
# The setup promises two things the fixture tests of the single checks cannot see, because they
# hold only on the real scaffold: that check 12 and check 13 of scripts/check-docs.sh name every
# placeholder the setup leaves behind once its file is gone, and that a project which has done
# every step is green. Each case copies the repository's files into a throwaway git repository
# and runs the checks against the copy:
#
#   1. Setup file deleted, nothing else done: check-docs.sh names each placeholder of steps 1,
#      2, 3, and 5 that is still in the tree, and the README's notes that point at the setup
#      file while they are there, and the setup skill left behind. Every file that carries a
#      token of a placeholder (TOKENS) is named in its output, so a placeholder added to a file
#      no row knows of fails here.
#   2. The remaining steps done, as an agent would do them from the steps' wording: no token is
#      left, and check-docs.sh, check-traceability.sh, and check-devcontainer.sh pass.
#   3. The same, on the path 'No Dev Container?' of 'A small project': ADR-0004 superseded and
#      what the container brings removed as the path lists it. check-docs.sh and
#      check-traceability.sh pass. It runs while the path is open, before a project has begun it.
#   4. --close, run by a copy's own script: it refuses on the template's tree, and after step 1,
#      also once the setup file was deleted by hand, removes what close_setup lists and keeps a
#      NOTE block of the project's own.
#
# A project partway through the setup has replaced some placeholders already. Case 1 asks only
# for those still present, and an edit of case 2 that finds nothing to change does nothing, so
# the test holds at every point of the setup. A placeholder the scaffold rewords so that an edit
# no longer applies stays in the copy, and case 2 fails on it.
#
# The setup file goes together with this script, its 'run' line in scripts/check-all.sh, and its
# step in the docs job, which --close removes. Should the script outlive the file, it says there
# is nothing left to test and passes.
#
# With --open it tests nothing and lists what the setup still leaves open, for a session that
# resumes it: what check-docs.sh names on the copy of case 1, its findings about the setup file
# aside, each section of the specification still reading '…', step 6 while checks.yml has no
# job beside the template's and the README names a command, and step 7 while the Building
# blocks of docs/ARCHITECTURE.md name no golden path. Case 1 holds the listing to what
# check-docs.sh names, to the sections that carry a '…', and to the two steps.
#
# With --close it tests nothing and closes the setup in this repository: it removes the setup
# file and what exists only for it, as close_setup lists, this script included. Case 2 makes the
# same edits on its copy, and case 4 runs --close on a copy. It refuses while the README carries
# the template's title, so the template's own tree stays.
#
# No '# Inputs:' line: it reads the whole tree, so scripts/check-all.sh runs it in its
# --changed mode too.
#
# Pure bash + coreutils/sed/awk/grep/tar, plus git to enumerate the files and build the copy. Usage:
#     scripts/test-template-setup.sh [--open|--close]
# Exit code 0 when every case passes, with --open, or once --close has closed the setup; 1 when
# a case fails; 2 on an unknown argument or a --close the template's title refuses.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"

# Once the setup file is gone there is nothing to test or list. --close still runs: a setup file
# deleted by hand leaves the rest of what the close removes behind.
if [[ ! -f "$ROOT/TEMPLATE-SETUP.md" ]]; then
  case "${1:-}" in
    "" | --open)
      echo "Template setup self-test: TEMPLATE-SETUP.md is gone, the setup is done — nothing to test."
      exit 0 ;;
    --close) ;;
    *) echo "test-template-setup: unknown argument '$1' — usage: scripts/test-template-setup.sh [--open|--close]" >&2; exit 2 ;;
  esac
fi

# copy <name> — the repository's files (tracked, and new ones git does not ignore) in a fresh git
# repository, printed as its path. Symlinks stay symlinks.
copy() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "$ROOT" ls-files --cached --others --exclude-standard -z \
    | (cd "$ROOT" && tar --null -T - -cf - 2>/dev/null) | tar -xf - -C "$dir"
  git -C "${dir:?}" init -q -b setup
  echo "$dir"
}

# stage <dir> — let the checks see the copy's files the way they see a working tree.
stage() { fixture "$1"; git -C "${1:?}" add -A; }

# edit <dir> <file> <sed script> — apply one edit of the setup to the copy, where the file exists.
edit() { [[ ! -f "$1/$2" ]] || { sed "$3" "$1/$2" > "$1/$2.tmp" && mv "$1/$2.tmp" "$1/$2"; }; }

# run_check <dir> <script> — run one check against the copy; prints its output and exit code.
run_check() { fixture "$1"; bash "$ROOT/scripts/$2" "$1" 2>&1; }

# expect_pass <name> <dir> <script> — the check must pass on the copy.
expect_pass() {
  local name="$1" dir="$2" script="$3" out rc
  out="$(run_check "$dir" "$script")"; rc=$?
  if ((rc == 0)); then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name ($script exited $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# open_steps — what the setup still leaves open, for a session that resumes it: what
# check-docs.sh names on a copy without the setup file, its findings about that file aside, and
# each section of the specification still reading '…'. One line each.
open_steps() {
  local d
  d="$(copy open)"
  rm "$d/TEMPLATE-SETUP.md"
  stage "$d"
  run_check "$d" check-docs.sh | grep '^  - ' | grep -v 'TEMPLATE-SETUP\.md'
  awk -v q="'" '/^## / { section = substr($0, 4); next }
       /(^|[ *])…$/ && !(section in seen) {
         seen[section]; print "  - docs/SPECIFICATION.md: the section " q section q " still reads " q "…" q
       }' "$ROOT/docs/SPECIFICATION.md"
  open_toolchain "$ROOT"
  open_golden_path "$ROOT"
}

# open_toolchain <dir> — step 6: no job of checks.yml beside the template's own, while the README
# names a command to run.
open_toolchain() {
  local wf="$1/.github/workflows/checks.yml" none
  [[ -f "$wf" ]] || return 0
  none="$(grep -cE '^- \*\*(Build|Test|Lint|Run):\*\* none$' "$1/README.md" 2>/dev/null)"
  ((none == 4)) && return 0
  awk '/^jobs:/ { j = 1; next } j && /^  [a-z][a-z0-9_-]*:$/ { print substr($0, 3, length($0) - 3) }' "$wf" \
    | grep -qvxE 'docs|traceability|devcontainer|sensors|shell' \
    || echo "  - step 6: .github/workflows/checks.yml has no job of the project's toolchain"
}

# open_golden_path <dir> — step 7: no entry of the Building blocks of the overview names a golden
# path. An entry is a list item, so the scaffold's sentence that says where it goes never counts.
open_golden_path() {
  local doc="$1/docs/ARCHITECTURE.md"
  [[ -f "$doc" ]] || return 0
  sed -n '/^## Building blocks/,/^## [^B]/p' "$doc" | grep -qiE '^- .*golden path' \
    || echo "  - step 7: docs/ARCHITECTURE.md names no golden path under Building blocks"
}

# close_setup <dir> — the close of the setup in the tree: the setup file, the README's notes
# that point at it and its row in the Project Layout block, the setup skill and its pointer, and
# this script with its 'run' line and its step in the docs job. The same edits make case 2.
close_setup() {
  local d="$1"
  rm -f "$d/TEMPLATE-SETUP.md"
  rm -rf "$d/.agents/skills/setup-project" "$d/.claude/skills/setup-project"
  # The setup's note only, not another NOTE block of the project: the note and the blank line
  # after it, when its first line says the project still carries its setup.
  if [[ -f "$d/README.md" ]]; then
    awk 'held != "" { if ($0 ~ /still carries its template setup/) { skip = 1 } else { print held }
                      held = "" }
         skip { if ($0 == "") skip = 0; next }
         /^> \[!NOTE\]$/ { held = $0; next }
         { print }
         END { if (held != "") print held }' "$d/README.md" > "$d/README.md.tmp" \
      && mv "$d/README.md.tmp" "$d/README.md"
  fi
  edit "$d" README.md '/^> Setting the project up/,/^$/d'
  edit "$d" README.md '/^TEMPLATE-SETUP\.md /d'
  edit "$d" scripts/check-all.sh '/ scripts\/test-template-setup\.sh$/d'
  # The step and its comment line by line, so an edited step never widens what goes.
  edit "$d" .github/workflows/checks.yml '/^# - Template setup/d
/^      # The setup promises a green project once every step is done/d
/^      # every placeholder left behind; this holds only on the real scaffold/d
/^      - name: Self-test the template setup against the scaffold$/d
/^        run: bash scripts\/test-template-setup\.sh$/d'
  rm -f "$d/scripts/test-template-setup.sh"
}

# close — close the setup in this repository, once step 1 has given it its own name: the
# template's own tree is never closed.
close() {
  if grep -q '^# NUC — an Agentic' "$ROOT/README.md" 2>/dev/null; then
    echo "test-template-setup: the README still carries the template's title — close the setup after step 1." >&2
    exit 2
  fi
  close_setup "$ROOT"
  echo "Template setup closed. Run scripts/check-all.sh: check-docs.sh names what the setup left open."
  exit 0
}

case "${1:-}" in
  "") ;;
  --open) open_steps; exit 0 ;;
  --close) close ;;
  *) echo "test-template-setup: unknown argument '$1' — usage: scripts/test-template-setup.sh [--open|--close]" >&2; exit 2 ;;
esac

# --- case 1: the setup file deleted, nothing else done ----------------------------------------

# TOKENS — what marks a placeholder of the scaffold; SCAFFOLD — the files that may carry one.
# Both are the template's own: a project's files, and a TODO or an owner of its own, are not
# read, so the scan holds while a project is partway through the setup.
TOKENS='<Project Name>|TODO: add a |^- \*\*<[A-Z][a-z]+>\*\*|@owner$|\*\*NUC\*\*|NUC DevContainer|^## Why NUC$|^Describe what this project|^TODO — show'
SCAFFOLD='^(README|CONTRIBUTING|SECURITY|CODE_OF_CONDUCT|AGENTS)\.md$|^LICENSE$|^docs/[A-Z]+\.md$|^\.github/CODEOWNERS$|^\.devcontainer/devcontainer\.json$'

# scaffold_tokens <dir> — every file of the tree that still carries a token, one per line.
scaffold_tokens() {
  local f
  while IFS= read -r f; do
    [[ -f "$1/$f" ]] && grep -qE "$TOKENS" "$1/$f" && echo "$f"
  done < <(git -C "${1:?}" ls-files --cached --others --exclude-standard | grep -E "$SCAFFOLD")
}

# Each row: the file, a pattern that finds the placeholder in it, and the message check-docs.sh
# reports while the placeholder is there. The pattern may hold a '|'; the message holds none.
PLACEHOLDERS=(
  "README.md|^# NUC — |README.md: the title is still the template's"
  "README.md|^\*\*NUC\*\* is named after|README.md: the intro still explains the template's name"
  "README.md|^## Why NUC$|README.md: the Why NUC section still presents the template"
  "README.md|^Describe what this project does|README.md: the Overview is still the scaffold's sentence"
  "README.md|^TODO — show a minimal example|README.md: the Usage section is still TODO"
  "README.md|^\[!\[[^]]*\]\([^)]*hivevm/nuc|README.md: a badge still points at the template's repository"
  ".devcontainer/devcontainer.json|\"name\": \"NUC DevContainer\"|devcontainer.json: the Dev Container still carries the template's name"
  "LICENSE|^Copyright .* Maintainer$|LICENSE: the copyright holder is still the placeholder"
  "SECURITY.md|TODO: add a security contact|SECURITY.md: the security contact is still the placeholder"
  "CODE_OF_CONDUCT.md|TODO: add a contact address|CODE_OF_CONDUCT.md: the reporting contact is still the placeholder"
  ".github/CODEOWNERS|^# /docs/|.github/CODEOWNERS: no active rule"
  ".github/CODEOWNERS|^# TODO: add a code owner|.github/CODEOWNERS: the instruction to add a code owner is still there"
  "docs/SPECIFICATION.md|^# .*<Project Name>|docs/SPECIFICATION.md: the title still carries '<Project Name>'"
  "docs/ARCHITECTURE.md|^# .*<Project Name>|docs/ARCHITECTURE.md: the title still carries '<Project Name>'"
  "docs/GLOSSARY.md|^# .*<Project Name>|docs/GLOSSARY.md: the title still carries '<Project Name>'"
  "docs/CONVENTIONS.md|^# .*<Project Name>|docs/CONVENTIONS.md: the title still carries '<Project Name>'"
  "docs/SPECIFICATION.md|\*\*[GQ]-[0-9]+\*\* — (…|<Quality)|docs/SPECIFICATION.md: a criterion is still the scaffold's placeholder"
  "docs/ARCHITECTURE.md|^- \*\*<Part>\*\*|docs/ARCHITECTURE.md: the scaffold's building block is still listed"
  "docs/GLOSSARY.md|^- \*\*<Term>\*\*|docs/GLOSSARY.md: the scaffold's entry is still listed"
  "docs/CONVENTIONS.md|^- \*\*<Convention>\*\*|docs/CONVENTIONS.md: the scaffold's entry is still listed"
  "README.md|^- \*\*(Build|Test|Lint|Run):\*\* TODO|README.md: a Build, Test, Lint, or Run command is still TODO"
  "docs/adr/0001-agent-governance-model.md|^- \*\*Deciders:\*\* NUC maintainer$|inherited from the template and not decided by this project"
  "README.md|\(TEMPLATE-SETUP\.md\)|README.md: broken relative link -> 'TEMPLATE-SETUP.md'"
  "README.md|^TEMPLATE-SETUP\.md |README.md: Project Layout lists 'TEMPLATE-SETUP.md'"
  ".agents/skills/setup-project/SKILL.md|\(\.\./\.\./\.\./TEMPLATE-SETUP\.md\)|.agents/skills/setup-project/SKILL.md: broken relative link -> '../../../TEMPLATE-SETUP.md'"
)

d="$(copy pending)"
rm "$d/TEMPLATE-SETUP.md"
stage "$d"
out="$(run_check "$d" check-docs.sh)"
for row in "${PLACEHOLDERS[@]}"; do
  file="${row%%|*}"
  rest="${row#*|}"
  message="${rest##*|}"
  pattern="${rest%|*}"
  if [[ ! -f "$ROOT/$file" ]] || ! grep -qE "$pattern" "$ROOT/$file"; then continue; fi
  if [[ "$out" == *"$message"* ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: once the setup file is gone, check-docs.sh does not report: $message" >&2
  fi
done
# Every file that carries a token must be named by check-docs.sh: a placeholder in a file no row
# above knows of is one check 13 does not hold either.
while IFS= read -r file; do
  if [[ "$out" == *"$file: "* ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $file carries a placeholder of the scaffold, and check-docs.sh names no placeholder in it" >&2
  fi
done < <(scaffold_tokens "$ROOT")

# --- case 1, read by --open: what is open, without the findings about the setup file -----------

# pass; fail <message> — count a case as passed, or as failed with the message.
pass() { passed=$((passed + 1)); }
fail() { failed=$((failed + 1)); echo "FAIL: $1" >&2; }

open="$(open_steps)"
# What check-docs.sh names on the copy: each finding about the setup file is a link to it or its
# row in the Project Layout, left out; every other one is listed.
link_re="broken relative link -> '([./]*)TEMPLATE-SETUP\.md"
layout="Project Layout lists 'TEMPLATE-SETUP.md'"
while IFS= read -r l; do
  [[ "$l" == "  - "* ]] || continue
  if [[ "$l" != *"TEMPLATE-SETUP.md"* ]]; then
    if [[ "$open" == *"$l"* ]]; then pass; else fail "--open does not list what check-docs.sh names:$l"; fi
    continue
  fi
  if [[ "$l" =~ $link_re || "$l" == *"$layout"* ]]; then pass
  else fail "a finding about the setup file that is neither a link nor its layout row:$l"; fi
  if [[ "$open" != *"$l"* ]]; then pass; else fail "--open lists a finding about the setup file:$l"; fi
done <<< "$out"
# The sections of the specification with a line still ending in '…', read here line by line:
# each is listed once, and no other.
declare -A unwritten=()
section=""
while IFS= read -r l; do
  if [[ "$l" == "## "* ]]; then section="${l#\#\# }"; continue; fi
  if [[ -n "$section" && ( "$l" == "…" || "$l" == *" …" || "$l" == *"*…" ) ]]; then
    unwritten["$section"]=1
  fi
done < "$ROOT/docs/SPECIFICATION.md"
for section in "${!unwritten[@]}"; do
  if [[ "$open" == *"  - docs/SPECIFICATION.md: the section '$section' still reads '…'"* ]]; then pass
  else fail "--open does not list the section '$section', which still reads '…'"; fi
done
listed="$(grep -c "still reads '…'" <<< "$open")"
if ((listed == ${#unwritten[@]})); then pass
else fail "--open lists $listed sections of the specification, and ${#unwritten[@]} still read '…'"; fi
# Steps 6 and 7: open on the scaffold, closed by a job of the project's own, by commands that
# are all "none", and by a golden path named under Building blocks.
for l in "step 6: .github/workflows/checks.yml has no job" "step 7: docs/ARCHITECTURE.md names no golden path"; do
  if [[ "$open" == *"$l"* ]]; then pass; else fail "--open on the scaffold does not list: $l"; fi
done
d="$(copy steps-6-7)"
awk '/^  shell:$/ { print "  build:"; print "    runs-on: ubuntu-latest" } { print }' \
  "$d/.github/workflows/checks.yml" > "$d/checks.tmp" && mv "$d/checks.tmp" "$d/.github/workflows/checks.yml"
if [[ -z "$(open_toolchain "$d")" ]]; then pass; else fail "--open lists step 6 beside a job of the project's own"; fi
d="$(copy steps-6-none)"
for field in Build Test Lint Run; do edit "$d" README.md "s/^- \*\*$field:\*\* TODO.*/- **$field:** none/"; done
if [[ -z "$(open_toolchain "$d")" ]]; then pass; else fail "--open lists step 6 where the README runs nothing"; fi
edit "$d" docs/ARCHITECTURE.md 's/^- \*\*<Part>\*\*.*/- **Greeter** — the golden path: one request answered end to end./'
if [[ -z "$(open_golden_path "$d")" ]]; then pass; else fail "--open lists step 7 beside a golden path named under Building blocks"; fi
# The argument: --open prints the listing and exits 0, an unknown one exits 2.
if by_arg="$(bash "$ROOT/scripts/test-template-setup.sh" --open)"; then pass; else fail "--open does not exit 0"; fi
if [[ "$by_arg" == "$open" ]]; then pass; else fail "--open prints something else than the listing"; fi
bash "$ROOT/scripts/test-template-setup.sh" --unknown >/dev/null 2>&1
rc=$?
if ((rc == 2)); then pass; else fail "an unknown argument exits $rc, not 2"; fi

# --- case 2: every step done ------------------------------------------------------------------

# setup_steps <dir> — the steps of the setup, done in the copy as an agent would do them.
setup_steps() {
  local d="$1" f doc
  close_setup "$d"
  # Step 1: the project's identity.
  edit "$d" README.md 's/^# NUC — .*/# Fixture/'
  edit "$d" README.md 's/^\*\*NUC\*\* is named after.*/**Fixture** answers questions./'
  edit "$d" README.md '/^## Why NUC$/,/^## Overview$/{/^## Overview$/!d}'
  edit "$d" .devcontainer/devcontainer.json 's/"name": "NUC DevContainer"/"name": "Fixture"/'
  edit "$d" README.md '/^\[!\[.*hivevm\/nuc/d'
  edit "$d" LICENSE 's/^\(Copyright .*\) Maintainer$/\1 Fixture Owner/'
  edit "$d" .github/CODEOWNERS 's|^# \(/docs/[^ ]*\) @owner$|\1 @fixture|'
  edit "$d" .github/CODEOWNERS '/^# TODO: add a code owner/,/^# would enforce nothing/d'
  edit "$d" SECURITY.md 's/<!-- TODO: add a security contact address -->/security@example.invalid/'
  edit "$d" CODE_OF_CONDUCT.md 's/<!-- TODO: add a contact address[^>]*-->/conduct@example.invalid/'
  # Steps 2 and 4: every inherited ADR accepted by a decider of this project.
  for f in "$d"/docs/adr/[0-9][0-9][0-9][0-9]-*.md; do
    edit "$d" "docs/adr/$(basename "$f")" \
      's/^- \*\*Deciders:\*\* NUC maintainer$/- **Deciders:** NUC maintainer, Fixture Owner/'
  done
  # Step 3: the project idea, and the documents of docs/ named and without scaffold entries.
  for doc in SPECIFICATION ARCHITECTURE GLOSSARY CONVENTIONS; do
    edit "$d" "docs/$doc.md" '1s/<Project Name>/Fixture/'
  done
  edit "$d" docs/SPECIFICATION.md \
    's/\*\*\(G-[0-9]*\)\*\* — …$/**\1** — When asked, the system shall answer./'
  edit "$d" docs/SPECIFICATION.md 's/<Quality, e\.g\. [^>]*>/Simplicity/'
  edit "$d" docs/ARCHITECTURE.md '/^- \*\*<Part>\*\*/,/^$/d'
  # shellcheck disable=SC2016  # "$" is sed's last line, not a shell expansion
  edit "$d" docs/GLOSSARY.md '/^- \*\*<Term>\*\*/,$d'
  edit "$d" docs/CONVENTIONS.md '/^- \*\*<Convention>\*\*/d'
  # Step 5: the README's Overview and Usage, and its commands; "none" where there is nothing to
  # run.
  edit "$d" README.md 's/^Describe what this project does.*/Fixture answers questions./'
  edit "$d" README.md 's/^TODO — show a minimal example.*/Run fixture ask./'
  for field in Build Test Lint Run; do
    edit "$d" README.md "s/^- \*\*$field:\*\* TODO.*/- **$field:** none/"
  done
  # The project's first part at the top level, named in the Project Layout as the setup says.
  mkdir -p "$d/src" && echo "echo fixture" > "$d/src/fixture.sh"
  awk '{ print } /^README\.md / && !done { print "src/                  # the fixture'"'"'s code"; done = 1 }' \
    "$d/README.md" > "$d/README.md.tmp" && mv "$d/README.md.tmp" "$d/README.md"
}

d="$(copy complete)"
setup_steps "$d"
stage "$d"
# A token left after every step is a placeholder no step names.
left="$(scaffold_tokens "$d")"
if [[ -z "$left" ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: placeholders no step of the setup replaces, in: $(tr '\n' ' ' <<< "$left")" >&2
fi
expect_pass "every step done passes the documentation checks" "$d" check-docs.sh
expect_pass "every step done passes the traceability check" "$d" check-traceability.sh
if [[ -f "$ROOT/scripts/check-devcontainer.sh" ]]; then
  expect_pass "every step done passes the Dev Container check" "$d" check-devcontainer.sh
fi

# --- case 3: every step done, on the small-project path without a Dev Container ----------------

# Only while the path is still open: a project that has walked it, or begun to, has no container
# left to remove, and case 2 covers what it has.
if [[ -d "$ROOT/.devcontainer" && -f "$ROOT/scripts/check-devcontainer.sh" ]] \
  && grep -q '^- \*\*Status:\*\* 🟢 accepted$' "$ROOT/docs/adr/0004-dev-container-runtime.md" 2>/dev/null; then
  d="$(copy no-container)"
  setup_steps "$d"
  # A check of the project's own in the devcontainer job, as a project adds one for its
  # environment: the path has to take it out of the local runner together with the job.
  printf '#!/usr/bin/env bash\necho "project environment check passed"\n' \
    > "$d/scripts/check-project-environment.sh"
  awk '{ print } /^  devcontainer:$/ { job = 1 }
       job && /run: bash scripts\/check-devcontainer\.sh$/ {
         print "      - name: Check the project'"'"'s environment"
         print "        run: bash scripts/check-project-environment.sh"; job = 0 }' \
    "$d/.github/workflows/checks.yml" > "$d/checks.yml.new" && mv "$d/checks.yml.new" "$d/.github/workflows/checks.yml"
  edit "$d" scripts/check-all.sh \
    '/scripts\/check-devcontainer\.sh$/a run "Project environment" scripts/check-project-environment.sh'
  # The superseding ADR, with the next free number; assembled, so this file cites no ADR that
  # does not exist here.
  n="$(find "$d/docs/adr" -maxdepth 1 -name '[0-9][0-9][0-9][0-9]-*.md' | wc -l)"
  id="$(printf 'ADR-%04d' "$((n + 1))")"
  file="$(printf '%04d-work-on-the-host.md' "$((n + 1))")"
  cat > "$d/docs/adr/$file" <<ADR
# $id: The work runs on the host

- **Status:** 🟢 accepted
- **Date:** 2026-01-01
- **Deciders:** Fixture Owner
- **Applies to:** the development environment
- **Supersedes:** [ADR-0004](0004-dev-container-runtime.md)

## Context

The project is a small tool, and its one maintainer works on the host.

## Decision

We will work on the host, without a Dev Container.

**Out of scope:** nothing beyond the sentence above.

## Alternatives considered

None — cheap to reverse.

## Sources / Prior art

None — cheap to reverse.

## Consequences

None — cheap to reverse.

## Enforcement

**Not mechanically decidable:** where a maintainer works is outside the tree.
ADR
  edit "$d" docs/adr/0004-dev-container-runtime.md \
    "s/^- \*\*Status:\*\* 🟢 accepted\$/- **Status:** ⚪ superseded by $id/"
  # The index: the new row among the binding ones, the old row under the superseded ones.
  row="$(grep -F '| [0004](0004-dev-container-runtime.md) |' "$d/docs/adr/README.md" | sed 's/🟢 accepted/⚪ superseded/')"
  awk -v row="$row" -v new="| [$(printf '%04d' "$((n + 1))")]($file) | The work runs on the host | the development environment | 🟢 accepted |" '
    index($0, "| [0004](0004-dev-container-runtime.md) |") == 1 { next }
    /^### Superseded and rejected/ { print new; print ""; print; superseded = 1; next }
    superseded && $0 == "None yet." { print "| ADR | Title | Applies to | Status |"; print "|-----|-------|------------|--------|"; print row; next }
    { print }' "$d/docs/adr/README.md" > "$d/docs/adr/README.md.new" && mv "$d/docs/adr/README.md.new" "$d/docs/adr/README.md"
  # What the container brings, removed as the small-project path lists it.
  rm -r "$d/.devcontainer" "$d/scripts/check-devcontainer.sh" "$d/scripts/test-check-devcontainer.sh"
  # Every script the devcontainer job runs leaves the local runner with the job, a project's own
  # check included; check 9 holds the job list and the runner together.
  awk '/^  devcontainer:$/ { job = 1; next } job && /^  [a-z][a-z0-9-]*:$/ { job = 0 }
       job && match($0, /run: bash scripts\/[A-Za-z0-9._-]+\.sh/) { print substr($0, RSTART + 10, RLENGTH - 10) }' \
    "$d/.github/workflows/checks.yml" > "$d/job-scripts"
  awk 'NR == FNR { gone[$0] = 1; next }
       match($0, /^[[:space:]]*run[[:space:]]+"[^"]*"[[:space:]]+scripts\/[A-Za-z0-9._-]+\.sh/) {
         script = substr($0, RSTART, RLENGTH); sub(/.*[[:space:]]/, "", script)
         if (script in gone) next
       }
       { print }' "$d/job-scripts" "$d/scripts/check-all.sh" > "$d/check-all.new" \
    && mv "$d/check-all.new" "$d/scripts/check-all.sh"
  # The scripts only the job ran go with it, or check 9 finds them in neither list.
  while IFS= read -r f; do rm -f "${d:?}/$f"; done < "$d/job-scripts"
  rm "$d/job-scripts"
  edit "$d" .github/workflows/checks.yml '/^# - Dev Container: /d'
  awk '/^  devcontainer:$/ { skip = 1; next } skip && /^  [a-z][a-z0-9-]*:$/ { skip = 0 } !skip' \
    "$d/.github/workflows/checks.yml" > "$d/checks.yml.new" && mv "$d/checks.yml.new" "$d/.github/workflows/checks.yml"
  # shellcheck disable=SC2016  # the backticks are Markdown, not a command substitution
  edit "$d" README.md 's/`devcontainer`, //'
  edit "$d" .vscode/settings.json '/^  \/\/ Manage the \*host/,/^  }$/d'
  edit "$d" .gitignore '/^# Dev Container:/,/(ADR-0004)\.$/d'
  edit "$d" README.md '/^## Dev Container$/,/^## Coding Agents$/{/^## Coding Agents$/!d;}'
  edit "$d" README.md '/^\.devcontainer\/ /d'
  edit "$d" README.md '/^This Dev Container preinstalls/,/^$/d'
  edit "$d" SECURITY.md '/^- \*\*The Dev Container has no access/,/^- \*\*Nothing leaves/{/^- \*\*Nothing leaves/!d;}'
  stage "$d"
  expect_pass "the small-project path passes the documentation checks" "$d" check-docs.sh
  expect_pass "the small-project path passes the traceability check" "$d" check-traceability.sh
fi

# --- case 4: --close, run by the copy's own script ---------------------------------------------

# On the template's tree it refuses and removes nothing.
d="$(copy close-refused)"
bash "$d/scripts/test-template-setup.sh" --close >/dev/null 2>&1
rc=$?
if ((rc == 2)) && [[ -f "$d/TEMPLATE-SETUP.md" ]]; then pass
else fail "--close on the template's own tree exits $rc or removes the setup file"; fi
# Once step 1 has named the project it removes what close_setup lists, and nothing names the
# setup file or this script any more.
d="$(copy close)"
edit "$d" README.md 's/^# NUC — .*/# Fixture/'
printf '\n> [!NOTE]\n> **Fixture keeps this note.**\n' >> "$d/README.md"
if bash "$d/scripts/test-template-setup.sh" --close >/dev/null 2>&1; then pass
else fail "--close after step 1 does not exit 0"; fi
for f in TEMPLATE-SETUP.md scripts/test-template-setup.sh .agents/skills/setup-project/SKILL.md; do
  if [[ ! -e "$d/$f" ]]; then pass; else fail "--close leaves $f"; fi
done
if [[ ! -L "$d/.claude/skills/setup-project" ]]; then pass; else fail "--close leaves the skill's pointer"; fi
left="$(grep -l -e 'TEMPLATE-SETUP\.md' -e 'test-template-setup\.sh' \
  "$d/README.md" "$d/scripts/check-all.sh" "$d/.github/workflows/checks.yml")"
if [[ -z "$left" ]]; then pass; else fail "--close leaves a mention of the setup in: $left"; fi
if grep -q 'Fixture keeps this note' "$d/README.md"; then pass
else fail "--close removes a NOTE block of the project's own"; fi
# A setup file deleted by hand: --close still removes the rest.
d="$(copy close-after-hand)"
edit "$d" README.md 's/^# NUC — .*/# Fixture/'
rm "$d/TEMPLATE-SETUP.md"
bash "$d/scripts/test-template-setup.sh" --close >/dev/null 2>&1
if [[ ! -e "$d/scripts/test-template-setup.sh" && ! -e "$d/.agents/skills/setup-project" ]]; then pass
else fail "--close after the setup file was deleted by hand leaves the script or the skill"; fi

# --- summary ---------------------------------------------------------------------------------

summary "Template setup self-test"
