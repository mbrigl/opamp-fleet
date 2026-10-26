# Contributing

Thanks for your interest in contributing. This project is **specification- and ADR-driven**. The
working rules in [`AGENTS.md`](AGENTS.md) are the single source of truth, and they govern coding
agents and human contributors alike. This file carries no rules of its own: it points at them and
adds only the contributor-facing procedure.

## Before you start

Read what [`AGENTS.md` §2](AGENTS.md#2-start-here) names before any non-trivial work, then
[`AGENTS.md`](AGENTS.md) itself, the working rules for humans and agents alike, and
[`docs/CONVENTIONS.md`](docs/CONVENTIONS.md), how this project writes code and tests where no
check decides it.

Authority runs **specification → accepted ADRs → task**
([`AGENTS.md` §3](AGENTS.md#3-adr-rules)).

## Workflow

1. **Settle scope and intent first** when they are not yet agreed
   ([`AGENTS.md` §4](AGENTS.md#4-working-style)).
2. **Follow [`AGENTS.md`](AGENTS.md)**: when a decision needs an ADR and how it is reviewed
   ([§3](AGENTS.md#3-adr-rules)), when a change counts as done
   ([§5](AGENTS.md#5-quality-bar--definition-of-done)), and the git and project rules
   ([§6](AGENTS.md#6-project-rules)). Run [`scripts/check-all.sh`](scripts/check-all.sh) locally
   before pushing ([§5](AGENTS.md#5-quality-bar--definition-of-done)). It runs the same checks CI
   runs on every pull request, and the repository's git hooks run them for you
   ([README → Git hooks](README.md#git-hooks)). They need bash 4.4 or later and coreutils,
   already in the Dev Container.
   Only the shell lint additionally needs [ShellCheck](https://www.shellcheck.net), which the Dev
   Container installs; elsewhere it skips itself where that is missing, and CI runs it always.
3. **Open a pull request** against `main` and fill in the PR template.

## License of contributions

This project is licensed under the [Apache License 2.0](LICENSE). By submitting a contribution, you
agree that it is provided under the terms of that License, without any additional terms or conditions
(section 5 of the Apache License 2.0). Do not submit code you are not authorised to license this way.

## Reporting security issues

Do **not** open a public issue for vulnerabilities. Follow [`SECURITY.md`](SECURITY.md).
