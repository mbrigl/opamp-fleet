# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately**. Do not open a public issue or pull request.

- Use GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
  (**Security → Report a vulnerability**) if enabled, or
- email the maintainer: [security@hivevm.org](mailto:security@hivevm.org).

Please include enough detail to reproduce the issue: affected version or commit, steps, and
impact. We aim to acknowledge reports within a reasonable time frame and will coordinate a fix and
disclosure with you.

## Dev Container & agent execution

OpAMP Fleet runs coding agents inside the Dev Container defined in
security posture:

- **The Dev Container has no access to the host container engine.** The host Docker or Podman
  socket is **not** mounted into the container ([ADR-0002](docs/adr/0002-dev-container-runtime.md)),
  so code or agents running inside cannot control the host engine. Host containers are managed
  from a host-side VS Code extension (see the README), which keeps that capability outside the
  container's reach. The container is still not a strong security boundary, so run only agents
  and code you trust in it.
- **Nothing leaves the machine without a human.** The agent commits on a branch on its own. A
  push, a `gh` action, and any command that rewrites or discards history wait for an explicit
  instruction ([`AGENTS.md` §6](AGENTS.md#6-project-rules)). That rule is documentation: no file
  in the repository can stop an agent from running `git push`, and a gate in one agent's own
  configuration would hold for that agent only, so none is used. Authentication uses the web flow
  of `gh` with no stored tokens. Two halves hold for every agent and every human, because they are
  the repository's own git hooks ([`.githooks/`](.githooks/), enabled by the Dev Container): no
  commit on `main`, and no push while the checks are red. Each is bypassable with `--no-verify`,
  which is a deliberate act, not a default.
- **The specification and the decision record change only by human decision.** A change to
  [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md) is a decision rather than an edit, and only a
  human changes an ADR's status ([`AGENTS.md` §3](AGENTS.md#3-adr-rules)). The gate is
  [`.github/CODEOWNERS`](.github/CODEOWNERS) together with the Code-Owner review requirement
  listed under [**Repository settings**](README.md#repository-settings). It does not stop the
  edit; it routes it to a human before it reaches `main`, and it holds for every agent alike.
  Two limits are worth knowing. [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) is deliberately
  not gated, because [§5](AGENTS.md#5-quality-bar--definition-of-done) requires it to change in
  the same commit as the structure it describes. And a code owner GitHub does not know matches
  nobody, so the file enforces nothing until it carries a real handle.

## Secrets

The rule is in [`AGENTS.md` §7](AGENTS.md#7-secrets), and that wording governs: a secret never
enters a tracked file, a commit message, an ADR, a log, or CI output, and one that reaches git is
rotated rather than deleted from the tip of a branch. Rotation first, history rewriting only as
cleanup, as [GitHub's own guidance](https://docs.github.com/en/authentication/keeping-your-account-secure/removing-sensitive-data-from-a-repository)
puts it. This section records what the repository's own configuration does about it, which is
less than the rule asks.

Two mechanisms keep environment files out of the way. [`.gitignore`](.gitignore) leaves `.env*`
untracked in every directory, with a secrets-free `.env.example` as the one tracked exception.
[`.claude/settings.json`](.claude/settings.json) denies Claude Code read access to the same
pattern, so their contents cannot reach the agent's context and from there a file, a commit
message, or a log.

Three limits are worth knowing. The deny pattern also covers a tracked `.env.example`, whose shape
an agent is told rather than shown. It is a permission rule on one agent's file-reading tool, not
a sandbox, and it reaches no other agent; for an agent without an equivalent mechanism the rule is
documentation only, as with the git rules above. And the two together are not enforcement of the
rule: they close one path for one agent. A secret that arrives by any other path (typed into a
commit message, echoed by a workflow step, pasted into an ADR) is caught by review or by GitHub's
[secret scanning with push protection](https://docs.github.com/en/code-security/secret-scanning/introduction/about-secret-scanning),
which is a repository setting (see [**Repository settings**](README.md#repository-settings)) and
does nothing until a maintainer enables it. No check in this repository looks for a secret: a
regex scan in [`scripts/`](scripts/) would be false confidence rather than enforcement, and a
scanning toolchain is a dependency a project adds through an ADR of its own once it has
credentials the built-in patterns miss.

## Actions in the workflows

The actions the workflows run are referenced by major version tag, a mutable reference accepted
with its risk in [ADR-0007](docs/adr/0007-action-references.md) and held by
[`scripts/check-action-refs.sh`](scripts/check-action-refs.sh).

## Supported versions

<!-- TODO: document which versions/branches receive security fixes once the project has releases. -->
The project is pre-release; a support policy will be defined once it reaches its first release.
