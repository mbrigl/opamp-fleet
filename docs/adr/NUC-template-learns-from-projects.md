# NUC-template-learns-from-projects: A derived project's design revision drafts findings about the template as issues, which a human sends and the template's revision reads

- **Status:** 🟢 accepted
- **Date:** 2026-11-14
- **Deciders:** NUC maintainer
- **Applies to:** the `design-revision` skill, `.github/ISSUE_TEMPLATE/template-finding.md`, `TEMPLATE-SETUP.md`, `scripts/test-template-setup.sh`, the rule `docs/template-identity` of `scripts/check-docs.sh`, and every file of the template, its skills, scripts, hooks, and workflows (clause 4)

## Context

A derived project learns about the template while it works. A check costs minutes and never
finds anything. A rule is misread the same way by every session. A skill sends a session down
a path that costs a review round. Today that knowledge stays in the project. It reaches the
template only when its human remembers it and writes to the maintainer.

The template measures its own work since v0.10.0. `scripts/check-all.sh` names the time of each
check. The pull request template carries the line
`Measures: tokens <n> | checks <n> s | review rounds <n>`.
`scripts/sensor-work-measures.sh` reports the hours and rounds of review per merge, and
`scripts/sensor-red-commits.sh` the commits that were red alone. A derived project has the same
measures. Its design
revision ([NUC-design-is-revised](NUC-design-is-revised.md)) is the moment it reads them.

Several forces bound the answer:

- **Sending publishes.** An issue in `hivevm/nuc` is public. A finding can carry the project's
  code, names, or paths. Only a human can judge what may leave the project.
- **Nothing acts on its own.** The template ships no service. An agent runs `gh` only on a
  human's instruction ([NUC-main-only-via-pr §2](NUC-main-only-via-pr.md)). A script that sends
  would publish on an agent's judgement.
- **The template copies itself.** A derived project starts as a copy of the template. A file
  meant for the template's own repository arrives in every project.
- **Findings need a reader.** A finding nobody reads changes nothing. The template revises its
  own design like any project, and that revision is where tickets for the next release are cut.
  A revision reads the modules the work touched
  ([NUC-design-is-revised §2](NUC-design-is-revised.md)); the findings sent are input from
  outside that work, so this ADR names them as a second input of the template's revision.
- **The human decides the work.** Only the human decides which backlog entry becomes a ticket
  ([NUC-large-work-is-planned §8](NUC-large-work-is-planned.md)). A finding sent from a project
  is a candidate in the same way.

The human chose on 2026-11-13: findings come from the design revision only; they reach the
template as issues in one form; the human sends them; the template's design revision reads them;
the setup removes the form from a derived project; a finding names the template only.

## Decision

We will carry findings about the template from a project's design revision to the template's
revision as issues in one form, sent by a human.

1. **Source.** A design revision of a derived project must write its findings about the template
   apart from the project's own tickets.
   - Another session that meets a finding about the template must record it as a backlog entry
     ([NUC-large-work-is-planned §6](NUC-large-work-is-planned.md)).
   - The next revision may draft a finding from such an entry.
   - The revision must mark the entry as drafted. A later revision must not draft a second finding
     from it.
   - The entry must stay until the human has sent the draft or dropped it. The human must then
     delete the entry, or close its issue where the backlog lives in the host's issues.
2. **Form.** A finding must follow the fields of the issue template `template-finding.md` of
   `hivevm/nuc`. The `design-revision` skill must carry the same fields, since the setup deletes
   the file from a derived project.
   - A finding must name the release the project carries, the rule, file, or skill of the
     template it concerns, and a proposal.
   - A finding must name the measures that show it, where any do.
   - A finding must not carry code, names, paths, or data of the project.
3. **Draft.** The revision must write each finding as a draft under `.scratch/`. It must list the
   drafts in its results, so that the human sees them before the session ends.
4. **Sending.** Only a human may send a finding, as an issue in `hivevm/nuc`. No file of the
   template may send anything on its own.
5. **Reading.** The open issues of that form are an input of the template's design revision.
   - The human must bring them in: by handing them over, or by instructing the session to read
     them with `gh` ([NUC-main-only-via-pr §2](NUC-main-only-via-pr.md)).
   - The revision must list each one as a candidate for a ticket.
   - The human must decide which candidate becomes a ticket.
   - The human must close each issue the revision listed, with a link to its ticket or the
     reason it was declined.
6. **Setup.** The setup of a derived project must delete
   `.github/ISSUE_TEMPLATE/template-finding.md`.

**Out of scope:**

