#!/usr/bin/env bash
#
# Documentation consistency checks for this repository.
#
# Enforces — as CI, not just convention — the documentation rules from AGENTS.md and
# docs/adr/README.md. Each check is named by its rule key, which every finding it reports ends
# in, '[<key>:<reason>]', and which its function carries, 'check_<name>() {  # <key>'. This list
# is the one place that maps a key to its check; ADRs name the checks by it, and the binding ADRs
# sensor routes a change by it. An item may carry the keys its check had before,
# '#   <key> (was: <old>, …)': an ADR that names an old key still names the check, for the rule
# docs/adr-refs and for the sensor. A key moves so: the script first, its item carrying the old
# key, then the ADRs that name it, each commit green. The old key stays, since a derived
# project's ADR may name it. scripts/test-check-docs.sh fails on a key the script prints that
# the list lacks, and on a check that prints a key not its own. One key is no check's: docs/files,
# when the repository lists no text file and nothing else is checked.
#
#   docs/adr-index
#      ADR index integrity: every ADR file, docs/adr/<CAT>-<slug>.md, but the 'NUC-' base,
#      which it lists in no row, is listed in the index
#      table in docs/adr/README.md once, every index row points to an existing file, and the
#      status and the 'Applies to' cell shown in the index match the **Status:** and **Applies
#      to:** lines inside each ADR — the index routes a reader from a change to the ADRs that bind
#      it (AGENTS.md, section 2), so a row that misstates what its ADR governs misroutes them.
#      A superseded or rejected row sits below the 'Superseded and rejected' heading and an
#      accepted or proposed one above it: the first table is what a session reads, and a
#      row in the wrong one is either a dead decision read as binding or a binding one
#      never read (docs/adr/README.md).
#   docs/adr-identifiers (was: docs/adr-numbering)
#      ADR identifier integrity: every identifier has the form of NUC-adrs-are-named-by-category
#      clause 1, its category is fixed or in the category list of the index (clause 2), and the
#      file's heading starts with '# <identifier>:'. No ADR deleted on the first-parent line of
#      HEAD carried the identifier; the message names the commit that deleted it, and a git that
#      cannot read the history fails the check. Every file in docs/adr/ named with letters before
#      a hyphen counts, so a misnamed ADR fails here, and so does a numbered ADR file, 'NNNN-*.md',
#      which no reader takes any more: a derived project maps it once, with the normalize skill of a
#      v0.9 release.
#   docs/links
#      Relative-link integrity: every relative Markdown link in every tracked .md file resolves
#      to a file or directory that exists. Links inside fenced code blocks are illustrations,
#      not claims about files on disk, and are skipped (here and in docs/adr-link-targets);
#      so is a target that still carries the placeholder '<CAT>' of docs/adr/template.md; an
#      optional link title ('[text](file "title")') is not part of the path. Reference-style
#      links ('[text][label]') are not resolved — this repository uses inline links only.
#
#      The rules docs/links, docs/section-refs, docs/adr-refs, and docs/adr-link-targets skip ⚪
#      superseded and 🔴 rejected ADRs (see is_inactive_adr): both describe a repository state
#      that does not exist, so holding their references against the current tree would demand
#      that it still contain what the project decided against.
#   docs/section-refs
#      Section-reference integrity: every section reference matches a numbered '## N.' heading
#      in AGENTS.md: the section sign followed by a number in Markdown, YAML, and shell files
#      and in the git hooks, and the words 'AGENTS.md, section N' in every text file but
#      Markdown. AGENTS.md is the only numbered document in this repository; extend the check if
#      another one appears. The headings there must themselves run 1..N in order: a gap or a
#      repeat is a botched renumbering, and it is the only half of the renumbering hazard a check
#      can see (NUC-documents-are-fixed §3). A clause citation, an ADR's identifier, a space, and
#      the section sign with a number or a list of them, cites that ADR and no section.
#   docs/adr-refs
#      ADR-reference integrity: every identifier of a known category, a clause citation's among
#      them, names an ADR file that exists in docs/adr/ — anticipated follow-ups are described by
#      topic, never by an identifier that does not exist yet (NUC-adrs-are-named-by-category §6).
#      A reference is a word of its own: a one-word slug, such as API-first, or a longer chain is
#      none. An identifier in a Markdown code span is no citation (clause 6) and is not read.
#      Every rule key that the Applies to of an ADR in force names of scripts/check-docs.sh,
#      'the rule `<key>` of `scripts/check-docs.sh`', is an item of this header or an old key
#      one carries: a key that names no check binds the ADR to no check, and no check fails.
#   docs/adr-link-targets
#      ADR link agreement: a Markdown link whose text cites an ADR points at that ADR's own
#      file — the identifier and the file it links to must name the same decision
#      (docs/adr/README.md).
#   docs/adr-header
#      ADR header hygiene: every ADR's '**Status:**' line carries exactly one legend emoji and
#      the word that belongs to it, and no ADR still contains the instruction paragraph from
#      docs/adr/template.md — recognized by a needle read off that template, so rewording it
#      cannot silently disable the check. docs/adr-index compares index and file emoji-to-emoji,
#      so a leftover list of alternatives and a word contradicting its emoji both survive it —
#      and the word is what a reader believes (docs/adr/README.md). Every header field is the
#      list item '- **<Name>:** …' of template.md, since scripts/lib/adr.sh reads only that
#      form: a field in another form would pass here and be skipped by the binding ADRs sensor.
#   docs/adr-supersessions
#      Supersession agreement: once the new ADR is accepted, its '**Supersedes:**' field and the
#      '⚪ superseded by <identifier>' status line of every old one it names — one, or several it
#      consolidates — name each other, in either form of identifier. While it is
#      proposed or rejected, no old one names it as its successor: only a human changes a status,
#      and the human flips the old lines when accepting the new one (docs/adr/README.md). A new
#      ADR superseded in its turn counts as accepted, so a chain stays whole. docs/adr-index cannot
#      see a missing half: it only mirrors the index against whatever status the file carries.
#      Every name of a Supersedes field counts, on whichever line it sits. No project ADR
#      supersedes a 'NUC-' ADR, and each clause an 'Extends:' or 'Narrows:' field names, as
#      'NUC-<slug> §n', is a numbered clause of that ADR's Decision (NUC-base-is-fixed §1).
#   docs/check-lists
#      Check-list agreement: the jobs of .github/workflows/checks.yml and the required status
#      checks named between the 'required-checks' markers in CONTRIBUTING.md list the same jobs, and
#      the workflows under .github/workflows/ and scripts/check-all.sh run the same scripts. A
#      check that runs in only one of them is a gate somebody believes in and does not have
#      (AGENTS.md, section 5). A job that runs no script under scripts/ fails too: its commands
#      run in CI and never in the local gate; a workflow check-all.sh names on a '# Not a gate:'
#      line, one that publishes rather than checks, is exempt, while the scripts it runs still
#      count. So does a script under scripts/ that neither list runs, unless check-all.sh names
#      it on an '# On instruction:' line. A job with a matrix reports one check run per entry, so
#      CONTRIBUTING.md names it by those, '<job> (<values>)', and never by its bare name.
#   docs/project-layout
#      Project Layout integrity: every path listed in the 'Project Layout' block of CONTRIBUTING.md
#      exists, a trailing '/' meaning a directory. The block is written by hand and nothing else
#      tells a reader that one of its lines has gone stale. A path with a '*' matches at least one
#      file, and docs/plan/ may be absent, since open work lives there only while there is some
#      (NUC-large-work-is-planned §2). The reverse holds for the top level only: every tracked
#      top-level file and directory is named, by itself or by a path under it. Below it the block
#      is a tour of the parts a newcomer needs, not a file listing.
#   docs/skill-pointers
#      Skills an agent reads: every skill under .agents/skills/ (a directory with a SKILL.md) has
#      a symlink of its name under .claude/skills/ that resolves to it, and every entry there is
#      such a symlink (NUC-procedures-are-skills §6); Claude Code reads only that directory, and a
#      copy drifts. Each SKILL.md opens with '---', closes with '---', and between them has a
#      'name' equal to its directory and a 'description' (§7); an unindented line there starts a
#      key or a '- ' item, as a wrap that hides the skill does not, and indented lines pass
#      unread. Each message says the fix. A repository with neither directory has nothing to check.
#   docs/base-blobs
#      Base held by its blob list (NUC-base-is-fixed §2): every 'NUC-' file in
#      docs/adr/ and the rule file AGENTS.md has the git blob identifier its line in
#      docs/adr/NUC.blobs names, and every line names a file that exists. The template rewrites
#      the list in the commit that changes one, and a derived project never changes one. Without
#      a list the check passes when no 'NUC-' file exists, or when the Template release line of
#      README.md names the release a fork forked, 'vX.Y.Z, forked'.
#   docs/template-identity
#      Setup done: once TEMPLATE-SETUP.md is gone, each required role of ADR_ROLES in
#      scripts/lib/adr.sh has exactly one 🟢 accepted ADR whose 'Role:' field names it
#      (NUC-base-is-fixed §3), and none of the template's placeholders is left.
#      Step 1 of the setup: the project name NUC in the README title, its
#      intro, its Why NUC section, and the Dev Container name, the template's repository in the
#      README badges, the copyright holder in LICENSE, the security contact in SECURITY.md, the
#      reporting contact in CODE_OF_CONDUCT.md, and a CODEOWNERS with no active rule or with its
#      TODO. Step 3: the '<Project Name>' title of the specification, the overview, the glossary, and the
#      conventions, a criterion of the specification still reading '…' or '<Quality', and the
#      scaffold's '<Part>', '<Term>', and '<Convention>' entries. Step 5: a 'TODO' as the Build,
#      Test, Lint, or Run command of the README, its Overview sentence, and its Usage TODO. The
#      close: the issue template .github/ISSUE_TEMPLATE/template-finding.md, whose findings go to
#      the template's repository (NUC-template-learns-from-projects §6). A placeholder that
#      survives the setup describes the template, not the project. A file that does not exist has
#      nothing to check.
#   docs/opens-no-issue
#      No file of the template opens an issue on its own: only a human sends a finding
#      (NUC-template-learns-from-projects §4). While TEMPLATE-SETUP.md exists, the repository is
#      the template, and the rule reads every text file under scripts/, .githooks/, .github/, and
#      .agents/ but the fixture tables under scripts/fixtures/, which nothing runs. After the
#      setup the files are the project's, and the rule reads none. A line whose first non-space
#      character is '#' is a comment and is not read; a trailing '# comment' on a command line is
#      read with it. A line that ends in a backslash goes on in the next one. A line fails in five
#      plain forms, each its reason:
#        cli      'gh issue create', or its alias 'gh issue new';
#        api      'gh api' on a path that ends in 'repos/…/issues', with '-X POST', '--method
#                 POST', or a field, '-f', '-F', '--field', '--raw-field', or '--input', and with
#                 no '-X GET', '--method GET', or '--request GET';
#        graphql  the mutation 'createIssue(', anywhere on the line;
#        http     curl or wget on that path with a POST or with data: '-X POST', '--request
#                 POST', '-d', '--data…', '--json', '--post-data', or '--post-file';
#        script   the call 'issues.create(', as actions/github-script makes it, or a step
#                 'uses:' an action whose name holds 'create-issue' or 'create-an-issue'.
#      Prose in Markdown is read too: a skill's sentence is an instruction to an agent, which is
#      what clause 4 guards. A skill names the human's act, such as 'the human sends the finding
#      as an issue', not the command. Reading issues passes, as does a link to an issues page.
#      The rule reads text: it misses a command built from variables, a shell alias, a list of
#      arguments such as Python's '["gh", "issue", "create"]', a client in another language, and
#      a mutation whose name and parenthesis stand on two lines. The review holds the rest, as the
#      Enforcement of that ADR says.
#   docs/template-release
#      Template release named: README.md carries a '**Template release:**' line naming a
#      'vX.Y.Z' tag or 'unreleased' (NUC-releases-are-semver-tags §4). Tags do not travel with
#      GitHub's template mechanism, so the line is the one thing that tells a derived project
#      which release of the template it carries; a release moves it, and a project that takes a
#      release up moves it (§5).
#      A change that moves the version on the line against its merge base with origin/main, else
#      main, cuts a release or takes one up, and then each 🟡 proposed 'NUC-' ADR fails (clause 8): a
#      derived project can neither accept nor reject one. A base without the line counts as none.
#      Without a merge base the line is read alone.
#   docs/prose-width
#      Prose width: a line of Markdown prose is at most 100 columns, so a diff shows the sentence
#      that changed. Fenced code, front matter, table rows, headings, and ADR header fields are
#      exempt, and so is a line that cannot break, such as a link standing on a line of its own.
#      Superseded and rejected ADRs are not read, nor is a file .gitattributes marks
#      'linguist-generated', such as a changelog a tool writes. Indented code blocks are read as
#      prose.
#   docs/named-sections
#      Named-section integrity: in every text file, a section of README.md or CONTRIBUTING.md
#      named in prose names a heading of that document. The form read is the file name, an
#      arrow, and the heading in double quotes, as in 'README.md → "Usage"' or 'README → "Usage"'.
#      The document is the one beside the file, else the one in the root; a file name after
#      '../' is a path. A reference wrapped over two lines is joined, the comment marker of the
#      second line dropped. A heading in angle brackets is a placeholder and names nothing. A
#      heading in a fenced code block is not a heading. Not read: an unquoted or emphasized
#      heading; a quoted word before 'in README.md', which is as often a value as a heading; a
#      file name after a path other than '../', such as 'docs/README.md'; and a section named
#      by what it says instead of by its heading, such as 'the manual checklist in README.md'.
#      Superseded and rejected ADRs are not read. A check of this project's own.
#   docs/clause-citations
#      Clause citations: a citation of an ADR's clause names a numbered clause of that ADR's
#      Decision, of an ADR in force: a normalization re-cites them by its clause table, and a
#      citation that names no clause, or a clause of a dead ADR, is stale. The form read is
#      NUC-adrs-are-named-by-category §3's: an identifier, a space, and the section sign with the
#      number, and a list of them after the identifier, '§N, §M', '§N and §M', '§N, §M, and §K' or
#      a range '§N–§M', inside a Markdown link's text where it is one. A citation wrapped over two
#      lines is joined, the comment marker of the second line and a backslash ending the first
#      dropped. Each number, both ends of a range included, lies between 1 and the count of the
#      Decision's numbered items ('N. ' at the start of a line, outside fenced code, where a '## '
#      line ends no section either); a Decision that numbers none is not held. A citation of a
#      superseded or rejected ADR fails outside docs/adr/, whose ADRs keep the past they were
#      written against, and outside CHANGELOG.md, which records what held when each entry was
#      written. So does a citation of a project ADR by
#      a place instead of a clause, which a normalization's clause table cannot follow: 'point',
#      'paragraph', 'line', 'rule', 'item', 'bullet', 'step', 'section', the older word 'clause',
#      or the section sign after a link, before a number and after an identifier, and
#      'clause N of' an identifier. The base's own text is the template's, and only its clause
#      numbers are read. An identifier that names no ADR is the rule docs/adr-refs'. Superseded and rejected
#      ADRs are not read, nor is the template. A check of this project's own.
#
# The rules docs/adr-refs and docs/adr-link-targets read every text file of the repository,
# not a list of documentation extensions: docs/adr/README.md states that code may reference an
# ADR, so a verifier restricted to documentation file types would leave the references most
# likely to go stale — those in source comments, which no reviewer reads alongside the ADR
# index — unchecked. docs/section-refs reads the section sign only in documentation file
# types on purpose: an ADR's identifier and the words 'AGENTS.md, section N' mean one thing
# wherever they appear, but '§' in source code is an ordinary character (see is_doc_file),
# and flagging it there would make a project's own string literals fail this repository's CI.
#
# Pure bash + coreutils/grep/sed/awk/find, plus git to enumerate the repository's files — all present
# in the Dev Container base image, so running it adds no toolchain and no dependency that would
# require an ADR.
#
# Usage:
#     scripts/check-docs.sh [--root <dir>]        (or: bash scripts/check-docs.sh)
# The root defaults to this repository; scripts/test-check-docs.sh passes fixture repositories.
# Exit code 0 when all checks pass, 1 otherwise, and 2 on an argument it does not read
# or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"
# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/check-docs.sh [--root <dir>]"
# The keys of the items of this header and the old keys they carry, read before enter_root
# changes the directory a relative path of this script starts from.
RULE_KEYS=" $(sed -nE '/^set /q; s/^#   ([a-z]+\/[a-z0-9-]+)( \(was: ([^)]+)\))?$/\1 \3/p' \
  "${BASH_SOURCE[0]}" | tr ',\n' '  ') "
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "${ARGS[@]}"
ADR_DIR="$ROOT/docs/adr"
ADR_INDEX="$ADR_DIR/README.md"


