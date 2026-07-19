# Contributing

Thanks for your interest in contributing. This project is **specification- and ADR-driven**, and the
same rules apply to humans and coding agents alike. For coding agents, the full instructions and
agent-specific details live in [`AGENTS.md`](AGENTS.md).

## Before you start

Read, in this order:

1. [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md) — the constitution: problem, goals, and vocabulary.
2. [`docs/adr/`](docs/adr/) — the Architecture Decision Records. **Accepted ADRs are binding.**
3. [`AGENTS.md`](AGENTS.md) — the working rules (these govern coding agents, but the workflow is the
   same for human contributors).

Authority runs **specification → accepted ADRs → individual change**.

## Workflow

1. **Open an issue first** for anything non-trivial, so scope and intent can be agreed before work
   starts.
2. **Architecture-relevant decisions need an ADR.** Adding a dependency, designing a public
   interface, choosing a protocol/data format or a persistence strategy — copy
   [`docs/adr/template.md`](docs/adr/template.md), set status `proposed`, and wait for a maintainer
   to accept it before implementing, and list the ADR in the index in
   [`docs/adr/README.md`](docs/adr/README.md) as part of the same change. See that file for the full
   process. Not every change needs an ADR — see the calibration rule in [`AGENTS.md`](AGENTS.md).
3. **Keep changes small and reviewable.** Reference the relevant ADR(s) in commits and the pull
   request (e.g. `Implements ADR-NNNN`).
4. **Open a pull request** against `main`. Fill in the PR template and link the issue/ADR.

## Conventions

- **All artifacts in the repository are written in English** — code, comments, commit messages,
  documentation, ADRs, and PRs. You may discuss in any language, but what lands in the repo is
  English.
- Use the **project vocabulary** from the specification consistently.
- Build, test, and formatting commands: see the **Build, Test & Run** section in
  [`README.md`](README.md).
- **Documentation consistency is checked in CI** (ADR index integrity and relative links) via
  [`scripts/check-docs.sh`](scripts/check-docs.sh). Run it locally with `scripts/check-docs.sh`
  before opening a pull request — it needs only bash and coreutils, already in the Dev Container.

## License of contributions

This project is licensed under the [Apache License 2.0](LICENSE). By submitting a contribution, you
agree that it is provided under the terms of that License, without any additional terms or conditions
(Apache-2.0 §5). Do not submit code you are not authorised to license this way.

## Reporting security issues

Do **not** open a public issue for vulnerabilities. Follow [`SECURITY.md`](SECURITY.md).