- the measures themselves, which the toolchain and the pull request template carry;
- a reader of the harness's tokens, which belongs to `hivevm/nuc-tools`;
- findings a project's human sends outside a design revision, by any channel they choose;
- the labels, milestones, and triage of `hivevm/nuc` beyond clause 5.

## Latitude

A project may choose:

- whether its revision drafts any finding at all; a revision that found nothing about the
  template drafts none;
- whether its human sends a draft, edits it, or drops it.

The catalogue has no entry for these choices, and none of them needs an ADR.

## Alternatives considered

- **Findings from any session, like the backlog.** More findings, and more noise: a session
  inside one ticket sees one path, not the cost across the work. The revision reads the
  measures of many merges and judges the template from them. A backlog entry about the template
  stays raw material, which only the revision drafts as a finding.
- **A script that opens the issue.** It is quick, and it publishes on an agent's judgement. An
  agent runs `gh` only on a human's instruction
  ([NUC-main-only-via-pr §2](NUC-main-only-via-pr.md)), and an instruction per finding is the
  human's look this ADR asks for anyway.
- **The form only in the skill, with a link to a new issue.** No file of the template would
  need deleting at setup. The human chose an issue template, which GitHub offers on the new-issue
  page of `hivevm/nuc` to anyone who files one, not only to a session that read the skill.
- **An issue form in YAML with required fields.** GitHub would refuse an issue without them. A
  Markdown template keeps the form readable in the tree and in the skill alike; a later release
  may move to a form.
- **Findings as backlog entries of the project.** They would stay in the project, which is the
  state this ADR ends.
- **No fixed form: mail, a discussion, or any issue.** The template's revision then has no one
  place to read, and a finding without the release and the measures cannot be weighed.
- **Keeping the issue template in derived projects.** It helps nobody there, since the findings
  belong to the template's repository, and a project's contributor would file template findings
  against the project.
- **Telemetry sent to the maintainer.** It reports what the harness measures, not what the
  project learned. It also sends data without a human's look at each item.

## Sources / Prior art

- GitHub, issue templates and the `.github/ISSUE_TEMPLATE/` folder:
  <https://docs.github.com/en/communities/using-templates-to-encourage-useful-issues-and-pull-requests/configuring-issue-templates-for-your-repository>
- Ticket 23 of the revision of 2026-11-07, parts 4 and 5,
  `docs/plan/revision-2026-11-07/23-cost-and-time-are-measured-while-the-work-runs.md`.
- The measures of the v0.9.0 work, read afterwards from its transcripts, as that ticket states
  them.

## Consequences

- Positive: what one project learns about the template can reach every other one, through the
  next release. The template's revision reads evidence from real projects, not only its own
  work.
- Positive: nothing leaves a project without a human's look at it.
- Negative / trade-offs: a finding depends on a human who sends it; many will not be sent. A
  public issue may still reveal more than its author meant. The form asks, the human decides.
- Negative / trade-offs: the setup gains a step, and the check of the setup gains a file. A
  derived project that takes up a release must not take up the issue template; the notes of the
  release that ships it say so, and the check fails a project that does.
- Negative / trade-offs: the form stands twice, in the issue template and in the skill, and the
  two must change together.
- Follow-ups: the section of the `design-revision` skill on cost and time, with the fields of
  the form; the issue template; the step in `TEMPLATE-SETUP.md` and `close_setup`; the
  extension of the rule `docs/template-identity`.

## Enforcement

The rule `docs/template-identity` of `scripts/check-docs.sh` fails a derived project whose setup
is done and which still carries `.github/ISSUE_TEMPLATE/template-finding.md` (clause 6). Its
self-test, `scripts/test-check-docs.sh`, cites this ADR with a `Verifies:` marker. The case of
`scripts/test-template-setup.sh` that closes the setup asserts that the file is gone.

The rule `docs/opens-no-issue` of `scripts/check-docs.sh` fails a file under `scripts/`,
`.githooks/`, `.github/`, or `.agents/` that opens an issue on its own, while `TEMPLATE-SETUP.md`
exists (clause 4). The same self-test holds its cases.

**Not mechanically decidable:** the review and the human hold these judgements, each for its
reason:

- whether a finding came from a design revision: a draft under `.scratch/` leaves no trace a
  check reads (clauses 1 and 3);
- whether an entry a draft came from was marked and kept: the tree shows neither the source of
  a draft nor the human's decision (clause 1);
- whether a draft carries nothing of the project: what is the project's is a judgement, not a
  pattern (clause 2);
- whether a file sends anything the rule `docs/opens-no-issue` does not match: the rule finds
  the plain forms only, and the review holds the rest (clause 4);
- whether the template's revision listed the open findings: they live on GitHub, outside the
  tree (clause 5).
