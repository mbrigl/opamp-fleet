---
name: normalize
description: Restate the specification and rewrite the ADRs in force as a new series, dead ADRs deleted and references moved by a map. Use only on a human's instruction to normalize, consolidate, or renumber the record, or when an existing project takes up this template or a template release.
---

# Normalize the project

The rule is
[NUC-decisions-are-proposed-then-accepted §8](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md),
with
[NUC-decisions-are-proposed-then-accepted §3](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)
and [NUC-documents-are-fixed §2](../../../docs/adr/NUC-documents-are-fixed.md); the mechanics of a
normalization are in [`docs/adr/README.md`](../../../docs/adr/README.md#mechanics). This skill is
the order of work and states no rule of its own
([NUC-procedures-are-skills §3](../../../docs/adr/NUC-procedures-are-skills.md)).

## 1. Entry point and inputs

Run in a fresh session. Name the entry point with the human:

- **A normalization** of this project's own record, the template's included. The inputs are the
  specification, the ADRs in force, and the superseded and rejected ones that explain them.
- **An adoption:** an existing project takes up the template. Copy the template's files into the
  tree first, without overwriting a file of the project; a file both have is merged by hand. Then
  walk step 1 of `TEMPLATE-SETUP.md`, which comes with the copy, for the identity. The inputs
  add the project's existing record, wherever and in whatever form it lives, its documents, and
  the decisions its code shows and no record holds.
- **A template release:** a derived project takes up a later release. A take-up is no
  normalization: walk [section 7](#7-take-up-a-template-release) instead of the sections between.

An adoption or a template release brings the template's git hooks and its repository settings
with its files:

- **Before the copy,** a local `main` with commits the remote's lacks moves them to a `feat/`
  branch: the hooks keep `main` to what `origin/main` has
  ([NUC-main-only-via-pr §4](../../../docs/adr/NUC-main-only-via-pr.md)).
- **Once the files are in,** every clone enables the hooks with the merge settings they rely on, as
  [Git hooks](../../../CONTRIBUTING.md#git-hooks) in the contributing guide says; a Dev Container
  rebuilt from the new configuration does it on every start.
- **Before the take-up reaches `main`,** a maintainer walks
  [Repository settings](../../../CONTRIBUTING.md#repository-settings) against the GitHub repository:
  for an adoption all of it, for a release what changed in the template's section between the
  two tags, such as the merge method or a required check.
  [`scripts/sensor-repo-settings.sh`](../../../scripts/sensor-repo-settings.sh), run on the
  maintainer's instruction ([NUC-checks-are-bash §7](../../../docs/adr/NUC-checks-are-bash.md)),
  reports what differs once the new contributing guide is in.
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
   map outside the tree or under `.scratch/`; its header gives the form of the lines. Rewrite by
   hand what it lists as left, and the citations noted in the step before.
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

Propose the new record to the human before writing it, as a table: identifier, title, the old
ADRs it restates, and the group it belongs to. An ADR restated alone keeps its identifier; a merged
or split one takes a new identifier. Order it as the mechanics say. Point out
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
4. Write the ADR map, one line per old identifier, `OLD NEW`: a dead ADR maps to its final
   successor, or to `-` where it has none, and a split names each successor. A successor that is
   itself mapped is refused, so a map has no chain. Check it with `remap.sh --check`, apply it
   with `remap.sh`, and rewrite by hand what it lists as left; `--check` then reports nothing.
5. Make the tests cite the new identifiers where `remap.sh` did not reach, and update
   [`docs/ARCHITECTURE.md`](../../../docs/ARCHITECTURE.md) where the cut changed what it cites.
6. Run `scripts/check-all.sh`.

## 6. Review and hand-off

Hand the change to the [`review`](../review/SKILL.md) skill with the old record at the base
commit, the map, and the clause table: the reviewer walks the table, reads each clause against the
new ADR that carries it, and reports any decision that moved and any clause the table misses.
Commit with the map in the commit body, and hand the branch to the human with both maps, the
cut, the clause table, and the review in its hand-off. One pull request lands the branch
([NUC-main-only-via-pr §4](../../../docs/adr/NUC-main-only-via-pr.md)).

In the template, a release that merges, splits, or removes a `NUC-` ADR is a major, and its notes
carry the map
([NUC-releases-are-semver-tags §7](../../../docs/adr/NUC-releases-are-semver-tags.md)).

## 7. Take up a template release

A derived project takes up a later release in the steps of
[NUC-releases-are-semver-tags §6](../../../docs/adr/NUC-releases-are-semver-tags.md). The project's
own ADRs stay as they are; no inherited ADR is merged into its record. Work on a branch of its own.

1. **Inputs.** Name with the human the tag the `Template release` line of
   [`README.md`](../../../README.md) names and the tag to take up. Read the release notes of every
   release between the two. Keep for step 5 each map of a re-cut a note carries
   ([NUC-releases-are-semver-tags §7](../../../docs/adr/NUC-releases-are-semver-tags.md)).
   A project on a release before `v0.9.0` still has numbered ADRs and migrates its record once:
   walk [section 8](#8-migrate-a-numbered-record) in place of step 2, and take the other steps as
   they stand.
2. **The base.** Replace the `NUC-` files under `docs/adr/`, `AGENTS.md`, and the blob list
   ([NUC-base-is-fixed §2](../../../docs/adr/NUC-base-is-fixed.md)) with those
   at the new tag, for example by
   `git checkout <tag> -- 'docs/adr/NUC-*' AGENTS.md docs/adr/NUC.blobs` from a remote that
   carries the template's tags. Delete a `NUC-` file the new tag no longer has. Change none of
   them beyond the copy; a project narrows a rule in an ADR of its own
   ([NUC-base-is-fixed §1](../../../docs/adr/NUC-base-is-fixed.md)).
3. **The other files.** Take up each other file the notes name. Where the project changed the file
   too, merge the two in the way the project chose. The git hooks and the repository settings
   follow as [section 1](#1-entry-point-and-inputs) says.
4. **The release line.** Move the `Template release` line to the new tag in the same change
   ([NUC-releases-are-semver-tags §5](../../../docs/adr/NUC-releases-are-semver-tags.md)).
5. **The checks.** Run [`scripts/check-all.sh`](../../../scripts/check-all.sh) and fix the
   project's own files until it passes. Where a map from step 1 moves a clause the project cites,
   list the citations with `git grep -nwE '<old identifiers>'` and move each as the map says, in
   accepted ADRs too
   ([NUC-decisions-are-proposed-then-accepted §6](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
   Where the cited clause changed its meaning, hand the ADR to the human for a supersession instead.
6. **Hand-off.** Hand the branch to the human with the two tags and, for a re-cut, the citations
   that moved. One pull request lands it
   ([NUC-main-only-via-pr §4](../../../docs/adr/NUC-main-only-via-pr.md)).

## 8. Migrate a numbered record

A project on a release before `v0.9.0` carries numbered ADRs: `0001` to `0006` inherited from the
template, and its own from `0007` on. The take-up of `v0.9.0` or a later release moves them once to
the fixed base and to identifiers
([NUC-adrs-are-named-by-category §1](../../../docs/adr/NUC-adrs-are-named-by-category.md) and
[NUC-base-is-fixed §1](../../../docs/adr/NUC-base-is-fixed.md)). The migration is
a normalization of identifiers only
([NUC-decisions-are-proposed-then-accepted §8](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)),
which covers its edits to accepted project ADRs: the renames, the headings, the `Status` and
`Supersedes` fields, the `Role` lines, and the rewraps. The release's map is
[`map-v0.9.0.txt`](map-v0.9.0.txt): the six inherited ADRs to the `NUC-` ADRs, and a table of every
clause with the `NUC-` clause that carries it.

1. **What the project added.** List what the project wrote into an inherited ADR, with
   `git diff <tag of the release line> HEAD -- 'docs/adr/000[1-6]-*.md'` against the template's
   tag. A decider it added goes with the file. Note for step 5 each clause it added, and each rule
   it added to `AGENTS.md`, from `git diff <tag of the release line> HEAD -- AGENTS.md`.
2. **The base.** Delete the inherited files `docs/adr/0001-*.md` to `docs/adr/0006-*.md`, and copy
   the `NUC-` files, `AGENTS.md`, and the blob list as step 2 of
   [section 7](#7-take-up-a-template-release) says
   ([NUC-base-is-fixed §2](../../../docs/adr/NUC-base-is-fixed.md)).
3. **The project map.** Write a map under `.scratch/`, one line per numbered ADR of the project,
   superseded and rejected ones included, since the migration keeps every ADR
   ([NUC-decisions-are-proposed-then-accepted §8](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)),
   in the form `remap.sh` names in its header: `0007-old-title.md <CAT>-<slug>`. Pick each category
   from the fixed ones or add it to the category list of the index
   ([NUC-adrs-are-named-by-category §2](../../../docs/adr/NUC-adrs-are-named-by-category.md)), and
   pick a slug that states the decision
   ([NUC-adrs-are-named-by-category §1](../../../docs/adr/NUC-adrs-are-named-by-category.md)). Hand
   the map to the human, who reviews it before anything moves.
4. **The index.** Rewrite the index as the release's index has it: the category list, the line
   on the base, and the project's rows by category, without the inherited ADRs
   ([NUC-adrs-are-named-by-category §5](../../../docs/adr/NUC-adrs-are-named-by-category.md)). The
   rows may keep their numbered links; step 8 moves them.
5. **The added clauses.** Turn each clause and rule from step 1 into a project ADR through
   [`propose-adr`](../propose-adr/SKILL.md). Its `Extends:` or `Narrows:` field names the `NUC-`
   clause that carries the inherited clause
   ([NUC-base-is-fixed §1](../../../docs/adr/NUC-base-is-fixed.md)). A clause that
   contradicts the base is a fork, which the human decides.
6. **The roles.** Give the project ADR that records the harness the header field
   `- **Role:** harness`, and the one that records the toolchain `- **Role:** toolchain`
   ([NUC-base-is-fixed §3](../../../docs/adr/NUC-base-is-fixed.md)).
7. **The other files.** Take up the other files and move the release line, as steps 3 and 4 of
   [section 7](#7-take-up-a-template-release) say, so that only the project's own files are left
   for the maps.
8. **The references.** Run `remap.sh --record --check` with the release's map and then with the
   project map, and read what each would move, rename, and leave. Then apply both, the release's
   map first. With `--record` the script also renames each numbered file to `<CAT>-<slug>.md` and
   moves its heading, its `Status` and `Supersedes` fields, its links, and its index row; it never
   edits a `NUC-` file.
   - Place each reference a split leaves at the `NUC-` clause that carries what it cites, from the
     clause table of the release's map, in accepted ADRs too
     ([NUC-decisions-are-proposed-then-accepted §6](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
   - A reference that names something other than this record's ADR, such as a message copied from
     an older release or a file of another repository, keeps its old number. The script cannot
     tell it apart, so revert each such move by hand after the run.
   - Rewrap each line that a longer identifier pushed past the width
     [`scripts/check-docs.sh`](../../../scripts/check-docs.sh) holds, in the record too.
9. **The checks.** Go on with step 5 of [section 7](#7-take-up-a-template-release), and hand both
   maps to the human with the branch in its step 6.
