# OpAMP Fleet

[![Release](https://img.shields.io/github/v/release/mbrigl/opamp-fleet?display_name=release&label=release)](https://github.com/mbrigl/opamp-fleet/releases/latest)
[![CI](https://github.com/mbrigl/opamp-fleet/actions/workflows/ci.yml/badge.svg)](https://github.com/mbrigl/opamp-fleet/actions/workflows/ci.yml)
[![Checks](https://github.com/mbrigl/opamp-fleet/actions/workflows/checks.yml/badge.svg)](https://github.com/mbrigl/opamp-fleet/actions/workflows/checks.yml)
[![Audit](https://github.com/mbrigl/opamp-fleet/actions/workflows/audit.yml/badge.svg)](https://github.com/mbrigl/opamp-fleet/actions/workflows/audit.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

**OpAMP Fleet** manages a fleet of telemetry agents over OpenTelemetry
[OpAMP](https://opentelemetry.io/docs/specs/opamp/), written in Rust: a **Server** that decides what
every agent runs, and a **Client** that runs the agents of a host and reports what they really run
— OpenTelemetry Collectors and agents that never spoke OpAMP alike.

> **📖 Running it? Read the [User Manual](docs/manual/README.md).** Setting up a fleet for the
> first time? Start with [Setting up a fleet](docs/manual/setup.md).

> Working on it? The rules for humans and coding agents alike are in [`AGENTS.md`](AGENTS.md).

## Overview

A fleet of agents, each configured by a local file, drifts: changing what a hundred agents do means
reaching a hundred machines, and nobody can say for certain what each one actually runs. OpAMP
closes that loop — an agent takes its configuration over the protocol and reports back what it
applied and how it is doing. OpAMP Fleet implements both ends:

- **Server** — an API-first control plane on Linux. It holds what the fleet should run, tracks what
  each agent reports, and reconfigures only an agent whose configuration differs. Its contract is
  an OpenAPI-described REST API, so any portal can drive it; the bundled UI is deliberately simple.
- **Client** — one process, installed as an OS service on Linux, macOS and Windows, that updates
  itself from a signed package. In **Supervisor Mode** it runs many Supervisors, each managing one
  process; in **Gateway Mode** it folds other Clients' connections onto a few upstream ones. Both
  modes combine on one host.
- **Kinds behind a hexagonal core** — a Supervisor's kind knows its agent: `collector` serves every
  OpenTelemetry Collector, `command` runs any other program and watches it from outside. A new kind
  of agent is a new kind behind the same ports; the core does not change.
- **The protocol on the record** — both ends implement OpAMP as completely as it allows, against a
  pinned version, and [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md) states each capability's status.

The [specification](docs/SPECIFICATION.md) holds the goals, the vocabulary and the non-goals; the
[Architecture Decision Records](docs/adr/) hold the reasoning behind every structural choice.
[`docs/HARDENING.md`](docs/HARDENING.md) collects candidate hardening measures as a backlog.

## Architecture

![The Server with its Operator and Agent planes, the Client with Supervisor Mode and Gateway Mode, and the agents, Collectors and downstream Clients they serve](docs/images/architecture.png)

*The image carries its [Excalidraw](https://excalidraw.com) scene: open the PNG there to edit it,
and export it again with "Embed scene" switched on.*

The Server sees only **Agents**, told apart by `instance_uid` and never by the connection that
carried them: a Supervisor's process, a Collector reporting through its own `opampextension`, and a
Client several hops behind a Gateway look alike to it. Every connection to the **Agent plane** is
TLS 1.3 with a client certificate, which the Server's CA issues once an operator approves a host's
enrolment. The **Operator plane** — the REST API and the UI — is a listener of its own and stays on
the loopback unless it is published behind a password. How the code is cut into crates and modules
is in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

## Prerequisites

- [VS Code](https://code.visualstudio.com/) with the
  [Dev Containers](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)
  extension, or any DevContainer-compatible IDE
- Docker / Podman (rootless) available on the host
- Outside the Dev Container: bash 4.4 or later for the checks, 3.2 for the git hooks
  ([ADR-0002](docs/adr/0002-decisions-verified-by-tests.md)); macOS ships 3.2

## Getting Started

1. Open the repository in VS Code and choose **Reopen in Container**. The Dev Container and the
   preconfigured agent extensions build automatically, and the container enables the repository's
   git hooks with the merge settings they rely on ([Git hooks](#git-hooks)). Working outside the
   container? Run the three commands listed there yourself, once per clone.
2. Authenticate your coding agent inside the container (for Claude Code: `claude login`).
3. Start working with the agent. Drive the work from the specification and the ADRs.

## Build, Test & Run

One Cargo workspace on Rust stable, which the Dev Container provides; its five crates are described
in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md). This section is the single source of the build,
test and run commands, for humans and agents alike. [`scripts/check-rust.sh`](scripts/check-rust.sh)
runs the build, test and lint commands and the first two rows of the table below, locally through
`scripts/check-all.sh` before every push and in CI as the `rust` job of
[`checks.yml`](.github/workflows/checks.yml); [`ci.yml`](.github/workflows/ci.yml) release-builds
the Client for Linux, Windows and macOS and the Server for Linux.

- **Build:** `cargo build --workspace`
- **Test:** `cargo test --workspace`
- **Lint:** `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- **Make development certificates:** `scripts/dev-pki.sh` writes them, a `server.toml` and a
  `supervisor.toml` to `.dev-pki/`; `scripts/seed_test_configs.sh --enrol` approves the development
  Client's enrolment
- **Run the Server:** `cargo run -p fleet-server -- --config .dev-pki/server.toml`
- **Run the Client:** `cargo run -p fleet-agent -- --config .dev-pki/supervisor.toml`
- **Run the operator tool:** `cargo run --bin opamp-fleetctl -- package fetch` (a known agent's
  release, handed to the Server), `-- package --help` (build, hash and sign an artifact) or
  `-- pki --help` (the fleet's certificates), documented in [the manual](docs/manual/tools.md)

The checks beyond these, each for what the plain build cannot show:

| Check | Command | When, and what it needs |
|---|---|---|
| `opamp` per feature | `for f in "" client server; do cargo clippy -p opamp --all-targets --no-default-features --features "$f" -- -D warnings; done` | each feature stands alone (ADR-0008); run by `check-rust.sh` |
| fewer Supervisor kinds | `cargo clippy -p fleet-agent --all-targets --no-default-features -- -D warnings && cargo test -p fleet-agent --no-default-features` | a build leaving a wrapped kind out compiles and refuses its `type` (ADR-0017); run by `check-rust.sh` |
| Windows build | `cargo xwin clippy -p fleet-agent --all-targets --target x86_64-pc-windows-msvc -- -D warnings` | on a change to platform-gated code; needs `cargo install cargo-xwin` and `rustup target add x86_64-pc-windows-msvc`; CI builds the Client on Windows |
| supply chain | `cargo deny check` | as [`deny.toml`](deny.toml) sets it; needs `cargo install cargo-deny`; the `Security audit` workflow runs it weekly and on lockfile changes |
| fuzzing | `cargo +nightly fuzz run <target> fuzz/corpus/<target> fuzz/seeds/<target>` | the targets are in [`fuzz/Cargo.toml`](fuzz/Cargo.toml); needs `cargo install cargo-fuzz` and nightly; the `Fuzz` workflow runs them daily |
| interop with `opamp-go` | `cargo test -p fleet-agent --test interop_opamp_go -- --ignored --nocapture --test-threads=1` | needs a Go toolchain, which the Dev Container does not ship; the `Interop` workflow runs it weekly; see [`interop/README.md`](interop/README.md) |
| service smoke | `FLEET_SMOKE_THROWAWAY=1 scripts/check-service-smoke.sh` | registers services and accounts, so a throwaway machine only; see [below](#the-service-smoke-test) |

Both binaries read a TOML file; every setting has a default except the TLS material. The annotated
examples are in [`config/`](config/).

### Releases

The version is `[workspace.package] version` in [`Cargo.toml`](Cargo.toml), so bumping it is a
reviewed commit. The `Release` workflow makes the `version/*` tag from it, then builds and publishes
the Client's archive and native packages per platform, the operator tool, an SBOM beside each,
`SHA256SUMS`, and the Server's image `ghcr.io/mbrigl/opamp-fleet-server:<version>`, every one
attested (ADR-0033, ADR-0034). Started with `dry_run`, the default, it builds everything and
publishes nothing. How an operator checks a download is in
[the manual](docs/manual/tools.md#getting-it), and the image in
[the Server's manual](docs/manual/server.md#in-a-container).

### The service smoke test

The `Service smoke` workflow installs the Client as a real service on Linux, macOS and Windows,
weekly and on demand: install, start, the Agent appearing in the fleet, its process killed and
restarted by the manager, a stop that stays stopped, uninstall, and an install that enrols with a
`bootstrap.p12`. Its test is `#[ignore]`d, so an ordinary `cargo test` installs nothing.

What still needs a human, per platform:

- starting at **boot**, the service's [log](docs/manual/client.md#where-the-services-logs-are), and
  the Agent in the fleet UI;
- an **SELinux-enforcing host**: the `.rpm` install must start, and a service dying with
  `status=203/EXEC` and an AVC denial is the failure ADR-0019 clause 12 prevents;
- each wrapped kind under the packaged account `opamp-fleet` and under `--run-as root`: it starts,
  reports, and its `sudo` or setuid helpers still work (ADR-0037).

## Usage

The [quick start](docs/manual/README.md#quick-start-a-closed-loop-on-one-machine) in the User Manual
closes the loop on one machine: a Server, a Client, a Configuration rolled out through a
Deployment, and the Agent reporting it applied. From there:

- [Setting up a fleet](docs/manual/setup.md) — the certificates, both configurations, enrolment;
- [Rollout walkthrough](docs/manual/rollout.md) — an artifact built, signed, uploaded, rolled out;
- [Running the Client as an OS service](docs/manual/client.md#running-it-as-an-os-service);
- [the REST API](docs/manual/server.md#the-rest-api), whose contract is the OpenAPI document at
  `/api/v1/openapi.json`.

## Project Layout

```
README.md             # this overview
CHANGELOG.md          # operator-facing changes: what an upgrade needs edited or moved
AGENTS.md             # the working rules, for coding agents and humans alike
CONTRIBUTING.md       # how to contribute, pointing at the rules
SECURITY.md           # how to report a vulnerability, and what the configuration guarantees
CODE_OF_CONDUCT.md    # the standards everyone taking part is held to
LICENSE               # the Apache License 2.0
NOTICE                # the copyright notice the license asks to be carried along
docs/                 # specification, architecture, glossary, conventions, conformance, hardening
docs/adr/             # Architecture Decision Records
docs/manual/          # the user manual: setting up a fleet, then Server, Client, and the tools
Cargo.toml            # the Cargo workspace and the product's version
Cargo.lock            # the resolved dependency graph, committed
rust-toolchain.toml   # the pinned Rust toolchain
deny.toml             # cargo-deny: advisories, licences, sources and banned crates
crates/               # opamp (shared) · fleet-core · fleet-server · fleet-agent · fleet-tools
config/               # annotated example configuration files (server.toml, supervisor.toml)
packaging/            # what the release packs with: Linux maintainer scripts, WiX, Containerfile
fuzz/                 # the fuzz targets of every parser that reads untrusted bytes
interop/              # the Go side of the interop check against opamp-go
observability/        # the development observability stack, run on the host
scripts/              # check-all.sh, the checks, sensors and their self-tests, the dev tools
.githooks/            # git hooks: nothing lands on main locally; checks before a merge and a push
.github/              # the workflows, Dependabot, templates and code owners
.devcontainer/        # the Dev Container definition
.vscode/              # shared editor settings
.agents/skills/       # the procedures of AGENTS.md as Agent Skills, one directory each
.claude/              # pointers for Claude Code to AGENTS.md and the skills; its permissions
.editorconfig         # editor-neutral formatting baseline
.gitattributes        # LF everywhere, and the ADR index merged by its driver
.gitignore            # what git leaves out, .scratch/ for session artifacts among it
```

## Dev Container

The environment is [`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json): a
prebuilt base image with Dev Container Features, pinned in the committed `devcontainer-lock.json`,
and VS Code extensions on top — no Dockerfile.

The development observability stack — Collector, ClickHouse and Grafana — runs on the **host**,
started when you want it, and the container reaches it on its own loopback; see
[`observability/`](observability/README.md):

```sh
docker compose -f observability/compose.yaml up -d     # on the host, from the repository root
```

The container has **no access to the host's Docker daemon**
([ADR-0004](docs/adr/0004-dev-container-runtime.md)). To manage the host's containers from VS Code,
install the **Container Tools** extension in the host's VS Code;
[`.vscode/settings.json`](.vscode/settings.json) keeps it running on the host.

## Coding Agents

This Dev Container preinstalls the **Claude Code** and **Mistral Vibe** VS Code extensions (see
[`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json)); other agents (OpenAI Codex,
Cursor, OpenCode, GitHub Copilot) work too once you add them.

The rules every agent follows live in [`AGENTS.md`](AGENTS.md); that there is exactly one such file
is decided in [ADR-0001](docs/adr/0001-agent-governance-model.md). An agent that cannot read it
natively gets a pointer file instead. [`.claude/CLAUDE.md`](.claude/CLAUDE.md) is the one shipped
here; it carries the pointer and no rules of its own ([`AGENTS.md` §3](AGENTS.md#3-adr-rules)). It
exists for one gap only: Claude Code loads `CLAUDE.md`, not `AGENTS.md`, tracked upstream as
[anthropics/claude-code#34235](https://github.com/anthropics/claude-code/issues/34235). Once that
lands, delete the pointer file rather than keeping a second file to maintain.

The procedures those rules name (proposing an ADR, planning a feature, working its tickets
alone or in parallel, reviewing a change, revising the design, normalizing the record) are Agent
Skills under [`.agents/skills/`](.agents/skills/), the open format and the directory Codex,
Gemini CLI, and Cursor read
([ADR-0003](docs/adr/0003-procedures-as-skills.md)). An agent invokes one by name (`/review`) or
picks it up when the task matches its description. Claude Code reads `.claude/skills/` only, so
that directory holds one symlink per skill into `.agents/skills/`; a directory-level symlink is
not followed. The symlinks are pointers of the same kind as `CLAUDE.md`, tracked upstream as
[anthropics/claude-code#31005](https://github.com/anthropics/claude-code/issues/31005). Delete them
when that lands, and add one for every skill added meanwhile; the documentation check fails on a
skill without its pointer. What enforces a rule is what every agent hits
([`AGENTS.md` §6](AGENTS.md#6-project-rules)): the git hooks under [`.githooks/`](.githooks/),
the checks, and the repository settings below.

Agents work tickets in parallel without any one vendor's mechanism: a dispatching session claims
each ticket by its branch in a git worktree of its own and starts a fresh session there, with the
non-interactive command of whichever agent the project's harness ADR names, and the session works
the ticket through the `work-tickets` skill.

## Git hooks

Git does not version its hooks, so the repository keeps them under [`.githooks/`](.githooks/).
They hold for every agent and every human. The Dev Container enables them, with the merge
settings they rely on, on every start; outside it, run once per clone:

```sh
git config core.hooksPath .githooks
git config merge.ff false
git config pull.ff only
git config merge.adr-index.driver '.githooks/merge-adr-index %O %A %B %S %X %Y'
```

`merge.ff false` makes every merge a merge commit, as a `feat/` branch keeps them
([`AGENTS.md` §6](AGENTS.md#6-project-rules)); a fast-forward would make no commit and run no
hook. `pull.ff only` keeps a pull from then making a merge. Without the two settings the hooks
still hold: a fast-forward skips the documentation check of `pre-merge-commit`, and the push
still gates what it brought. The merge driver of the ADR index keeps the rows both sides add and
stops on its prose ([`.githooks/merge-adr-index`](.githooks/merge-adr-index)); without it, every
row two branches append conflicts.

| Hook | Refuses | Bypassed by |
|---|---|---|
| `reference-transaction` | moving the local `main` to a commit `origin/main` lacks: by a commit, merge, cherry-pick, reset, or `git branch -f`. It lets through `git merge --ff-only origin/main`, a reset to it, `git fetch origin main:main`, a write of `main` where it already is, and a deletion. Needs git 2.28 or later. | running git without the hooks (`git -c core.hooksPath=/dev/null`), or moving `origin/main` by hand; not by `--no-verify` |
| `pre-commit` | a commit on `main`; the conclusion of a conflicted merge while `scripts/check-all.sh --changed` is red | `--no-verify` |
| `pre-merge-commit` | a merge on `main`; a merge into any other branch while `scripts/check-docs.sh` is red | `--no-verify` |
| `pre-push` | a push to the remote's `main`, a deletion included; any other push while `scripts/check-all.sh --changed` is red, except one that only deletes, and one from a clean working tree whose tree the checks passed on before, as after a rework that only regroups | `--no-verify`, as the claim on a ticket does ([`AGENTS.md` §6](AGENTS.md#6-project-rules)); a tree written by hand into `checks-green-trees` of the git directory |

With `--changed`, the self-tests of what the branch left untouched since `main` are skipped; CI
runs every one before anything reaches `main`. A bypass is a deliberate act, not a default;
[`AGENTS.md` §6](AGENTS.md#6-project-rules) says when an agent may. Each refusal says what to do
instead.
[`scripts/test-git-hooks.sh`](scripts/test-git-hooks.sh) exercises every row.

## Integration branches

A change reaches `main` through a pull request. Several branches may integrate first on a
`feat/` branch, by local merges or by pull requests against it, and it stays the human's to
rework until its pull request into `main` merges ([`AGENTS.md` §6](AGENTS.md#6-project-rules)).
Each step waits for a human's instruction.

1. Start it at the remote's `main`: `git fetch origin`, then
   `git switch -c feat/<topic> origin/main`.
2. Merge a reviewed branch into it with its hand-off as the message, written to a file since an
   editor strips its headings: `git merge --no-ff -F .scratch/<hand-off>.md <branch>`. The
   documentation check gates the merge, and the push of step 6 runs the rest. One that stops on a
   conflict is concluded, once resolved and staged, with `git commit -F .scratch/<hand-off>.md`,
   and all the checks gate that commit.
3. Or merge it by a pull request against the `feat/` branch, so that its pipelines run first
   ([`AGENTS.md` §6](AGENTS.md#6-project-rules)). With both branches pushed, on the branch:
   `gh pr create --base feat/<topic> --body-file .scratch/<hand-off>.md`, and once it is green,
   `gh pr merge <number> --merge --body-file .scratch/<hand-off>.md`, never by the web interface,
   whose merge commit drops the hand-off. Then, on the `feat/` branch, `git fetch origin` and
   `git merge --ff-only origin/feat/<topic>`.
4. Rework it as often as needed: rebase it, regroup its commits, or drop what did not hold,
   with `git rebase -i --autosquash --rebase-merges`. `--autosquash` folds in the fixups a review
   leaves, as the `review` skill says; without `--rebase-merges` the merges flatten, and their
   hand-offs with them. A pull request still open against the `feat/` branch is rebased onto the
   rework.
5. After a rework, review the whole branch at its head
   ([`AGENTS.md` §5](AGENTS.md#5-quality-bar--definition-of-done)) and name that commit on the
   `Reviewed at:` line of the pull request of step 6, which
   [`review-binding.yml`](.github/workflows/review-binding.yml) reads on every push and edit.
6. Test it in the pipelines: `git push -u origin feat/<topic>`, then open a draft pull request,
   `gh pr create --draft --base main`, which runs them on every push against what the merge
   into `main` would produce. A branch kept without a pull request, such as an experiment, runs
   them by hand: `gh workflow run checks.yml --ref feat/<topic>`. A rework already pushed goes out
   with `git push --force-with-lease`.
7. Make the pull request ready: [`scripts/ready-for-main.sh`](scripts/ready-for-main.sh) refuses
   a fixup steps 4 to 6 left, takes up the remote's `main`, and numbers the draft ADRs
   ([ADR-0002](docs/adr/0002-decisions-verified-by-tests.md)); then the push it names and
   `gh pr ready`. What `main` gains later is taken up the same way, and an ADR it gains moves the
   branch's numbers on ([mechanics](docs/adr/README.md#mechanics)).
8. Once the pull request into `main` has merged as a merge commit, take it up and drop the branch:
   on it, [`scripts/drop-merged-branch.sh`](scripts/drop-merged-branch.sh) refuses a branch
   `origin/main` lacks, takes up the remote's `main`, and deletes the local branch; then the
   `git push origin --delete feat/<topic>` it names.

## Repository settings

Some of this project's rules cannot be enforced by files in the repository
([`AGENTS.md` §6](AGENTS.md#6-project-rules)). They are settings on the GitHub repository itself,
configured once by a maintainer and worth re-checking after a repository move or transfer:

- a **ruleset on `main`** that requires pull requests and allows the merge method **merge commit**
  only, since squash and rebase discard the merges a `feat/` branch carries and the commits it
  was reworked into ([`AGENTS.md` §6](AGENTS.md#6-project-rules)); requires the
  <!-- required-checks begin — compared with the workflow's jobs by scripts/check-docs.sh -->
  `docs`, `traceability`, `devcontainer`, `actions`, `sensors`, `shell`, `rust`,
  `client-windows`, `client-macos`, `s3-store`, and `postgres-store`
  <!-- required-checks end -->
  jobs as required status checks: every job of the **Checks** workflow, the Client's Windows and
  macOS jobs and the S3 and PostgreSQL stores' tests against a real store among them (rulesets
  list checks by their job name); requires a branch up to date with `main` before it merges, so
  that the checks see what the merge produces, such as two ADRs proposed in parallel under one
  number; and blocks force pushes and branch deletion. Nobody bypasses this ruleset, the maintainer
  included;
- a second **ruleset `main-review`** on `main` that requires one approval and **a review from Code
  Owners**, so the two artifacts only a human decides,
  [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md) and everything under [`docs/adr/`](docs/adr/)
  ([`AGENTS.md` §3](AGENTS.md#3-adr-rules)), cannot be merged without the owner named in
  [`.github/CODEOWNERS`](.github/CODEOWNERS); it allows the merge method merge commit only, and
  the admin role bypasses it for pull requests only, so the maintainer merges their own reviewed
  pull request without an approval GitHub would refuse them
  ([ADR-0007](docs/adr/0007-the-harness.md), clause 4);
- enable **secret scanning with push protection**, the only mechanical backstop behind the secrets
  rule ([`AGENTS.md` §7](AGENTS.md#7-secrets)), which is otherwise carried by review alone;
- enable **private vulnerability reporting** (see [`SECURITY.md`](SECURITY.md)).
- after the first release that pushes the Server's image, set the package
  **`opamp-fleet-server`** on `ghcr.io` to **public** and linked to this repository: a package a
  workflow publishes for the first time is private, and operators pull the image without an account
  ([ADR-0034](docs/adr/0034-releases-attestations-and-the-server-image.md)).

[`scripts/sensor-repo-settings.sh`](scripts/sensor-repo-settings.sh) reads these settings
through `gh` and reports each one that differs from this list. It runs on a human's
instruction ([ADR-0002](docs/adr/0002-decisions-verified-by-tests.md)).

## Template

- **Template release:** v0.5.0 — the release of [NUC](https://github.com/hivevm/nuc) this
  repository carries; a project moves the line when it takes up a later one
  ([ADR-0005](docs/adr/0005-template-releases.md)).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the workflow (specification- and ADR-driven, small
reviewable changes) and [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) for the community standards we
expect of everyone taking part. Security issues: please follow [`SECURITY.md`](SECURITY.md) instead
of opening a public issue.

## License

Released under the Apache License 2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
