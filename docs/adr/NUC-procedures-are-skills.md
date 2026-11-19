# NUC-procedures-are-skills: The procedures of the rule file are Agent Skills under `.agents/skills/`, each carrying a how and no rule of its own

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** `.agents/skills/`, `.claude/skills/`, checks 3, 4, and 11 of `scripts/check-docs.sh`, `AGENTS.md` where it names a procedure, `docs/adr/template.md`, `.github/ISSUE_TEMPLATE/`, and `.github/PULL_REQUEST_TEMPLATE.md`

## Context

[`AGENTS.md`](../../AGENTS.md) states rules. Several of them are procedures: developing an ADR,
planning work larger than one session and working its tickets, reviewing a change, and revising
the design. Each was once written out in the rule file and again where it is used: the ADR
template, the issue templates, the pull request template. The copies drift. Every sharpening
lands in a file that every session reads in full, whether it reviews, plans, or fixes a typo.

Two forces bound the answer. The rule file is the single source of working rules, and no other
document states one. So wherever a procedure moves, it carries the *how* and cites the rule. And
the template is vendor-neutral: a format that one agent reads is worth a pointer, not a home for
process.

Agent Skills are an open specification. A skill is a directory with a `SKILL.md` whose front
matter names it and says when it applies. An agent loads it in full only when a task matches.
Every agent this template names reads skills, and some forty others do. The location is not part
of the specification. Codex, Gemini CLI, and Cursor read `.agents/skills/`. Claude Code reads only
`.claude/skills/`, and the request to read the neutral path is open upstream. Tried on Claude Code
2.1.274: a `.claude/skills` that is itself a symlink loads nothing. A real `.claude/skills/`
directory holding one symlink per skill loads all of them.

## Decision

We will keep the procedures of the rule file as Agent Skills, each carrying a how and no rule.

1. **Skills.** Each procedure of the rule file is a skill: a directory with a `SKILL.md` under
   `.agents/skills/`. The skills are `propose-adr`, `plan-feature`, `work-tickets`, `review`,
   `design-revision`, and `normalize`, which also carries the take-up of a release.
2. **Setup.** While `TEMPLATE-SETUP.md` exists, its walk is the skill `setup-project`. The change
   that deletes the file must delete the skill too.
3. **Content.** A skill carries the steps of one procedure. It must cite the `AGENTS.md` section,
   ADR, or setup step that requires the procedure. It must not state a rule.
4. **Naming.** The rule file and the templates must name the skill instead of its steps.
5. **Files.** A skill may carry reference files that its procedure reads. It may carry a script
   for a step whose outcome depends on the order of its commands. These files must not hold a
   rule. A self-test under `scripts/` must run each script.
6. **Pointer directory.** An agent that reads skills only from another directory reaches them
   through a pointer directory. That directory must hold one symlink per skill, named like the
   skill and resolving to it, and nothing else. A copy or one symlink for the whole directory
   must not replace them. Claude Code reads only `.claude/skills/`, so that directory is a
   pointer directory.
7. **Front matter.** A skill's `SKILL.md` must open with YAML front matter between two `---`
   lines. It must set `name`, on the line of its key, to the skill's directory. It must set a
   `description`.

**Out of scope:**