# The helpers below set a variable rather than print: a check calls them once per ADR or per
# index row, and a subshell each time made the script's cost grow with the record.

# status_emoji <text> — sets EMOJI to the first legend status emoji (🟢 🟡 🔴 ⚪) in the text, empty
# when there is none, and EMOJI_COUNT to how many the text carries.
EMOJI="" EMOJI_COUNT=0
status_emoji() {
  local text="$1" e before at=-1 rest
  EMOJI="" EMOJI_COUNT=0
  for e in 🟢 🟡 🔴 ⚪; do
    before="${text%%"$e"*}"
    [[ "$before" == "$text" ]] && continue
    if ((at < 0 || ${#before} < at)); then at=${#before}; EMOJI="$e"; fi
    rest="${text//"$e"/}"
    EMOJI_COUNT=$((EMOJI_COUNT + (${#text} - ${#rest}) / ${#e}))
  done
}

# trim <text> — sets TRIMMED to the text without leading and trailing whitespace.
TRIMMED=""
trim() {
  TRIMMED="${1#"${1%%[![:space:]]*}"}"
  TRIMMED="${TRIMMED%"${TRIMMED##*[![:space:]]}"}"
}

# The header of every ADR, read by one awk pass over the record instead of a grep per field, per
# ADR, and per check. Keyed by the ADR's path: the first line of each header field anywhere in the
# file, as adr_field_line reads it (ADR_STATUS, ADR_APPLIES); the Supersedes, Extends, and Narrows
# fields with their continuation lines, since a header field wraps (ADR_SUPERSEDES, and ADR_REL
# keyed '<field> <path>' with the Role field); the first '# ' heading (ADR_HEADING); every header
# field written in another form, as adr_stray_fields reports it (ADR_STRAY); and whether the file
# still carries the template's instruction paragraph (ADR_NEEDLE). ADR_LIST is every ADR file in
# the order adr_files gives, ADR_BY_ID the file each identifier names.
ADR_LIST=()
declare -A ADR_KNOWN=() ADR_BY_ID=() ADR_STATUS=() ADR_APPLIES=() ADR_SUPERSEDES=() ADR_HEADING=() \
  ADR_STRAY=() ADR_NEEDLE=() ADR_REL=()
read_adr_headers() {
  local file field value
  adr_categories "$ADR_INDEX"
  [[ -d "$ADR_DIR" ]] || return 0
  mapfile -t ADR_LIST < <(adr_files "$ADR_DIR")
  ((${#ADR_LIST[@]})) || return 0
  for file in "${ADR_LIST[@]}"; do
    ADR_KNOWN["$file"]=1
    adr_key_of "$file"
    [[ -v ADR_BY_ID["$ADR_ID"] ]] || ADR_BY_ID["$ADR_ID"]="$file"
  done
  while IFS=$'\x1f' read -r file field value; do
    case "$field" in
      Status) ADR_STATUS["$file"]="$value" ;;
      "Applies to") ADR_APPLIES["$file"]="$value" ;;
      Supersedes) ADR_SUPERSEDES["$file"]+="$value"$'\n' ;;
      Extends | Narrows | Role) ADR_REL["$field $file"]+="$value"$'\n' ;;
      heading) ADR_HEADING["$file"]="$value" ;;
      stray) ADR_STRAY["$file"]+="$value"$'\n' ;;
      needle) ADR_NEEDLE["$file"]=1 ;;
    esac
  done < <(awk -v mark="$ADR_FIELD_MARK_RE" -v item="$ADR_ITEM_RE" -v field="$ADR_FIELD_ITEM_RE" -v needle="$(instruction_needle)" '
    function out(k, v) { printf "%s\037%s\037%s\n", FILENAME, k, v }
    FNR == 1 { head = 1; cont = ""; split("", seen) }
    {
      if (head && /^## /) head = 0
      if (head && $0 ~ mark && $0 !~ item) out("stray", FNR ":" $0)
      if (needle != "" && !("needle" in seen) && index($0, needle)) { seen["needle"]; out("needle", 1) }
      if (!("heading" in seen) && /^# /) { seen["heading"]; out("heading", $0) }
      if (cont != "" && /^[[:space:]]+[^[:space:]]/) { out(cont, $0); next }
      cont = ""
      if (match($0, field)) {
        name = substr($0, 5, RLENGTH - 7)
        if (!(name in seen)) {
          seen[name]; out(name, $0)
          if (name == "Supersedes" || name == "Extends" || name == "Narrows") cont = name
        }
      }
    }
  ' "${ADR_LIST[@]}")
}

# adr_header <file> — sets HEADER_STATUS and HEADER_APPLIES to the Status and Applies to lines of
# the file: from the cache for an ADR of the record, read from the file for one outside it, such as
# a file an index row links that is no ADR.
HEADER_STATUS="" HEADER_APPLIES=""
adr_header() {
  if [[ -v ADR_KNOWN["$1"] ]]; then
    HEADER_STATUS="${ADR_STATUS["$1"]-}" HEADER_APPLIES="${ADR_APPLIES["$1"]-}"
  else
    HEADER_STATUS="$(adr_field_line "$1" Status)"
    HEADER_APPLIES="$(adr_field_line "$1" 'Applies to')"
  fi
}

# applies_to_of — sets TRIMMED to the content of the '**Applies to:**' line adr_header read,
# trimmed; empty when the line is missing or blank.
applies_to_of() { adr_field_value "$HEADER_APPLIES" 'Applies to'; trim "$ADR_VALUE"; }

# TEXT_FILES — every text file the repository consists of, absolute paths, sorted. Collected once
# by collect_text_files below and then iterated by each check: building the list reads every
# file, so rebuilding it inside every check would multiply that cost by the number of checks, and
# a repository of a few thousand files makes that difference visible in CI.
#
# The list comes from git: tracked files plus new, not-yet-added ones, minus everything
# .gitignore excludes. That keeps generated trees (node_modules/, target/, dist/) out without
# this script having to guess the directory names of a toolchain this repository does not yet know.
# Binary files are dropped by grep -I, which never matches inside one.
# Outside a work tree (an exported tarball) git cannot answer, so fall back to a plain walk.
TEXT_FILES=()
collect_text_files() {
  local f paths=()
  while IFS= read -r f; do
    [[ -f "$f" ]] && paths+=("$f")
  done < <(
    if git -C "$ROOT" rev-parse --is-inside-work-tree > /dev/null 2>&1; then
      git -C "$ROOT" ls-files --cached --others --exclude-standard \
        | sed "s|^|$ROOT/|"
    else
      find "$ROOT" -type f -not -path '*/.git/*'
    fi | sort
  )
  # One grep for every file, in their order: -l names a file with a line, -I passes a binary one
  # over.
  ((${#paths[@]})) && mapfile -t TEXT_FILES < <(grep -Il -- . "${paths[@]}" 2>/dev/null)
  if ((${#TEXT_FILES[@]} == 0)); then
    add_error "no text files found under $ROOT — the file list is empty, so nothing was checked [docs/files:empty]"
  fi
}

# is_doc_file <file> — true for the file types in which the section sign is a reference into
# AGENTS.md by convention: Markdown, YAML, shell scripts, and the git hooks. Elsewhere, in JSON
# and in source code, the section sign is an ordinary character with its own meanings — a statute
# cited in a German string literal, a translated message, a test fixture — none of which are
# claims about a section of AGENTS.md.
# The ADR checks scan every text file because an identifier of a known category is unambiguous;
# this notation is not.
is_doc_file() {
  case "$1" in
    *.md | *.yml | *.yaml | *.sh | "$ROOT"/.githooks/*) return 0 ;;
    *) return 1 ;;
  esac
}

# is_inactive_adr <file> — true for an ADR whose status is ⚪ superseded or 🔴 rejected. Both are
# historical record rather than live decisions, and both describe a repository state that does not
# exist: a superseded ADR is frozen with the past it was written against (AGENTS.md,
# docs/adr/README.md), and a rejected one describes a road not taken — the check script, the
# workflow job, the AGENTS.md section it names were never created, or were removed together with
# the rejection. Verifying either one's references would demand that the tree still contain what
# the project decided against, so the reference checks below skip them.
is_inactive_adr() {
  [[ "$1" == "$ADR_DIR"/* ]] || return 1
  local status="${ADR_STATUS["$1"]-}"
  [[ "$status" == *⚪* || "$status" == *🔴* ]]
}

check_adr_index() {  # docs/adr-index
  if [[ ! -f "$ADR_INDEX" ]]; then
    add_error "ADR index not found: docs/adr/README.md [docs/adr-index:absent]"
    return
  fi

  # ADR files on disk (basenames), of either form, but the base; the template is neither.
  local disk_files=()
  local f
  for f in "${ADR_LIST[@]}"; do [[ "${f##*/}" == NUC-* ]] || disk_files+=("${f##*/}"); done

  # Parse the index into parallel arrays: filename, 'Applies to' cell, and status emoji per row.
  # Columns are '| ADR | Title | Applies to | Status |'; the cell is compared verbatim.
  local indexed_files=() indexed_applies=() indexed_status=()
  local line number target filename applies status archive=0
  local -a cells
  local row_re="^\\|[[:space:]]*\\[([A-Za-z]+-[^]]+)\\]\\(([^)]+)\\)"
  while IFS= read -r line; do
    [[ "$line" =~ ^#+[[:space:]]+Superseded\ and\ rejected[[:space:]]*$ ]] && { archive=1; continue; }
    [[ "$line" =~ $row_re ]] || continue
    number="${BASH_REMATCH[1]}"
    target="${BASH_REMATCH[2]}"
    filename="${target%%#*}"
    # Cell 3 after the leading pipe, trimmed and compared verbatim.
    IFS='|' read -r -a cells <<< "$line"
    trim "${cells[3]-}"
    applies="$TRIMMED"
    status_emoji "$line"
    status="$EMOJI"
    indexed_files+=("$filename")
    indexed_applies+=("$applies")
    indexed_status+=("$status")
    [[ "$number" == NUC-* ]] && add_error \
      "ADR index: lists $number, an ADR of the base — the index links the base in one line and lists no NUC- ADR (NUC-adrs-are-named-by-category §5) [docs/adr-index:base]"
    [[ "$filename" == "$number.md" ]] \
      || add_error "ADR index: row for $number links to '$filename', not to '$number.md' [docs/adr-index:link]"
    case "$status" in
      ⚪|🔴)
        ((archive)) \
          || add_error "ADR index: row for $number is $status but sits above the 'Superseded and rejected' heading — move it there, a session reads the first table as binding [docs/adr-index:above]"
        ;;
      🟢|🟡)
        ((archive)) \
          && add_error "ADR index: row for $number is $status but sits below the 'Superseded and rejected' heading — move it up, a binding decision there is never read [docs/adr-index:below]"
        ;;
    esac
  done < "$ADR_INDEX"

  # A file listed twice is a row both sides of a merge changed (.githooks/merge-adr-index).
  local -A seen_rows=()
  for f in "${indexed_files[@]}"; do
    [[ -v seen_rows["$f"] ]] \
      && add_error "ADR index: lists '$f' twice — keep the row that matches the file, delete the other [docs/adr-index:twice]"
    seen_rows["$f"]=1
  done

  # Every file on disk must be listed.
  local d
  for d in "${disk_files[@]}"; do
    if [[ ! -v seen_rows["$d"] ]]; then
      add_error "ADR index: file '$d' exists but is not listed in the index [docs/adr-index:unlisted]"
    fi
  done

  # Every listed file must exist, and its status and 'Applies to' must match the file.
  local i file_status file_applies
  for i in "${!indexed_files[@]}"; do
    f="${indexed_files[$i]}"
    status="${indexed_status[$i]}"
    applies="${indexed_applies[$i]}"
    if [[ ! -f "$ADR_DIR/$f" ]]; then
      add_error "ADR index: lists '$f', but no such ADR file exists [docs/adr-index:missing]"
      continue
    fi
    adr_header "$ADR_DIR/$f"
    status_emoji "$HEADER_STATUS"
    file_status="$EMOJI"
    if [[ -z "$file_status" ]]; then
      add_error "$f: no '**Status:**' line with a status emoji found [docs/adr-index:nostatus]"
    elif [[ "$file_status" != "$status" ]]; then
      add_error "ADR index: status for '$f' is ${status:-<none>} in the index but $file_status in the file [docs/adr-index:status]"
    fi
    applies_to_of
    file_applies="$TRIMMED"
    if [[ -z "$file_applies" ]]; then
      add_error "$f: no '**Applies to:**' line naming what the decision constrains [docs/adr-index:noapplies]"
    elif [[ "$file_applies" != "$applies" ]]; then
      add_error "ADR index: 'Applies to' for '$f' is '${applies:-<none>}' in the index but '$file_applies' in the file [docs/adr-index:applies]"
    fi
  done
}

# The rule docs/adr-identifiers (see the header). An identifier is never reused, so a deleted one,
# read off the history, stays taken. The history is the first-parent line of HEAD, each merge
# read against the branch it landed on: a side branch that deleted an ADR and wrote it anew
# deleted nothing there.
check_adr_identifiers() {  # docs/adr-identifiers
  local f base heading id commit
  local -A deleted=()
  [[ -d "$ADR_DIR" ]] && while IFS= read -r f; do
    add_error "${f##*/}: a numbered ADR, a form no check reads — migrate the record with the normalize skill of a v0.9 release, which maps it to '<CAT>-<slug>' [docs/adr-identifiers:numbered]"
  done < <(LC_ALL=C find "$ADR_DIR" -maxdepth 1 -type f -name '[0-9]*-*.md' | LC_ALL=C sort)
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    adr_key_of "$base"
    if [[ ! "$ADR_ID" =~ ^$ADR_NEW_RE$ ]]; then
      add_error "$base: the identifier is not '<CAT>-<slug>': two to five capital letters, a hyphen, and two to five lower case words or numbers joined by hyphens [docs/adr-identifiers:form]"
      continue
    elif ! _contains "${ADR_ID%%-*}" "${ADR_CATEGORIES[@]}"; then
      add_error "$base: the category ${ADR_ID%%-*} is neither fixed nor in the category list of docs/adr/README.md [docs/adr-identifiers:category]"
    fi
    heading="${ADR_HEADING["$f"]-}"
    [[ "$heading" == "# $ADR_ID:"* ]] \
      || add_error "$base: heading says '${heading:-<none>}', but the filename says $ADR_ID [docs/adr-identifiers:heading]"
  done
  # Without a commit there is no history; a git that cannot read it, such as one before 2.31
  # without --diff-merges, is an error, not a pass.
  local log=""
  if git -C "$ROOT" rev-parse -q --verify 'HEAD^{commit}' > /dev/null 2>&1; then
    local args=(log --first-parent --diff-merges=first-parent --no-renames --diff-filter=D --name-only
                --format='commit %h' HEAD -- ":(glob)docs/adr/$ADR_GLOB")
    # stderr stays out of the list: a warning on a success would be read as a file name.
    log="$(git -C "$ROOT" "${args[@]}" 2>/dev/null)" || { log="$(git -C "$ROOT" "${args[@]}" 2>&1 >/dev/null)"
      add_error "ADR identifiers: git log cannot read the deleted ADRs (git 2.31 or later is needed): ${log%%$'\n'*} [docs/adr-identifiers:history]"; log=""; }
  fi
  while IFS= read -r f; do
    if [[ "$f" == commit\ * ]]; then commit="${f#commit }"; elif [[ -n "$f" && ! -v deleted["${f##*/}"] ]]; then deleted["${f##*/}"]="$commit"; fi
  done <<< "$log"
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    [[ -v deleted["$base"] ]] || continue
    id="${base%.md}"
    add_error "$base: the identifier $id was taken by a deleted ADR, deleted in ${deleted["$base"]} — a successor takes a new slug [docs/adr-identifiers:taken]"
  done
}

# instruction_needle — the opening of the instruction paragraph in docs/adr/template.md: the first
# line of its italic block, without the leading '*' and cut to a phrase. Reading the needle off the
# template instead of hard-coding a sentence is what keeps the check alive when the paragraph is
# reworded — which is what a project does with a template. Empty when the template is gone or
# carries no such block; the sub-check then does not run rather than reporting a phantom.
instruction_needle() {
  local line
  line="$(grep -m1 -E '^\*[^*]' "$ADR_DIR/template.md" 2>/dev/null)" || return 0
  printf '%s' "${line:1:40}"
}

# Every ADR's '**Status:**' line carries exactly one legend emoji plus the word that belongs to it,
# and no ADR still carries the instruction paragraph from template.md. check_adr_index reads the
# status as the first emoji status_emoji finds, so on its own it would accept a line that kept a
# list of alternatives ('🟡 proposed | 🟢 accepted | …') as whatever stands first, and a line whose word
# and emoji disagree passes there on the strength of the emoji — while the word is what a reader
# believes. A missing or emoji-less status line is left to check_adr_index, which reports it.
check_adr_headers() {  # docs/adr-header
  local f base line expected word stray
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    while IFS= read -r stray; do
      [[ -n "$stray" ]] || continue
      add_error "$base:${stray%%:*}: a header field not written as the list item '- **<Name>:** …' of template.md — every script reads only that form [docs/adr-header:field]"
    done <<< "${ADR_STRAY["$f"]-}"
    line="${ADR_STATUS["$f"]-}"
    status_emoji "$line"
    if [[ -n "$EMOJI" ]]; then
      case "$EMOJI" in
        🟢) expected=accepted ;;
        🟡) expected=proposed ;;
        🔴) expected=rejected ;;
        ⚪) expected=superseded ;;
      esac
      read -r word _ <<<"${line#*"$EMOJI"}"
      if ((EMOJI_COUNT > 1)); then
        add_error "$base: status line carries $EMOJI_COUNT status emoji — keep exactly one [docs/adr-header:emojis]"
      elif [[ "$word" != "$expected" ]]; then
        add_error "$base: status line reads '$EMOJI ${word:-<nothing>}', but $EMOJI means '$expected' [docs/adr-header:word]"
      fi
    fi
    if [[ -v ADR_NEEDLE["$f"] ]]; then
      add_error "$base: still carries the instruction paragraph from template.md — delete it [docs/adr-header:instruction]"
    fi
  done
}

# superseding_id <status line> — sets SUCCESSOR to the identifier that a '⚪ superseded by <id>'
# status line names, empty when it names none. The citation may be plain or a Markdown link, so
# the bracket is optional (docs/adr/README.md asks for the link form).
SUCCESSOR=""
superseding_id() {
  local re="superseded by \\[?($ADR_REF_RE)([^$ADR_WORD_CHARS]|\$)"
  SUCCESSOR=""
  [[ "$1" =~ $re ]] && SUCCESSOR="${BASH_REMATCH[1]}"
  return 0
}

# adr_refs <text> — sets REFS to every ADR identifier the text cites, in order.
REFS=()
adr_refs() {
  local rest="$1"
  REFS=()
  while [[ "$rest" =~ $ADR_REF_WORD_RE ]]; do
    REFS+=("${BASH_REMATCH[2]}")
    rest="${rest#*"${BASH_REMATCH[0]}"}"
  done
}

# A supersession is written into two files: the new ADR's '**Supersedes:**' field, from the
# proposal on, and the old ADR's status line, which the human flips when accepting the new one
# (docs/adr/README.md). Nothing else compares them — check_adr_index only mirrors the index
# against whatever status a file carries, so a half performed flip passes it. Both directions
# are verified here; a cited identifier that names no file is left to check_adr_refs, which reports
# it repository-wide. A Supersedes field wraps at the line width like any other header field,
# and a name on its second line counts.
check_adr_supersessions() {  # docs/adr-supersessions
  local f base nr line target target_file succ_file named accepted field rest re="(NUC-$ADR_SLUG_RE) ?§ ?([0-9]+)"
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    adr_key_of "$f"
    nr="$ADR_ID"
    line="${ADR_STATUS["$f"]-}"

    # Forward: once this one is accepted, every ADR it claims to supersede — one, or several
    # whose decisions it consolidates — must carry the matching status line. One superseded in
    # its turn was accepted before, and its own successor keeps the line it flipped.
    status_emoji "$line"
    accepted=0
    [[ "$EMOJI" == "🟢" || "$EMOJI" == "⚪" ]] && accepted=1
    adr_refs "${ADR_SUPERSEDES["$f"]-}"
    for target in "${REFS[@]}"; do
      [[ "$target" == NUC-* && "$base" != NUC-* ]] && add_error \
        "$base: supersedes $target — no project ADR supersedes the base; extend or narrow its clause, or fork the template (NUC-base-is-fixed §1) [docs/adr-supersessions:base]"
      target_file="${ADR_BY_ID["$target"]-}"
      if [[ -n "$target_file" ]] && ((accepted)); then
        superseding_id "${ADR_STATUS["$target_file"]-}"
        [[ "$SUCCESSOR" == "$nr" ]] || add_error \
          "${target_file##*/}: $base supersedes it, but its status line does not say 'superseded by $nr' [docs/adr-supersessions:unflipped]"
      fi
    done

    # Backward: a superseded ADR names its successor, and the successor claims it.
    superseding_id "$line"
    if [[ -z "$SUCCESSOR" ]]; then
      status_emoji "$line"
      [[ "$EMOJI" == "⚪" ]] && add_error "$base: status is ⚪ but the line names no superseding ADR [docs/adr-supersessions:unnamed]"
    else
      succ_file="${ADR_BY_ID["$SUCCESSOR"]-}"
      if [[ -n "$succ_file" ]]; then
        adr_refs "${ADR_SUPERSEDES["$succ_file"]-}"
        named=0
        for target in "${REFS[@]}"; do [[ "$target" == "$nr" ]] && named=1; done
        ((named)) || add_error "${succ_file##*/}: $base says it supersedes, but it carries no '**Supersedes:**' field naming $nr [docs/adr-supersessions:unclaimed]"
        # A successor superseded in its turn was accepted before: a chain of supersessions.
        status_emoji "${ADR_STATUS["$succ_file"]-}"
        [[ "$EMOJI" == "🟢" || "$EMOJI" == "⚪" ]] || add_error \
          "$base: superseded by $SUCCESSOR, which is not accepted — the status line flips when the human accepts $SUCCESSOR [docs/adr-supersessions:unaccepted]"
      fi
    fi

    # An extension or a narrowing names clauses of the base that exist.
    for field in Extends Narrows; do
      rest="${ADR_REL["$field $f"]-}"
      [[ -n "$rest" ]] || continue
      [[ "$rest" =~ $re ]] || add_error "$base: the $field field names no clause of a NUC- ADR, 'NUC-<slug> §n' (NUC-base-is-fixed §1) [docs/adr-supersessions:field]"
      while [[ "$rest" =~ $re ]]; do
        rest="${rest#*"${BASH_REMATCH[0]}"}" target="${BASH_REMATCH[1]} §${BASH_REMATCH[-1]}"
        target_file="${ADR_BY_ID["${BASH_REMATCH[1]}"]-}"
        [[ -z "$target_file" ]] || grep -qE "^${BASH_REMATCH[-1]}\. " <<< "$(adr_section Decision < "$target_file")" \
          || add_error "$base: $field $target, a clause its Decision does not have [docs/adr-supersessions:clause]"
      done
    done
  done
}

# Print the GitHub-style slug of every ATX heading in a Markdown file. One awk pass per file: a
# pipeline per heading made this the most expensive part of the script.
extract_heading_slugs() {
  awk '
    match($0, /^#+[[:space:]]+/) {
      hashes = $0; sub(/[^#].*$/, "", hashes)
      if (length(hashes) > 6) next
      text = substr($0, RLENGTH + 1)
      # Drop a trailing run of "#" (closed ATX headings), then slugify the way GitHub does:
      # lowercase, remove everything but [a-z0-9 _-], then one hyphen per remaining space. A run
      # of spaces yields a run of hyphens: GitHub drops a character such as "&" and keeps the
      # spaces around it, so "Quality bar & Definition of Done" anchors as
      # "quality-bar--definition-of-done".
      sub(/[[:space:]]+#*[[:space:]]*$/, "", text)
      text = tolower(text)
      gsub(/[^a-z0-9 _-]+/, "", text)
      gsub(/[[:space:]]/, "-", text)
      print text
    }
  ' "$1"
}

# HEADING_SLUGS — the slugs of each Markdown file an anchor was looked up in, newline-framed, keyed
# by path: a file is linked from many others, and its headings are read once.
declare -A HEADING_SLUGS=()

# anchor_resolves <file> <fragment> — true if the fragment exists in the file.
# Handles GitHub line anchors (Lnn, Lnn-Lmm) for any file and heading-slug anchors for
# Markdown files; any other anchor is unverifiable from disk and is accepted.
anchor_resolves() {
  local file="$1" frag="$2" start end lines
  if [[ "$frag" =~ ^L([0-9]+)(-L([0-9]+))?$ ]]; then
    start="${BASH_REMATCH[1]}"; end="${BASH_REMATCH[3]:-$start}"
    lines="$(wc -l < "$file")"
    # +1 tolerates a final line with no trailing newline (uncounted by wc -l).
    (( start >= 1 && end >= start && end <= lines + 1 ))
    return
  fi
  case "$file" in
    *.md)
      [[ -v HEADING_SLUGS["$file"] ]] \
        || HEADING_SLUGS["$file"]=$'\n'"$(extract_heading_slugs "$file")"$'\n'
      [[ "${HEADING_SLUGS["$file"]}" == *$'\n'"$frag"$'\n'* ]] ;;
    *) return 0 ;;
  esac
}

# The Markdown of the repository, read by one awk pass over every file instead of a pipeline per
# file and per check. Keyed by the file's path:
#
#   MD_LINKS  every '[text](target)' expression outside a fenced code block, one per line. A fence
#             (``` or ~~~, also inside a blockquote) opens and closes on any line that starts with
#             one: links in fences are illustrations, not claims about files on disk.
#   MD_SECT   every line outside such a fence that carries the section sign, as '<line>:<text>'.
#   MD_WIDE   every line longer than 100 bytes outside the front matter and a fence as the rule
#             docs/prose-width reads one, as '<line>:<text>': a fence there closes only on
#             a line of the same character, at least as long, and nothing else. Bytes are at
#             least the characters, so docs/prose-width counts the characters of these lines alone.
declare -A MD_LINKS=() MD_SECT=() MD_WIDE=()
scan_markdown() {
  local files=() f kind file line value
  for f in "${TEXT_FILES[@]}"; do [[ "$f" == *.md ]] && files+=("$f"); done
  ((${#files[@]})) || return 0
  while IFS=$'\x1f' read -r kind file value; do
    case "$kind" in
      L) MD_LINKS["$file"]+="$value"$'\n' ;;
      S) MD_SECT["$file"]+="$value"$'\n' ;;
      W) MD_WIDE["$file"]+="$value"$'\n' ;;
    esac
  done < <(awk '
    function out(k, v) { printf "%s\037%s\037%s\n", k, FILENAME, v }
    FNR == 1 { fence = 0; wfence = ""; front = 0 }
    {
      if (/^[[:space:]>]*(```|~~~)/) fence = !fence
      else if (!fence) {
        rest = $0
        while (match(rest, /\[[^]]*\]\([^)]+\)/)) {
          out("L", substr(rest, RSTART, RLENGTH))
          rest = substr(rest, RSTART + RLENGTH)
        }
        if (index($0, "§")) out("S", FNR ":" $0)
      }

      if (FNR == 1 && $0 == "---") { front = 1; next }
      if (front) { if ($0 == "---") front = 0; next }
      if (wfence != "") {
        if (match($0, /^[[:space:]]*[`~]+[[:space:]]*$/)) {
          run = $0; gsub(/[[:space:]]/, "", run)
          if (length(run) >= 3 && index(run, wfence) == 1) wfence = ""
        }
        next
      }
      if (match($0, /^[[:space:]]*(```+|~~~+)/)) {
        wfence = substr($0, RSTART, RLENGTH); sub(/^[[:space:]]+/, "", wfence)
        if (length(wfence) >= 3) next
        wfence = ""
      }
      if (length($0) > 100) out("W", FNR ":" $0)
    }
  ' "${files[@]}")
}

# link_target <linkexpr> — sets LINK_TARGET to the target of one '[text](target)' expression:
# trimmed, with an optional trailing '"title"' or "'title'" removed (a link title is not part of
# the path). It sets a variable rather than printing, so that a caller reading every link of the
# repository forks no subshell per link.
LINK_EXPR_RE='^\[[^]]*\]\(([^)]+)\)$'
# LINK_RE — one '[text](target)' expression anywhere in a line, as the checks' grep -o reads it.
LINK_RE='\[[^]]*\]\([^)]+\)'
LINK_TITLE_RE="^([^[:space:]]+)[[:space:]]+(\"[^\"]*\"|'[^']*')\$"
LINK_TARGET=""
link_target() {
  local target="$1"
  [[ "$target" =~ $LINK_EXPR_RE ]] && target="${BASH_REMATCH[1]}"
  target="${target#"${target%%[![:space:]]*}"}"
  target="${target%"${target##*[![:space:]]}"}"
  [[ "$target" =~ $LINK_TITLE_RE ]] && target="${BASH_REMATCH[1]}"
  LINK_TARGET="$target"
}

check_relative_links() {  # docs/links
  local md rel linkexpr target path_part frag dir target_file
  for md in "${TEXT_FILES[@]}"; do
    case "$md" in *.md) ;; *) continue ;; esac
    rel="${md#"$ROOT"/}"
    is_inactive_adr "$md" && continue
    dir="${md%/*}"
    while IFS= read -r linkexpr; do
      [[ -n "$linkexpr" ]] || continue
      link_target "$linkexpr"
      target="$LINK_TARGET"
      case "$target" in
        http://*|https://*|mailto:*) continue ;;
      esac
      # '<CAT>' is the placeholder docs/adr/template.md uses where a real ADR carries its
      # identifier, so such a target names no file and claims none exists.
      case "$target" in *'<CAT>'*) continue ;; esac
      # Split into a path part and an optional '#fragment'. An empty path is a same-file anchor.
      path_part="${target%%#*}"
      frag=""
      [[ "$target" == *"#"* ]] && frag="${target#*#}"
      if [[ -z "$path_part" ]]; then
        target_file="$md"
      else
        target_file="$dir/$path_part"
        if [[ ! -e "$target_file" ]]; then
          add_error "$rel: broken relative link -> '$target' [docs/links:broken]"
          continue
        fi
      fi
      # Validate the anchor fragment against the resolved file, when there is one.
      if [[ -n "$frag" && -f "$target_file" ]]; then
        anchor_resolves "$target_file" "$frag" \
          || add_error "$rel: link '$target' has no matching anchor '#$frag' in ${target_file#"$ROOT"/} [docs/links:anchor]"
      fi
    done <<< "${MD_LINKS["$md"]-}"
  done
}

# code_spans_out <line> — sets LINE to the line without its inline code spans. A span runs from a
# backtick to the next one; an unpaired backtick stays.
LINE=""
code_spans_out() {
  local rest="$1"
  LINE=""
  while [[ "$rest" == *'`'*'`'* ]]; do
    LINE+="${rest%%\`*}" rest="${rest#*\`}" rest="${rest#*\`}"
  done
  LINE+="$rest"
}

# clauses_out <line> — sets LINE to the line without its clause citations, an ADR's identifier, a
# space, and the section sign with a number, or a list of them ('§4, §5', '§4 and §5', '§2–§5').
clauses_out() {
  local re="($ADR_REF_RE) §[0-9]+((, |,? and |–)§[0-9]+)*"
  LINE="$1"
  while [[ "$LINE" =~ $re ]]; do LINE="${LINE/"${BASH_REMATCH[0]}"/}"; done
}

# Section references point into AGENTS.md, whose sections are numbered '## N.' headings. Inserting
# or removing a section renumbers the ones after it, and a bare '§N' would keep pointing at
# whatever now carries that number — valid-looking and wrong. In Markdown a reference is
# therefore written as a link to the section's own anchor, which carries the number *and* the
# title ('AGENTS.md#3-adr-rules'): renumbering or retitling a section makes that anchor stop
# resolving, and docs/links fails on every reference to it. This check verifies the other half
# — that the number in the text and the anchor the link points at name the same section.
#
# Named gap (AGENTS.md, section 5): outside Markdown there are no links, so a '§N' in a workflow or
# a script comment is still only checked for the number existing. Those references live in files a
# maintainer edits next to AGENTS.md; the weaker guarantee is recorded here rather than hidden.
check_section_refs() {  # docs/section-refs
  local agents="$ROOT/AGENTS.md"
  if [[ ! -f "$agents" ]]; then
    add_error "AGENTS.md not found in the repository root [docs/section-refs:absent]"
    return
  fi

  local valid_sections=()
  local n
  while IFS= read -r n; do
    valid_sections+=("$n")
  done < <(sed -nE 's/^## ([0-9]+)\..*/\1/p' "$agents")
  if ((${#valid_sections[@]} == 0)); then
    add_error "AGENTS.md: no numbered '## N.' section headings found [docs/section-refs:unnumbered]"
    return
  fi

  # The numbering itself: 1..N in order, no gap and no repeat. A botched renumbering is what this
  # catches; that a reference still means the section it meant before is what the anchors do.
  local i expected
  for i in "${!valid_sections[@]}"; do
    expected=$((i + 1))
    if [[ "${valid_sections[$i]}" != "$expected" ]]; then
      add_error "AGENTS.md: numbered sections must run 1..N in order — expected '## $expected.' at position $expected, found '## ${valid_sections[$i]}.' [docs/section-refs:order]"
      break
    fi
  done

  # The anchor each section carries, read with the same slug rule the link check uses.
  local section_slug=() slug
  while IFS= read -r slug; do
    [[ "$slug" =~ ^([0-9]+)- ]] || continue
    n="${BASH_REMATCH[1]}"
    _contains "$n" "${valid_sections[@]}" && section_slug[n]="$slug"
  done < <(extract_heading_slugs "$agents")

  local f rel lineno line clean linkexpr target frag rest
  local links=()
  for f in "${TEXT_FILES[@]}"; do
    rel="${f#"$ROOT"/}"
    is_inactive_adr "$f" && continue
    # The word form, 'AGENTS.md, section N', is unambiguous wherever it stands, so it is read in
    # every text file but Markdown, which cites a section as a link.
    if [[ "$f" != *.md ]]; then
      while IFS=: read -r lineno line; do
        n="${line##* }"
        _contains "$n" "${valid_sections[@]}" \
          || add_error "$rel:$lineno: reference 'AGENTS.md, section $n' matches no numbered section in AGENTS.md [docs/section-refs:unmatched]"
      done < <(grep -noE 'AGENTS\.md,? section [0-9]+' "$f")
    fi
    is_doc_file "$f" || continue

    case "$f" in
      *.md)
        # Only the lines that carry the section sign, outside a fence (scan_markdown).
        while IFS= read -r line; do
          [[ -n "$line" ]] || continue
          lineno="${line%%:*}"
          line="${line#*:}"
          # Inline code spans hold literals, not citations: '`§1`' in a sentence about references
          # is the notation itself. Dropping them also strips the '`AGENTS.md`' inside a link text,
          # which leaves the link expression itself intact.
          code_spans_out "$line"
          clauses_out "$LINE"
          line="$LINE"
          [[ "$line" == *§* ]] || continue
          # Split the line into its links and what is left once every link is removed, which is a
          # reference nothing verifies. Each match is the leftmost one, so cutting the text up to
          # its first occurrence cuts exactly the text up to the match.
          links=() clean="" rest="$line"
          while [[ "$rest" =~ $LINK_RE ]]; do
            links+=("${BASH_REMATCH[0]}")
            clean+="${rest%%"${BASH_REMATCH[0]}"*}"
            rest="${rest#*"${BASH_REMATCH[0]}"}"
          done
          clean+="$rest"
          for linkexpr in "${links[@]}"; do
            [[ "$linkexpr" =~ §([0-9]+) ]] || continue
            n="${BASH_REMATCH[1]}"
            if ! _contains "$n" "${valid_sections[@]}"; then
              add_error "$rel:$lineno: reference '§$n' matches no numbered section in AGENTS.md [docs/section-refs:unmatched]"
              continue
            fi
            link_target "$linkexpr"
            target="$LINK_TARGET"
            frag=""
            [[ "$target" == *"#"* ]] && frag="${target#*#}"
            if [[ -z "$frag" ]]; then
              add_error "$rel:$lineno: '§$n' is cited without an anchor — point the link at '#${section_slug[$n]}' [docs/section-refs:anchorless]"
            elif [[ "$frag" != "${section_slug[$n]}" ]]; then
              add_error "$rel:$lineno: '§$n' links to anchor '#$frag', but section $n of AGENTS.md anchors as '#${section_slug[$n]}' [docs/section-refs:anchor]"
            fi
          done
          rest="$clean"
          while [[ "$rest" =~ §([0-9]+) ]]; do
            n="${BASH_REMATCH[1]}"
            rest="${rest#*"${BASH_REMATCH[0]}"}"
            if _contains "$n" "${valid_sections[@]}"; then
              add_error "$rel:$lineno: '§$n' stands outside a link — cite a section as a link to its anchor, e.g. [\`AGENTS.md\` §$n](AGENTS.md#${section_slug[$n]}) [docs/section-refs:unlinked]"
            else
              add_error "$rel:$lineno: reference '§$n' matches no numbered section in AGENTS.md [docs/section-refs:unmatched]"
            fi
          done
        done <<< "${MD_SECT["$f"]-}"
        ;;
      *)
        while IFS=: read -r lineno line; do
          clauses_out "$line"
          while [[ "$LINE" =~ §([0-9]+) ]]; do
            n="${BASH_REMATCH[1]}" LINE="${LINE#*"${BASH_REMATCH[0]}"}"
            _contains "$n" "${valid_sections[@]}" \
              || add_error "$rel:$lineno: reference '§$n' matches no numbered section in AGENTS.md [docs/section-refs:unmatched]"
          done
        done < <(grep -nE '§[0-9]+' "$f")
        ;;
    esac
  done
}

check_adr_refs() {  # docs/adr-refs
  local f files=() lineno line match
  for f in "${TEXT_FILES[@]}"; do
    [[ "$f" == "$ADR_DIR/template.md" ]] && continue
    is_inactive_adr "$f" && continue
    files+=("$f")
  done
  ((${#files[@]})) || return 0
  # One grep over every file; --null ends each file name with a NUL, which no path carries.
  while IFS= read -r -d '' f && IFS=: read -r lineno line; do
    [[ "$f" == *.md ]] && code_spans_out "$line" && line="$LINE"
    while [[ "$line" =~ $ADR_REF_WORD_RE ]]; do
      match="${BASH_REMATCH[2]}" line="${line#*"${BASH_REMATCH[0]}"}"
      [[ -v ADR_BY_ID["$match"] ]] \
        || add_error "${f#"$ROOT"/}:$lineno: reference '$match' matches no ADR file in docs/adr/ [docs/adr-refs:unmatched]"
    done
  done < <(grep -Hn --null -E "$ADR_REF_RE" "${files[@]}")
  # A rule key an ADR in force names of this script is a key of its header (see the header).
  local path keys key
  for f in "${ADR_LIST[@]}"; do
    [[ "${ADR_APPLIES["$f"]-}" == *"the rule"* ]] || continue
    is_inactive_adr "$f" && continue
    while IFS=$'\t' read -r path keys; do
      [[ "$path" == scripts/check-docs.sh ]] || continue
      for key in $keys; do
        [[ "$RULE_KEYS" == *" $key "* ]] \
          || add_error "${f#"$ROOT"/}: Applies to names the rule '$key' of scripts/check-docs.sh, which its header does not list [docs/adr-refs:unlisted]"
      done
    done < <(adr_rule_paths "${ADR_APPLIES["$f"]}")
  done
}

check_adr_link_targets() {  # docs/adr-link-targets
  local f rel linkexpr text target base id dir
  for f in "${TEXT_FILES[@]}"; do
    case "$f" in *.md) ;; *) continue ;; esac
    rel="${f#"$ROOT"/}"
    [[ "$f" == "$ADR_DIR/template.md" ]] && continue
    is_inactive_adr "$f" && continue
    while IFS= read -r linkexpr; do
      [[ -n "$linkexpr" ]] || continue
      # The text runs from the opening bracket to the first closing one, which it cannot contain.
      text="${linkexpr#\[}"
      text="${text%%]*}"
      [[ "$text" =~ $ADR_REF_WORD_RE ]] || continue
      id="${BASH_REMATCH[2]}"
      link_target "$linkexpr"
      target="${LINK_TARGET%%#*}"
      base="${target##*/}"
      # Compare only against an ADR file in docs/adr/; anything else is not a claim about which
      # ADR it is.
      dir="${f%/*}"
      [[ "$target" == */* ]] && dir+="/${target%/*}"
      adr_is_file "$base" || continue
      [[ "$(cd "$dir" 2>/dev/null && pwd)" == "$ADR_DIR" ]] || continue
      adr_key_of "$base"
      [[ "$ADR_ID" == "$id" ]] || add_error "$rel: link '$linkexpr' cites $id but points at '$base' [docs/adr-link-targets:other]"
    done <<< "${MD_LINKS["$f"]-}"
  done
}

check_check_lists() {  # docs/check-lists
  local rel_wf=".github/workflows/checks.yml" rel_all="scripts/check-all.sh"
  local wf="$ROOT/$rel_wf" all="$ROOT/$rel_all" readme="$ROOT/CONTRIBUTING.md"
  local f
  for f in "$wf" "$all" "$readme"; do
    if [[ ! -f "$f" ]]; then
      add_error "${f#"$ROOT"/}: not found — the check lists cannot be compared [docs/check-lists:absent]"
      return
    fi
  done

  # The workflow: job names at two-space indent under 'jobs:', and the check script each job runs.
  # A job that runs no script under scripts/ — its commands written inline — is work CI does and
  # the local gate cannot: the toolchain job of a derived project, written that way, left the
  # pre-push hook without the build and the tests while the two lists still agreed.
  # Every workflow under .github/workflows/ counts for the scripts it runs; the jobs of checks.yml
  # alone are the required checks. A workflow that publishes rather than checks, such as a
  # release, builds and uploads with commands of its own that no push could run; the local runner
  # names it on a line '# Not a gate: .github/workflows/<file>', and its jobs may then run no
  # script, while the scripts they do run still count. checks.yml is the gate and cannot be named.
  local not_gate=() name
  while IFS= read -r name; do
    if [[ "$ROOT/$name" == "$wf" ]]; then
      add_error "$rel_all: '# Not a gate: $name' names the workflow of the required checks [docs/check-lists:gate]"
    elif [[ ! "$name" =~ ^\.github/workflows/[A-Za-z0-9._-]+\.ya?ml$ || ! -f "$ROOT/$name" ]]; then
      add_error "$rel_all: '# Not a gate: $name' names no workflow of this repository [docs/check-lists:nonworkflow]"
    fi
    not_gate+=("$name")
  done < <(sed -n 's/^#[[:space:]]*Not a gate:[[:space:]]*\([^[:space:]]*\).*/\1/p' "$all")
  local line job="" job_scripts=0 in_jobs=0 file rel is_checks is_gate key=""
  local wf_jobs=() wf_scripts=() matrix_jobs=()
  # job_done — report the job just parsed when none of its steps ran a script.
  job_done() {
    [[ -n "$job" ]] && ((is_gate && job_scripts == 0)) \
      && add_error "$rel: job '$job' runs no script under scripts/ — the local gate cannot run what it does [docs/check-lists:inline]"
    job=""
  }
  for file in "$ROOT"/.github/workflows/*.yml "$ROOT"/.github/workflows/*.yaml; do
    [[ -f "$file" ]] || continue
    rel="${file#"$ROOT"/}" in_jobs=0 job="" job_scripts=0 is_checks=0 is_gate=1
    [[ "$file" == "$wf" ]] && is_checks=1
    _contains "$rel" "${not_gate[@]}" && is_gate=0
    while IFS= read -r line; do
      [[ "$line" == "jobs:"* ]] && { in_jobs=1; continue; }
      ((in_jobs)) || continue
      if [[ "$line" =~ ^\ \ ([a-z][a-z0-9-]*):[[:space:]]*$ ]]; then
        job_done
        job="${BASH_REMATCH[1]}"
        job_scripts=0
        ((is_checks)) && wf_jobs+=("$job")
        key=""
        continue
      fi
      # A job with a matrix reports one check run per entry, '<job> (<values>)', never '<job>'.
      # The matrix is the one under the job's 'strategy:', in block or in flow style; a 'matrix'
      # elsewhere, such as an output that computes one, is no strategy of this job.
      [[ "$line" =~ ^\ {4}([a-z][a-z-]*): ]] && key="${BASH_REMATCH[1]}"
      if ((is_checks)) && [[ -n "$job" ]] \
        && { [[ "$key" == strategy && "$line" =~ ^\ {6}matrix: ]] \
          || [[ "$line" =~ ^\ {4}strategy:.*[{,][[:space:]]*matrix: ]]; }; then
        _contains "$job" "${matrix_jobs[@]}" || matrix_jobs+=("$job")
      fi
      [[ "$line" =~ run:[[:space:]]+bash[[:space:]]+(scripts/[A-Za-z0-9._-]+\.sh) ]] \
        && { wf_scripts+=("${BASH_REMATCH[1]}"); job_scripts=$((job_scripts + 1)); }
    done < "$file"
    job_done
  done
  if ((${#wf_jobs[@]} == 0)); then
    add_error "$rel_wf: no jobs found under 'jobs:' — the check lists cannot be compared [docs/check-lists:jobless]"
    return
  fi

  # The local runner: every 'run <label> <script>' line.
  local all_scripts=()
  while IFS= read -r line; do
    [[ "$line" =~ ^[[:space:]]*run[[:space:]]+\"[^\"]*\"[[:space:]]+(scripts/[A-Za-z0-9._-]+\.sh) ]] \
      && all_scripts+=("${BASH_REMATCH[1]}")
  done < "$all"
  if ((${#all_scripts[@]} == 0)); then
    add_error "$rel_all: no 'run <label> <script>' invocations found — the check lists cannot be compared [docs/check-lists:runless]"
    return
  fi

  local s
  for s in "${wf_scripts[@]}"; do
    _contains "$s" "${all_scripts[@]}" \
      || add_error "$rel_all: does not run '$s', which a workflow runs — CI and the local gate disagree [docs/check-lists:unrun]"
  done
  for s in "${all_scripts[@]}"; do
    _contains "$s" "${wf_scripts[@]}" \
      || add_error "$rel_all: runs '$s', which no job of a workflow runs — CI and the local gate disagree [docs/check-lists:extra]"
  done

  # Every script under scripts/ runs in the two lists, unless the local runner names it on a line
  # '# On instruction: scripts/<name>.sh', as for the scripts of NUC-checks-are-bash §7.
  # A script in neither list is a check nobody runs.
  local on_instruction=() name
  while IFS= read -r name; do on_instruction+=("$name"); done \
    < <(sed -n 's/^#[[:space:]]*On instruction:[[:space:]]*\(scripts\/[A-Za-z0-9._-]*\.sh\).*/\1/p' "$all")
  while IFS= read -r name; do
    s="scripts/${name##*/}"
    [[ "$s" == "$rel_all" ]] && continue
    _contains "$s" "${all_scripts[@]}" "${on_instruction[@]}" \
      || add_error "$s: no workflow and not $rel_all runs it — add it to both lists, or name it on an '# On instruction:' line of $rel_all [docs/check-lists:orphan]"
  done < <(find "$ROOT/scripts" -maxdepth 1 -type f -name '*.sh' | LC_ALL=C sort)

  # The contributing guide: the check names fenced by the 'required-checks' markers, which a maintainer copies
  # into the ruleset. Rulesets name checks as they report: a job by its name, a job with a matrix
  # by one check run per entry, '<job> (<values>)'. A name that does not report is a required
  # check that never arrives and blocks every pull request. Which values a matrix has is the
  # workflow's; the check holds only the form.
  local in_block=0 readme_names=() readme_jobs=() base
  while IFS= read -r line; do
    [[ "$line" == *"required-checks begin"* ]] && { in_block=1; continue; }
    [[ "$line" == *"required-checks end"* ]] && { in_block=0; continue; }
    ((in_block)) || continue
    # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
    while IFS= read -r name; do
      readme_names+=("$name")
    done < <(grep -oE '`[a-z][a-z0-9-]*( \([^`()]+\))?`' <<< "$line" | tr -d '`')
  done < "$readme"
  if ((${#readme_names[@]} == 0)); then
    add_error "CONTRIBUTING.md: no job names between the 'required-checks' markers — the required status checks are unverifiable [docs/check-lists:unnamed]"
    return
  fi

  for name in "${readme_names[@]}"; do
    base="${name%% (*}"
    readme_jobs+=("$base")
    if ! _contains "$base" "${wf_jobs[@]}"; then
      add_error "CONTRIBUTING.md: required check '$name' is not a job of $rel_wf — a ruleset would wait for a check that never reports [docs/check-lists:unreported]"
    elif [[ "$name" == "$base" ]] && _contains "$base" "${matrix_jobs[@]}"; then
      add_error "CONTRIBUTING.md: required check '$name' is a job with a matrix, which reports as '$name (<values>)' — name each check run [docs/check-lists:matrix]"
    elif [[ "$name" != "$base" ]] && ! _contains "$base" "${matrix_jobs[@]}"; then
      add_error "CONTRIBUTING.md: required check '$name' names a check run of '$base', which has no matrix and reports as '$base' [docs/check-lists:nomatrix]"
    fi
  done
  for job in "${wf_jobs[@]}"; do
    _contains "$job" "${readme_jobs[@]}" \
      || add_error "CONTRIBUTING.md: job '$job' of $rel_wf is missing from the required checks — it would run without gating a merge [docs/check-lists:ungated]"
  done
}

check_project_layout() {  # docs/project-layout
  local readme="$ROOT/CONTRIBUTING.md"
  if [[ ! -f "$readme" ]]; then
    add_error "CONTRIBUTING.md: not found — the Project Layout block cannot be checked [docs/project-layout:absent]"
    return
  fi

  local line path in_section=0 in_fence=0 entries=0 listed=()
  while IFS= read -r line; do
    if ((!in_section)); then
      [[ "$line" == "## Project Layout" ]] && in_section=1
      continue
    fi
    if ((!in_fence)); then
      [[ "$line" =~ ^(\`\`\`|~~~) ]] && { in_fence=1; continue; }
      # A new heading before the fence means the section holds no block at all.
      [[ "$line" =~ ^##[[:space:]] ]] && break
      continue
    fi
    [[ "$line" =~ ^(\`\`\`|~~~) ]] && break
    path="${line%%[[:space:]]*}"
    # Blank lines and the continuation lines of a wrapped comment carry no path.
    [[ -z "$path" || "$path" == "#"* ]] && continue
    entries=$((entries + 1))
    listed+=("$path")
    if [[ "$path" == *"*"* ]]; then
      compgen -G "$ROOT/$path" >/dev/null \
        || add_error "CONTRIBUTING.md: Project Layout lists '$path', which matches nothing in this repository [docs/project-layout:unmatched]"
    elif [[ "$path" == */ ]]; then
      # Open work lives under docs/plan/ only while there is some (NUC-large-work-is-planned §2).
      [[ -d "$ROOT/${path%/}" || "$path" == "docs/plan/" ]] \
        || add_error "CONTRIBUTING.md: Project Layout lists '$path', which is not a directory of this repository [docs/project-layout:notdir]"
    else
      [[ -f "$ROOT/$path" ]] \
        || add_error "CONTRIBUTING.md: Project Layout lists '$path', which does not exist [docs/project-layout:missing]"
    fi
  done < "$readme"

  if ((entries == 0)); then
    add_error "CONTRIBUTING.md: no paths found under '## Project Layout' — the inventory is unverifiable [docs/project-layout:empty]"
    return
  fi

  # The reverse: every top-level part of the tree is named, by itself or by a path under it.
  local top covered
  while IFS= read -r top; do
    covered=0
    for path in "${listed[@]}"; do
      [[ "$path" == "$top" || "$path" == "$top/"* ]] && { covered=1; break; }
    done
    ((covered)) || add_error "CONTRIBUTING.md: Project Layout does not name '$top', a top-level part of this repository [docs/project-layout:unnamed]"
  done < <(git -C "$ROOT" ls-files --cached 2>/dev/null | cut -d/ -f1 | LC_ALL=C sort -u)
}

# The rule docs/skill-pointers (see the header). A pointer is resolved with 'cd -P', which
# every shell has, rather than realpath, whose flags differ between GNU and BSD.
check_skill_pointers() {  # docs/skill-pointers
  local skills="$ROOT/.agents/skills" pointers="$ROOT/.claude/skills"
  local d name p target want
  for d in "$skills"/*/; do
    [[ -f "$d/SKILL.md" ]] || continue
    name="$(basename "$d")"
    awk -v name="$name" 'NR == 1 { if ($0 !~ /^---[ \t]*$/) exit; next }
      /^---[ \t]*$/ { closed = 1; exit }
      /^([ \t#]|-([ \t]|$)|$)/ { next }
      !/^[A-Za-z0-9_-]+:([ \t]|$)/ { exit }
      /^name:/ { v = $0; sub(/^name:[ \t]*/, "", v); gsub(/^["\047]|["\047]?[ \t]*$/, "", v); n = v == name }
      /^description:/ { desc = 1 }
      END { exit !(closed && n && desc) }' "$d/SKILL.md" \
      || add_error ".agents/skills/$name/SKILL.md: no front matter an agent reads — '---' first, a 'name: $name' and a 'description:' line, unindented lines only keys, then '---' (NUC-procedures-are-skills §7) [docs/skill-pointers:frontmatter]"
    p="$pointers/$name"
    if [[ -L "$p" ]]; then
      continue
    elif [[ -e "$p" ]]; then
      add_error ".claude/skills/$name: is a copy of the skill, not a symlink — a second source drifts; replace it: rm -r .claude/skills/$name && ln -s ../../.agents/skills/$name .claude/skills/$name [docs/skill-pointers:copy]"
    else
      add_error ".claude/skills/$name: no pointer — the skill .agents/skills/$name is invisible to Claude Code; add it: ln -s ../../.agents/skills/$name .claude/skills/$name [docs/skill-pointers:missing]"
    fi
  done
  for p in "$pointers"/*; do
    [[ -e "$p" || -L "$p" ]] || continue
    name="$(basename "$p")"
    if [[ ! -L "$p" ]]; then
      # A copy of an existing skill was reported above; what is left is a skill living here only.
      [[ -f "$skills/$name/SKILL.md" ]] \
        || add_error ".claude/skills/$name: is not a symlink into .agents/skills/ — a skill lives there, where every agent reads it, and is pointed at from here (NUC-procedures-are-skills §6) [docs/skill-pointers:stray]"
      continue
    fi
    if ! target="$(cd -P "$p" 2>/dev/null && pwd -P)"; then
      add_error ".claude/skills/$name: dangles — its target does not exist; point it at .agents/skills/$name or delete it [docs/skill-pointers:dangles]"
      continue
    fi
    want="$(cd -P "$skills/$name" 2>/dev/null && pwd -P)"
    [[ -n "$want" && "$target" == "$want" && -f "$want/SKILL.md" ]] \
      || add_error ".claude/skills/$name: points at '${target#"$ROOT"/}', not at the skill .agents/skills/$name [docs/skill-pointers:target]"
  done
}

# The rule docs/base-blobs (see the header), both ways, one message for the template and a
# derived project.
check_base_blobs() {  # docs/base-blobs
  local list="$ADR_DIR/NUC.blobs" f name blob fix out
  fix="a derived project restores the file of its release and narrows a rule by an ADR of its own; the template rewrites the list: bash -c 'source scripts/lib/adr.sh && adr_blobs docs/adr' > docs/adr/NUC.blobs (NUC-base-is-fixed §2)"
  local -A want=() have=()
  out="$(adr_blobs "$ADR_DIR" 2>&1)" || { add_error "docs/adr/NUC.blobs: no blob identifiers computed: ${out##*$'\n'} [docs/base-blobs:compute]"; return; }
  while read -r blob name; do [[ -n "$name" ]] && have["$name"]="$blob"; done <<< "$out"
  if [[ ! -f "$list" ]]; then
    ((${#have[@]})) || return 0
    # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
    grep -qE '^[[:space:]]*([-*] )?\*\*Template release:\*\*[[:space:]]*[[`]?v[0-9]+\.[0-9]+\.[0-9]+[]`]?(\([^)]*\))?, forked([^[:alnum:]]|$)' \
      "$ROOT/README.md" 2>/dev/null \
      || add_error "docs/adr/NUC.blobs: not found — it names each NUC- file and AGENTS.md with its blob; a fork names the release it forked on the Template release line of README.md, 'vX.Y.Z, forked' (NUC-base-is-fixed §1) [docs/base-blobs:absent]"
    return
  fi
  while read -r blob name; do [[ -n "$name" ]] && want["$name"]="$blob"; done < "$list"
  for name in "${!want[@]}"; do
    [[ -v have["$name"] ]] || add_error "docs/adr/NUC.blobs: names $name, which does not exist — $fix [docs/base-blobs:gone]"
  done
  for name in "${!have[@]}"; do
    if [[ ! -v want["$name"] ]]; then add_error "$name: has no line in docs/adr/NUC.blobs — $fix [docs/base-blobs:unlisted]"
    elif [[ "${want["$name"]}" != "${have["$name"]}" ]]; then add_error "$name: differs from its line in docs/adr/NUC.blobs — $fix [docs/base-blobs:differs]"; fi
  done
}

# The rule docs/template-identity (see the header). Each placeholder is a pattern in one file;
# the file is skipped when it does not exist, so a project that dropped one of them is not
# asked to edit it.
check_template_identity() {  # docs/template-identity
  [[ -f "$ROOT/TEMPLATE-SETUP.md" ]] && return
  # placeholder <file> <grep -E pattern> <message> <reason>
  placeholder() {
    [[ -f "$ROOT/$1" ]] || return 0
    grep -qE "$2" "$ROOT/$1" && add_error "$1: $3 [docs/template-identity:$4]"
  }
  placeholder README.md '^# NUC — an Agentic' "the title is still the template's; give the project its name" title
  placeholder README.md '^\*\*NUC\*\* is named after' "the intro still explains the template's name; say what the project is" intro
  placeholder README.md '^## Why NUC$' "the Why NUC section still presents the template; delete it" why
  placeholder README.md '^Describe what this project does' "the Overview is still the scaffold's sentence; describe the project" overview
  placeholder README.md '^TODO — show a minimal example' "the Usage section is still TODO; show how the project is used, or say there is nothing to use yet" usage
  # Anchored to a badge line: the Template section links the template's repository on purpose,
  # and that link stays. The name ends where a URL goes on, so that a derived repository named
  # after the template, as hivevm/nuc-tools, is not taken for it.
  placeholder README.md '^\[!\[[^]]*\]\([^)]*hivevm/nuc([/)?#&]|\.svg|$)' "a badge still points at the template's repository; repoint or delete it" badge
  placeholder .devcontainer/devcontainer.json '"name": "NUC DevContainer"' "the Dev Container still carries the template's name" container
  placeholder LICENSE '^Copyright\b.*\bMaintainer\b' "the copyright holder is still the placeholder; name the maintainer" copyright
  placeholder SECURITY.md 'TODO: add a security contact' "the security contact is still the placeholder" security
  placeholder CODE_OF_CONDUCT.md 'TODO: add a contact address' "the reporting contact is still the placeholder" conduct
  placeholder .github/CODEOWNERS '^# TODO: add a code owner' "the instruction to add a code owner is still there; remove it once the rules are active" instruction
  if [[ -f "$ROOT/.github/CODEOWNERS" ]] && ! grep -qE '^[^#[:space:]]' "$ROOT/.github/CODEOWNERS"; then
    add_error ".github/CODEOWNERS: no active rule; name the code owner and uncomment the two rules [docs/template-identity:codeowners]"
  fi
  local doc
  for doc in SPECIFICATION ARCHITECTURE GLOSSARY CONVENTIONS; do
    placeholder "docs/$doc.md" '^# .*<Project Name>' "the title still carries '<Project Name>'; give the project its name" projectname
  done
  placeholder docs/SPECIFICATION.md '\*\*[GQ]-[0-9]+\*\* — (…|<Quality)' "a criterion is still the scaffold's placeholder; write it, or delete the item" criterion
  placeholder docs/ARCHITECTURE.md '^- \*\*<Part>\*\*' "the scaffold's building block is still listed; describe the parts, or delete the entry" part
  placeholder docs/GLOSSARY.md '^- \*\*<Term>\*\*' "the scaffold's entry is still listed; write the first term, or delete the entry" term
  placeholder docs/CONVENTIONS.md '^- \*\*<Convention>\*\*' "the scaffold's entry is still listed; write the first convention, or delete the entry" convention
  placeholder README.md '^- \*\*(Build|Test|Lint|Run):\*\* TODO' "a Build, Test, Lint, or Run command is still TODO; name the command the toolchain ADR chose" command
  [[ -f "$ROOT/.github/ISSUE_TEMPLATE/template-finding.md" ]] \
    && add_error ".github/ISSUE_TEMPLATE/template-finding.md: the template's form for findings about itself is still here; delete it, since a finding goes to the template's repository (NUC-template-learns-from-projects §6) [docs/template-identity:finding]"
  local role n f
  for role in "${ADR_ROLES[@]}"; do
    n=0
    for f in "${ADR_LIST[@]}"; do
      adr_field_value "${ADR_REL["Role $f"]-}" Role && trim "$ADR_VALUE"
      [[ "$TRIMMED" == "$role" && "${ADR_STATUS["$f"]-}" == *🟢* ]] && n=$((n + 1))
    done
    ((n == 0)) && add_error "docs/adr/: no accepted ADR fills the role $role — one carries '- **Role:** $role' (NUC-base-is-fixed §3) [docs/template-identity:unfilled]"
    ((n > 1)) && add_error "docs/adr/: $n accepted ADRs fill the role $role — exactly one does (NUC-base-is-fixed §3) [docs/template-identity:several]"
  done
}

check_opens_no_issue() {  # docs/opens-no-issue
  [[ -f "$ROOT/TEMPLATE-SETUP.md" ]] || return 0
  local f files=() file line reason
  for f in "${TEXT_FILES[@]}"; do
    case "${f#"$ROOT"/}" in
      scripts/fixtures/*) ;;
      scripts/* | .githooks/* | .github/* | .agents/*) files+=("$f") ;;
    esac
  done
  ((${#files[@]})) || return 0
  # One awk pass: a command is a line and the lines its trailing backslashes join to it, named by
  # its first line; the first form it matches is its reason.
  while IFS=$'\t' read -r file line reason; do
    add_error "${file#"$ROOT"/}:$line: opens an issue on its own ($reason) — only a human sends a finding, and no file of the template sends anything (NUC-template-learns-from-projects §4) [docs/opens-no-issue:$reason]"
  done < <(awk '
    function word(w) { return "(^|[^[:alnum:]_-])" w "([^[:alnum:]_-]|$)" }
    function form(c) {
      if (c ~ word("gh[[:space:]]+issue[[:space:]]+(create|new)")) return "cli"
      if (c ~ word("gh[[:space:]]+api") && c ~ path && c !~ get && (c ~ post || c ~ field)) return "api"
      if (c ~ /(^|[^[:alnum:]_])createIssue[[:space:]]*[(]/) return "graphql"
      if (c ~ word("(curl|wget)") && c ~ path && (c ~ post || c ~ data)) return "http"
      if (c ~ /(^|[^[:alnum:]_])issues\.create[[:space:]]*[(]/ || c ~ /uses:[[:space:]]*[^[:space:]]*create-(an-)?issue/) return "script"
      return ""
    }
    BEGIN {
      path = "repos/[^[:space:]]*/issues([[:space:]\"\047`]|$)"
      get = "(-X|--method|--request)[[:space:]=]*GET"
      post = "(-X|--method|--request)[[:space:]=]*POST"
      field = "[[:space:]](-f|-F|--field|--raw-field|--input)([[:space:]=]|$)"
      data = "[[:space:]](-d|--data[a-z-]*|--json|--post-data|--post-file)([[:space:]=]|$)"
    }
    FNR == 1 { cmd = "" }
    {
      if (cmd == "") { if ($0 ~ /^[[:space:]]*#/) next; at = FNR }
      if (/\\$/) { cmd = cmd substr($0, 1, length($0) - 1) " "; next }
      c = cmd $0; cmd = ""
      r = form(c)
      if (r != "") printf "%s\t%s\t%s\n", FILENAME, at, r
    }
  ' "${files[@]}")
}

# LINK_SPACE_RE — a space inside the text of a link; docs/prose-width reads a link as one word.
LINK_SPACE_RE='(\[[^] ]*) ([^]]*\]\()'

# columns <text> — sets COLUMNS_OF to the characters of the text: its bytes less the UTF-8
# continuation bytes, so a bash that counts UTF-16 units, as Git Bash does, counts an emoji once.
columns() {
  local LC_ALL=C cont
  cont="${1//[!$'\x80'-$'\xbf']/}"
  COLUMNS_OF=$((${#1} - ${#cont}))
}

# The rule docs/prose-width (see the header). Characters are counted, not bytes, so an em dash
# is one column; scan_markdown hands over only the lines longer than 100 bytes outside front
# matter and fences.
check_prose_width() {  # docs/prose-width
  local f rel n line indent body in_adr attr COLUMNS_OF
  local LC_ALL=C.UTF-8
  # A file git marks as generated is written by a tool, not wrapped by hand; asked once for all.
  local -A generated=()
  while IFS= read -r attr; do
    case "$attr" in
      *": linguist-generated: set" | *": linguist-generated: true")
        generated["${attr%: linguist-generated: *}"]=1 ;;
    esac
  done < <(for f in "${!MD_WIDE[@]}"; do printf '%s\n' "${f#"$ROOT"/}"; done \
             | git -C "$ROOT" check-attr --stdin linguist-generated 2>/dev/null)
  for f in "${TEXT_FILES[@]}"; do
    [[ -v MD_WIDE["$f"] ]] || continue
    is_inactive_adr "$f" && continue
    rel="${f#"$ROOT"/}"
    [[ -v generated["$rel"] ]] && continue
    in_adr=0
    [[ "$f" == "$ADR_DIR"/* ]] && in_adr=1
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      n="${line%%:*}"
      line="${line#*:}"
      columns "$line"
      ((COLUMNS_OF > 100)) || continue
      # A table row, a heading, and an ADR header field are one line by their syntax.
      [[ "$line" =~ ^[[:space:]]*\| || "$line" =~ ^# ]] && continue
      [[ in_adr -eq 1 && "$line" =~ $ADR_FIELD_ITEM_RE ]] && continue
      # A link is one word, whatever its text holds. A line without a space outside its links
      # cannot break; any other line can.
      indent="${line%%[! ]*}"
      body="${line:${#indent}}"
      while [[ "$body" =~ $LINK_SPACE_RE ]]; do
        body="${body/"${BASH_REMATCH[0]}"/"${BASH_REMATCH[1]}_${BASH_REMATCH[2]}"}"
      done
      [[ "$body" == *" "* ]] \
        && add_error "$rel:$n: $COLUMNS_OF columns — wrap the prose at 100 (a link that does not fit stands on a line of its own) [docs/prose-width:wide]"
    done <<< "${MD_WIDE["$f"]}"
  done
}

# The rule docs/template-release (see the header). The first matching line is read; a Markdown
# list marker before the bold label is allowed, the version may be written as a link or in
# backticks, and what follows it — a sentence, a link target — is not read. After the version
# only a space, a punctuation mark, or the end of the line may follow: a pre-release suffix,
# build metadata, and a fourth number are not a tag NUC-releases-are-semver-tags §1 cuts,
# and SemVer allows no leading zero.
# release_of <text> — sets RELEASE to what the first Template release line of the text names,
# 'vX.Y.Z' or 'unreleased'; to nothing when the text has no such line, and to '-' when its line
# names neither.
RELEASE=""
release_of() {
  local line num='(0|[1-9][0-9]*)' re
  # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
  re='^[[`]?(v'"$num"'\.'"$num"'\.'"$num"'|unreleased)($|[][:space:],;:)`]|\.([[:space:]]|$))'
  RELEASE=""
  line="$(grep -m1 -E '^[[:space:]]*([-*] )?\*\*Template release:\*\*' <<< "$1")" || return 0
  line="${line#*\*\*Template release:\*\*}" line="${line#"${line%%[![:space:]]*}"}"
  if [[ "$line" =~ $re ]]; then RELEASE="${BASH_REMATCH[1]}"; else RELEASE=-; fi
}

check_template_release() {  # docs/template-release
  local readme="$ROOT/README.md"
  if [[ ! -f "$readme" ]]; then
    add_error "README.md: not found — the Template section names the release this repository carries (NUC-releases-are-semver-tags §4) [docs/template-release:absent]"
    return
  fi
  release_of "$(< "$readme")"
  if [[ -z "$RELEASE" ]]; then
    add_error "README.md: no '**Template release:**' line — the Template section names the release of the template this repository carries, or 'unreleased' (NUC-releases-are-semver-tags §4) [docs/template-release:line]"
    return
  fi
  if [[ "$RELEASE" == - ]]; then
    add_error "README.md: the Template release line names neither a 'vX.Y.Z' tag nor 'unreleased' — a release moves it to the tag it cuts (NUC-releases-are-semver-tags §1, §4) [docs/template-release:version]"
    return
  fi
  # A change that moves the version against the merge base with main cuts a release, or takes
  # one up: it carries no proposed NUC- ADR (NUC-releases-are-semver-tags §8). A base without
  # the line, or with one that names no version, counts as none. Without a merge base the line
  # is read alone.
  local now="$RELEASE" base was f
  base="$(main_merge_base HEAD 2>/dev/null)" || return 0
  release_of "$(git show "$base:README.md" 2>/dev/null)"
  [[ "$RELEASE" == "$now" ]] && return 0
  was="$RELEASE"
  [[ -n "$was" && "$was" != - ]] || was=none
  for f in "${ADR_LIST[@]}"; do
    [[ "${f##*/}" == NUC-* && "${ADR_STATUS["$f"]-}" == *🟡* ]] || continue
    add_error "${f#"$ROOT"/}: proposed, and the change moves the Template release line from $was to $now — a release carries only accepted NUC- ADRs (NUC-releases-are-semver-tags §8) [docs/template-release:proposed]"
  done
}

# heading_texts <file> — print the text of every ATX heading outside a fenced code block.
heading_texts() {
  awk '
    /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
    !fence && match($0, /^#+[[:space:]]+/) {
      hashes = $0; sub(/[^#].*$/, "", hashes)
      if (length(hashes) > 6) next
      text = substr($0, RLENGTH + 1)
      sub(/[[:space:]]+#*[[:space:]]*$/, "", text)
      print text
    }
  ' "$1"
}

# The rule docs/named-sections (see the header). One awk pass over every text file that mentions either document
# prints each reference as file, line, the '../' before the name, the name, and the heading. A
# line is read joined to the next, whose comment marker is dropped, and a match counts on the line
# it starts on: a reference wrapped over two lines is read once, and whole. HEADING_TEXTS holds
# the headings of each document, newline-framed, read once.
declare -A HEADING_TEXTS=()
check_named_sections() {  # docs/named-sections
  local files=() f
  for f in "${TEXT_FILES[@]}"; do is_inactive_adr "$f" || files+=("$f"); done
  ((${#files[@]})) || return 0
  mapfile -t files < <(grep -lE 'README|CONTRIBUTING' -- "${files[@]}" 2>/dev/null)
  ((${#files[@]})) || return 0
  local file lineno up name heading rel doc
  while IFS=$'\x1f' read -r file lineno up name heading; do
    # '<Heading>' is the placeholder a sentence about these references writes, as '<CAT>' is
    # for an ADR's category (docs/links).
    [[ "$heading" == '<'*'>' ]] && continue
    rel="${file#"$ROOT"/}"
    f="${file%/*}"
    if [[ -n "$up" ]]; then
      doc="$f/$up$name.md"
      [[ -f "$doc" ]] && doc="$(cd "${doc%/*}" && pwd)/$name.md"
    elif [[ -f "$f/$name.md" ]]; then
      doc="$f/$name.md"
    else
      doc="$ROOT/$name.md"
    fi
    if [[ ! -f "$doc" ]]; then
      add_error "$rel:$lineno: \"$heading\" names a section of $up$name.md, which does not exist [docs/named-sections:document]"
      continue
    fi
    [[ -v HEADING_TEXTS["$doc"] ]] || HEADING_TEXTS["$doc"]=$'\n'"$(heading_texts "$doc")"$'\n'
    [[ "${HEADING_TEXTS["$doc"]}" == *$'\n'"$heading"$'\n'* ]] \
      || add_error "$rel:$lineno: \"$heading\" names no heading of ${doc#"$ROOT"/} [docs/named-sections:heading]"
  done < <(awk '
    BEGIN {
      doc = "(\\.\\./)*(README|CONTRIBUTING)(\\.md)?`?"
      arrow = "(^|[^A-Za-z0-9_./-])`?" doc "[[:space:]]*→[[:space:]]*\"[^\"]*\""
    }
    function refs(line, joined, file, n,   rest, off, m, d, h, up) {
      rest = joined; off = 0
      while (match(rest, arrow)) {
        if (off + RSTART > length(line)) return
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        d = m; sub(/[[:space:]]*→.*$/, "", d)
        h = m; sub(/^[^"]*"/, "", h); sub(/"$/, "", h)
        gsub(/`/, "", d)
        match(d, /(\.\.\/)*(README|CONTRIBUTING)(\.md)?$/); d = substr(d, RSTART)
        up = d; sub(/(README|CONTRIBUTING)(\.md)?$/, "", up)
        sub(/^(\.\.\/)*/, "", d); sub(/\.md$/, "", d)
        printf "%s\037%s\037%s\037%s\037%s\n", file, n, up, d, h
      }
    }
    function scan(line, following,   joined) {
      sub(/[[:space:]]+$/, "", line)
      sub(/^[[:space:]]*(\/\/[\/!]?|#+|--|\*+|>+)?[[:space:]]*/, "", following)
      joined = line " " following
      refs(line, joined, file, n)
    }
    FNR == 1 && NR > 1 { scan(prev, "") }
    { if (FNR > 1) scan(prev, $0); prev = $0; file = FILENAME; n = FNR }
    END { if (NR) scan(prev, "") }
  ' "${files[@]}")
}

# The rule docs/clause-citations (see the header). DECISION_CLAUSES holds, for each ADR's identifier, the numbers of its
# Decision's numbered items, space-framed, outside fenced code (ADR_FENCE_AWK), read once. One awk pass over
# every file that names an ADR prints each citation as file, line, the key, the word cited by
# ('clause' or a place), and its numbers, a range as 'N-M'; a line is read joined to the next, as
# in docs/named-sections.
declare -A DECISION_CLAUSES=()
check_clause_citations() {  # docs/clause-citations
  local files=() f
  for f in "${TEXT_FILES[@]}"; do
    [[ "$f" == "$ADR_DIR/template.md" ]] && continue
    is_inactive_adr "$f" || files+=("$f")
  done
  ((${#files[@]})) || return 0
  local cats
  cats="$(IFS='|'; echo "${ADR_CATEGORIES[*]}")"
  mapfile -t files < <(grep -lE -- "($cats)-[a-z0-9]" "${files[@]}" 2>/dev/null)
  ((${#files[@]})) || return 0
  if ((${#ADR_LIST[@]})); then
    while IFS=$'\x1f' read -r f n; do
      adr_key_of "$f"
      DECISION_CLAUSES["$ADR_ID"]="$n"
    done < <(awk "$ADR_FENCE_AWK"'
      FNR == 1 { if (NR > 1) printf "%s\037%s\n", file, n; file = FILENAME; n = " "; d = 0; fence_c = "" }
      { f = fenced($0) }
      !f && /^## / { d = ($0 ~ /^## Decision[[:space:]]*$/); next }
      d && !f && /^[0-9]+\. / { k = $1; sub(/\.$/, "", k); n = n (k + 0) " " }
      END { if (NR) printf "%s\037%s\n", file, n }
    ' "${ADR_LIST[@]}")
  fi
  local file lineno key word numbers rel target n cited end low high have items
  while IFS=$'\x1f' read -r file lineno key word numbers; do
    rel="${file#"$ROOT"/}"
    target="${ADR_BY_ID["$key"]-}"
    [[ -n "$target" ]] || continue
    if [[ "$word" != clause ]]; then
      # The base writes its own text, and a project never edits it (NUC-base-is-fixed §1).
      [[ "$rel" == CHANGELOG.md || "$key" == NUC-* ]] \
        || add_error "$rel:$lineno: $key is cited by $word, a place a normalization cannot follow — cite its clause as '$key §N' [docs/clause-citations:place]"
      continue
    fi
    if is_inactive_adr "$target" && [[ "$file" != "$ADR_DIR"/* && "$rel" != CHANGELOG.md ]]; then
      add_error "$rel:$lineno: $key is no longer in force — cite the clause of the ADR that is [docs/clause-citations:inactive]"
      continue
    fi
    have="${DECISION_CLAUSES["$key"]- }"
    read -r -a items <<< "$have"
    n=${#items[@]}
    # A Decision that numbers no clauses, as the fixtures of the template's checks write one, is
    # not held; every ADR of the record numbers its clauses (NUC-adrs-are-named-by-category §3).
    ((n > 0)) || continue
    for cited in $numbers; do
      low="${cited%-*}" high="${cited#*-}"
      for end in "$low" "$high"; do
        if [[ "$have" != *" $((10#$end)) "* ]]; then
          add_error "$rel:$lineno: $key §$end is cited, and its Decision has $n [docs/clause-citations:beyond]"
          [[ "$low" == "$high" ]] && break
        fi
      done
    done
  done < <(awk -v cats="$cats" '
    BEGIN {
      # An identifier of a known category; docs/adr-refs holds what it names.
      key = "(" cats ")-[a-z0-9]+(-[a-z0-9]+)+"
      # A separator is spelled out, never bare space, and no starred term stands inside a repeated
      # group: mawk then matches nothing.
      sep = "(,[[:space:]]|,[[:space:]]and[[:space:]]|,[[:space:]]or[[:space:]]|[[:space:]]and[[:space:]]"
      sep = sep "|[[:space:]]or[[:space:]]|[[:space:]]to[[:space:]]|[[:space:]]?\342\200\223[[:space:]]?)"
      list = "[0-9]+(" sep "[0-9]+)*"
      # The clause form: the identifier, a space, and the section sign before each number.
      sign = "\302\247"
      clause = key "[[:space:]]" sign "[0-9]+((,[[:space:]]|,[[:space:]]and[[:space:]]|[[:space:]]and[[:space:]]|\342\200\223)" sign "[0-9]+)*"
      # The words a citation names a place by, the older word clause among them, and the section sign
      # after a link, which leaves the link text of the identifier.
      word = "([Cc]lauses?|[Pp]oints?|[Pp]aragraphs?|[Ll]ines?|[Rr]ules?|[Ii]tems?|[Bb]ullets?"
      word = word "|[Ss]teps?|[Ss]ections?)"
      after = key "(\\]\\([^)]+\\))?,?[[:space:]]+\\(?" word "[[:space:]]?" list
      linked = key "\\]\\([^)]+\\),?[[:space:]]+\\(?" sign "[[:space:]]?" list
      before = "[Cc]lauses?[[:space:]]+" list "[[:space:]]+of[[:space:]]+\\[?" key
    }
    # numbers <text> — the numbers of a list, each range as N-M, joined by spaces.
    function numbers(text,   out) {
      gsub(sign, "", text)
      gsub(/[[:space:]]*(to|\342\200\223)[[:space:]]*/, "-", text)
      gsub(/,|and|or/, " ", text)
      out = ""
      while (match(text, /[0-9]+(-[0-9]+)?/)) {
        out = out (out == "" ? "" : " ") substr(text, RSTART, RLENGTH)
        text = substr(text, RSTART + RLENGTH)
      }
      return out
    }
    function emit(id, w, list) {
      printf "%s\037%s\037%s\037%s\037%s\n", file, n, id, w, numbers(list)
    }
    # whole <list> <rest> — the list without its last number where the text goes on as a number
    # does, as a date after a comma would: "clause 2, 2026-11-14" cites clause 2.
    function whole(list, rest) {
      if (rest ~ /^[-\/0-9]/) sub(/[^0-9]+[0-9]+$/, "", list)
      return list
    }
    function cites(line, joined,   rest, off, m, id, w, list) {
      rest = joined; off = 0
      while (match(rest, clause)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        list = substr(m, RSTART + RLENGTH)
        emit(id, "clause", list)
      }
      rest = joined; off = 0
      while (match(rest, linked)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        emit(id, "the section sign after its link", "")
      }
      rest = joined; off = 0
      while (match(rest, after)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        # The word and the list follow the identifier, its link and its comma.
        w = m; sub(/^[A-Z]+-[^[:space:]]*[[:space:]]+\(?/, "", w)
        list = w; sub(/^[^0-9]+/, "", list)
        sub(/[[:space:]0-9].*$/, "", w); w = tolower(w); sub(/s$/, "", w)
        if (w == "clause") w = "the older word clause"
        emit(id, w, whole(list, rest))
      }
      rest = joined; off = 0
      while (match(rest, before)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        emit(id, "clause N of", "")
      }
    }
    # A line ending in a backslash, as a Rust string continues, joins as any other.
    function scan(line, following) {
      sub(/[[:space:]]*\\?[[:space:]]*$/, "", line)
      sub(/^[[:space:]]*(\/\/[\/!]?|#+|--|\*+|>+)?[[:space:]]*/, "", following)
      cites(line, line " " following)
    }
    FNR == 1 && NR > 1 { scan(prev, "") }
    { if (FNR > 1) scan(prev, $0); prev = $0; file = FILENAME; n = FNR }
    END { if (NR) scan(prev, "") }
  ' "${files[@]}")
}

collect_text_files
read_adr_headers
scan_markdown
check_adr_index
check_adr_identifiers
check_adr_headers
check_adr_supersessions
check_relative_links
check_section_refs
check_adr_refs
check_adr_link_targets
check_check_lists
check_project_layout
check_skill_pointers
check_base_blobs
check_template_identity
check_opens_no_issue
check_template_release
check_prose_width
check_named_sections
check_clause_citations

if ((${#errors[@]} > 0)); then
  echo "Documentation checks FAILED:"
  echo
  for e in "${errors[@]}"; do echo "  - $e"; done
  echo
  echo "${#errors[@]} problem(s) found."
  exit 1
fi

echo "Documentation checks passed."
exit 0
