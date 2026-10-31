---
name: normalize
description: Normalize the project on a human's instruction — restate the specification, then rewrite the ADRs in force as a new series in reading order, inherited ones included, with superseded and rejected ADRs deleted and every reference moved by a map. Use when the human asks to normalize, consolidate, or renumber the record, when an existing project takes up this template, or when a derived project takes up a template release.
---

# Normalize the project

The rule is rule 5 of [`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules), with rules 3 and 7; the
mechanics of a normalization are in [`docs/adr/README.md`](../../../docs/adr/README.md#mechanics).
This skill is the order of work and states no rule of its own
([ADR-0003](../../../docs/adr/0003-procedures-as-skills.md)).

## 1. Entry point and inputs

Run in a fresh session. Name the entry point with the human:

- **A normalization** of this project's own record, the template's included. The inputs are the
  specification, the ADRs in force, and the superseded and rejected ones that explain them.
- **An adoption:** an existing project takes up the template. Copy the template's files into the
  tree first, without overwriting a file of the project; a file both have is merged by hand. Then
  walk step 1 of `TEMPLATE-SETUP.md`, which comes with the copy, for the identity. The inputs
  add the project's existing record, wherever and in whatever form it lives, its documents, and
  the decisions its code shows and no record holds.
- **A template release:** a derived project takes up a later release. The inputs add the
  template's ADRs and specification scaffold at the tag the `Template release` line of
  [`README.md`](../../../README.md) names and at the new tag, and the release notes. What changed
  between the two tags is what enters. A template ADR the release corrected in place enters by
  the normalization, a correction that changes a decision as one of its
  [supersessions](#3-the-supersessions), never as an edit of an accepted ADR outside a normalization
  ([ADR-0005](../../../docs/adr/0005-template-releases.md)). Where the project has normalized
  before, the maps in the commit bodies of its normalizations, read in order, say which of its
  ADRs restates which template ADR. `AGENTS.md` is taken up as the release has it, except the
  ADR rules of [§3](../../../AGENTS.md#3-adr-rules) that an ADR of the project on the ADR
  process changed: those keep the project's wording, as its opening note says.

An adoption or a template release brings the template's git hooks and its repository settings
with its files:

- **Before the copy,** a local `main` with commits the remote's lacks moves them to a `feat/`
  branch: the hooks keep `main` to what `origin/main` has
  ([`AGENTS.md` §6](../../../AGENTS.md#6-project-rules)).
- **Once the files are in,** every clone enables the hooks with the merge settings they rely
  on, as [Git hooks](../../../README.md#git-hooks) in the README says; a Dev Container rebuilt
  from the new configuration does it on every start.
- **Before the take-up reaches `main`,** a maintainer walks
  [Repository settings](../../../README.md#repository-settings) against the GitHub repository:
  for an adoption all of it, for a release what changed in the template's section between the
  two tags, such as the merge method or a required check.
  [`scripts/sensor-repo-settings.sh`](../../../scripts/sensor-repo-settings.sh), run on the
  maintainer's instruction ([ADR-0002](../../../docs/adr/0002-decisions-verified-by-tests.md)),
  reports what differs once the new README is in.
  A ruleset that still requires a check the workflow no longer runs blocks the merge; one that
  lacks a new check lets the merge through while that check fails.

Confirm the preconditions of the mechanics before anything is written. When the last
normalization is recent, say so before starting. Report the inventory to the human: the ADRs in
force, the dead ones and their final successors, the criteria, and, for an adoption, each
decision found outside a record with where it was found.

## 2. The specification

1. Restate the specification with the human, one section at a time in the order of the file,
   each drafted, shown, and written once confirmed. What no longer holds goes, criteria are
   sharpened, and `G-n` and `Q-n` run from 1 in the order listed.
2. Write the criteria map, one line `OLD NEW` per identifier, `-` for a criterion that goes.
   The map script moves a criterion only in Markdown and on a marker line. Before applying the
   map, list the other citations with `git grep -nwE '<old identifiers>' -- ':!*.md'`, and note
   each that a fixture does not explain.
3. Apply the map with [`remap.sh`](remap.sh): `.agents/skills/normalize/remap.sh <map file>`, the
   map outside the tree or under `.scratch/`. Rewrite by hand what it lists as left, and the
   citations noted in the step before.
4. Name every accepted ADR whose basis the change moves, and why. The human judges that list.
5. Run [`scripts/check-all.sh`](../../../scripts/check-all.sh), commit on the normalization's
   branch, `chore/normalize`, and hand the commit to the human with the map. The supersessions
   and the record follow on the same branch once the human has confirmed it.

## 3. The supersessions

Each ADR the new specification invalidates is superseded through
[`propose-adr`](../propose-adr/SKILL.md), each in commits of ADRs only, before the record
is restated. These are the only decisions that change. The record step begins once the human
has decided each: accepted, with the ADRs it supersedes flipped to superseded, or rejected.

## 4. The cut

Propose the new series to the human before writing a record, as a table: new number, title, the
old ADRs it restates, and the group it belongs to. Order it as the mechanics say. Point out
every merge and every split, and every old ADR that maps to none. Write nothing until the human
has agreed the cut.

## 5. The record

1. For each new ADR, start from [`docs/adr/template.md`](../../../docs/adr/template.md) and
   restate the decisions of its old ADRs against the specification that landed in step 2.
   Every clause of every old ADR is carried into one new ADR, in today's words. Context,
   alternatives, sources, and consequences are rewritten to the state of today. What a
   superseded or rejected ADR chose, and why it did not stand, becomes an alternative of the new
   ADR that governs its topic. The header follows the mechanics.
2. Write the clause table for the hand-off: each clause of each old ADR, in its own words
   shortened to a line, with the new ADR and the section that carry it.
3. Delete the old files, and rewrite both tables of the index.
4. Write the ADR map, one line per old file, `OLD NEW`: a dead ADR maps to the new file of its
   final successor, or to `-` where it has none. Apply it with `remap.sh` and rewrite by hand
   what it lists as left.
5. Make the tests cite the new numbers where `remap.sh` did not reach, and update
   [`docs/ARCHITECTURE.md`](../../../docs/ARCHITECTURE.md) where the cut changed what it cites.
6. Run `scripts/check-all.sh`.

## 6. Review and hand-off

Hand the change to the [`review`](../review/SKILL.md) skill with the old record at the base
commit, the map, and the clause table: the reviewer walks the table, reads each clause against the
new ADR that carries it, and reports any decision that moved and any clause the table misses.
Commit with the map in the commit body, and hand the branch to the human with both maps, the
cut, the clause table, and the review in its hand-off. One pull request lands the branch
([`AGENTS.md` §6](../../../AGENTS.md#6-project-rules)).

In the template, the release that lands a normalization is a major, and its notes carry the map
([ADR-0005](../../../docs/adr/0005-template-releases.md)). In a derived project that took up a
release, the `Template release` line moves to it in the same change.