- whether a skill may run unattended, which its ticket decides by
  [`AGENTS.md` §4](../../AGENTS.md#4-working-style);
- that only the rule file states rules, which [NUC-documents-are-fixed](NUC-documents-are-fixed.md)
  decides.

## Latitude

A project may choose:

- skills of its own for its domain, with their reference files and scripts, in the form of
  clauses 3, 5, and 6;
- the further agents its harness names, per the entry
  [harness](../../.agents/skills/propose-adr/concepts/harness.md) of the catalogue. Clause 6 then
  requires a pointer directory for each that reads skills only from another directory.

The catalogue has no entry for the first choice, and it needs no ADR.

## Alternatives considered

- **Procedures stay in prose.** Four copies drift, and every session pays for the whole rule file.
- **One agent's native commands,** such as Claude Code commands or Cursor rules. They put the
  process in one vendor's format, against the reason for one rule file.
- **A published skills collection or plugin.** It brings a process in one piece, with its own
  artifacts and vocabulary. The template declines the same trade for planning.
- **Procedures as plain files under `docs/`, loaded by pointer.** They offer no progressive
  disclosure and no invocation by name. Every session pays for all of them or for none.
- **A copy per agent directory.** Two tracked sources drift, which one rule file exists to
  prevent. A symlink is one source.
- **One symlink for the whole directory.** It is the obvious first try, and Claude Code does not
  follow it (see Context).

## Sources / Prior art

- Agent Skills specification — <https://agentskills.io/specification>; client list at
  <https://agentskills.io/>. Its front matter is YAML and requires `name`, equal to the parent
  directory, and a non-empty `description`; `metadata` is a nested mapping.
- Claude Code reads the front matter only when the opening `---` is the first line. If the YAML
  does not parse, the skill loads with no fields set —
  <https://code.claude.com/docs/en/skills>.
- Skill locations — Claude Code: <https://code.claude.com/docs/en/skills>; Codex:
  <https://learn.chatgpt.com/docs/build-skills>; Gemini CLI:
  <https://geminicli.com/docs/cli/skills/>; Cursor: <https://cursor.com/docs/context/skills>.
- Matt Pocock's skills — <https://github.com/mattpocock/skills>: a process as a set of named,
  lazily loaded `SKILL.md` files rather than a framework that owns it; *Writing for agents* —
  <https://www.aihero.dev/skills-writing-for-agents>: what applies in one context out of ten goes
  behind a pointer. Addy Osmani, *Agent skills* — <https://addyosmani.com/blog/agent-skills/>:
  process over prose, each phase ending in evidence.
- The open request for Claude Code to read `.agents/skills/` —
  <https://github.com/anthropics/claude-code/issues/31005>; symlinked skill folders load but the
  agent's listing may not show them — <https://github.com/anthropics/claude-code/issues/14836>.

## Consequences

- Positive: one place for each how, and the rule file shrinks to rules.
- Positive: a procedure costs context only when it runs.
- Positive: a human starts a review or an ADR by name instead of hoping a paragraph is followed.
- Negative: a skill is a fifth kind of tracked artifact. Its steps or its reference files can
  smuggle in a rule, and only review sees that.
- Negative: a skill is Markdown and is followed as reliably as Markdown. What the template
  enforces, it enforces through hooks, checks, and repository settings, and this decision adds
  nothing to that.
- Negative: the pointers rest on observed behaviour of one Claude Code version, not on documented
  behaviour.
- Negative: check 11 reads the front matter without a YAML parser. YAML that breaks on a
  top-level line, such as an unquoted `: ` in a value, passes it.
- Follow-ups: delete the pointer directory and its check once Claude Code reads `.agents/skills/`.

## Enforcement

- Check 11 of `scripts/check-docs.sh` holds `.claude/skills/` (clause 6). It fails on a skill
  without a symlink there, and on an entry that is no symlink, dangles, or resolves to another
  skill. Its self-test cites this ADR.
- Check 11 also holds clause 7. It fails on a `SKILL.md` whose first line is not `---` or that has
  no closing `---` line; a fence may carry trailing spaces. It fails on one without a top-level
  `name` equal to its directory or a top-level `description`. It fails on a non-empty unindented
  line between the fences that starts no key, no sequence item, and no comment. Indented lines,
  such as nested keys and block scalars, pass unread.
- Checks 3 and 4 of `scripts/check-docs.sh` fail on a link or a section reference of a skill
  that does not resolve.

**Not mechanically decidable:** the review holds four judgements:

- whether a skill or its files state a rule (clauses 3 and 5);
- whether the rule file and the templates name the skill instead of its steps (clause 4);
- whether a self-test runs each script of a skill (clause 5);
- whether a pointer directory other than `.claude/skills/` is complete (clause 6).
