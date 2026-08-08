# ADR-0027: The first configuration is written by an interactive install — asked once, never overwritten, validated before the service is registered

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

## Context

[ADR-0010](0010-client-os-service-and-cli.md) gave the Client the lifecycle an operator needs —
`client service install | uninstall | start | stop | status`, one CLI over the `service-manager`
crate, writing the systemd unit (Ubuntu and RHEL are both systemd), the launchd `LaunchDaemon`, and
the Windows SCM registration; [ADR-0020](0020-client-self-update.md) closed the last platform gap by
configuring the SCM recovery actions the `sc.exe` backend discards. That half of the operator's
question — *does this work the same on all four systems* — is answered.

The **configuration** half is not, and ADR-0010 left it out deliberately: the Client is
file-configured ([ADR-0008](0008-toml-configuration.md)) and the installed unit carries the config
*path*, never the config. On a fresh host that path names nothing, and nothing says so.
`ClientConfig::load` returns the defaults when the file is absent, so `service install` succeeds, the
service starts, and the Client dials `ws://127.0.0.1:4320/v1/opamp` — the development default —
forever. The host is not broken and it is not managed either. That silent half-state is the defect
this decision closes; it is the same reason ADR-0020 refused to let a Windows self-update look like
it worked on two platforms out of three.

Three forces shape the answer.

**There is nothing on the host to copy from.** [ADR-0025](0025-release-pipeline-and-artifacts.md)
packs each release asset around the binary the Client looks for (`client` / `client.exe`), precisely
so a release asset is also a valid package artifact for ADR-0020. `config/client.toml` is a document
in this repository, not a shipped file. An operator who follows the documented download path has a
binary and no example.

**`install` already runs unattended.** It is the command an Ansible play, an MDM profile, or an MSI
wrapper invokes. A command that blocks on stdin there does not fail — it hangs, which is worse. Any
interactivity has to be something the operator asks for, never something they discover.

**A first configuration contains a secret.** The endpoint is useless without the credential that goes
with it ([ADR-0013](0013-opamp-endpoint-authentication.md)), and asking for a bearer token or a
password puts it on the screen unless the terminal is told otherwise. Suppressing the echo is
platform terminal control (`termios` on Unix, `SetConsoleMode` on Windows), not string handling.

None of this overturns ADR-0008. What gets written is a *starting* file the operator keeps editing by
hand; the Client gains no second configuration mechanism, and no environment fallback. But a new CLI
surface and a new dependency are architecture-relevant under `AGENTS.md` §3, which is why this is an
ADR and not a commit.

There is no upstream answer to adopt wholesale. The OpenTelemetry `opampsupervisor` has a
hand-written configuration file and no scaffolding command at all. Elastic Agent is the closest prior
art and is instructive in both directions: `elastic-agent install` prompts for confirmation and for
Fleet enrollment by default, and offers `--non-interactive` for automation — but it also *overwrites*
`elastic-agent.yml`, and `--force` exists to skip the confirmation for that. Overwriting is exactly
the behaviour we must not copy on a file that holds a credential an operator typed.

## Decision

We will let `client service install` write the Client's **first** configuration file — asked
interactively only when the operator asks for it, never overwriting a file that exists, and validated
before anything is registered.

1. **Interactivity is an opt-in flag on `install`, not a new command.** `client service install
   --interactive` runs the questionnaire; without the flag `install` behaves exactly as it does
   today. The default stays non-interactive because that is what every existing scripted invocation
   already relies on. `--interactive` on a stdin that is not a terminal is an **error**, not a
   silent fallback: `std::io::IsTerminal` has been in `std` since Rust 1.70 and the workspace MSRV is
   1.97, so this costs no dependency and turns "hangs a deploy forever" into a message.

2. **A file that exists is never overwritten.** If the target already exists, the questionnaire is
   skipped, `install` proceeds with that file, and prints which file it kept. Re-installing stays
   idempotent, as ADR-0010 requires of the version layout, and a second `--interactive` install can
   never eat the credential typed into the first.

3. **When the operator names no path, the file goes to `<root>/client.toml`** — inside the install
   root ADR-0010 already derives per scope and instance. That is one rule for four platforms instead
   of a new `/etc` vs `/Library` vs `%ProgramData%` policy, and the absolute path baked into the unit
   is the one just written. An explicit `--config` wins; since that flag has a default value, the two
   cases are distinguished by clap's value source, not by comparing against the default string.

4. **The questionnaire asks only what has no useful default on a fresh host:** the Server `endpoint`,
   the Agent `name`, whether authentication is used and under which scheme (ADR-0013), and — only
   when the endpoint scheme is `wss://` or `https://` — the CA file for a private CA
   ([ADR-0007](0007-dual-transport-and-tls.md)). `[self_update]` (ADR-0020) is offered last and
   defaults to **no**: consent for the Server to replace the binary that manages every other binary
   on the host is the larger grant, and it stays a deliberate answer rather than a default one.
   Everything else is written as commented defaults, in the shape of `config/client.toml`, so the
   file remains a starting point for hand-editing.

5. **The written file is treated as holding a secret:** mode `0600` on Unix; on Windows it inherits
   the install root's ACL, which for a system-scope install is already administrator-owned.

6. **It is validated before the service is registered.** The order is write → load through
   `ClientConfig::load` → lay out the versioned install → register. This preserves the existing
   "fail on a broken configuration now, not at the service's first start" property. A file that
   fails to load is left on disk and named in the error, never silently deleted — a typo is corrected
   by editing, not by answering five questions again.

7. **A non-interactive install with no configuration file warns.** Not an error — automation must not
   break — but a printed line naming the path that will be baked into the unit and saying the Client
   will run on defaults until that file exists. The silence is the bug.

8. **Hidden input comes from `dialoguer`.** It is the one part worth a dependency: the platform
   terminal control behind a password prompt is not something to hand-roll for three operating
   systems. Version 0.12.0 (2025-08-23) is current and maintained, it is MIT-licensed, and it brings
   no TLS or crypto backend, so ADR-0007's constraint on the rustls/ring stack is untouched.

## Alternatives considered

- **A separate `client config init` command** — a second entry point for work the operator is already
  in the middle of when they run `install`, and a second place where the config path must be
  resolved. Rejected for the flag; the cost is recorded under Consequences.
- **Interactive by default, with `--non-interactive` to opt out** (Elastic Agent's shape) — rejected
  because our `install` is *already* the scripted command. Elastic can afford that default because
  their install is the documented first contact with the product; flipping ours would hang every
  existing automated invocation.
- **Overwrite an existing file, with `--force` to confirm** (what Elastic does to
  `elastic-agent.yml`) — rejected: a re-install that discards a credential the operator typed is a
  worse failure than one that refuses to write.
- **Plain `stdin().read_line()`, no dependency at all** — the simplest thing, and it echoes the
  bearer token onto the screen and into the scrollback. Rejected on that point alone.
- **`inquire` 0.9.4 (2026-02-24)** — actively maintained and richer (editor prompts, derive macros
  for enum menus), which is more than four questions need, at roughly a third of `dialoguer`'s
  adoption. **`cliclack`** — a styled multi-step experience we do not need. Both remain viable if
  `dialoguer` ever stops being maintained; nothing outside the questionnaire module would change.
- **Ship `config/client.toml` inside the release artifact** — rejected: ADR-0025 keeps the asset
  installable as a package artifact, and an example copied onto the host still has to be edited
  before the service does anything, which is the actual problem.
- **Environment variables for the first configuration** — already refused by ADR-0008 and ADR-0010;
  nothing here reopens it.

## Sources / Prior art

- [Elastic Agent command reference](https://www.elastic.co/docs/reference/fleet/agent-command-reference)
  — `install` prompts for confirmation and enrollment, `--non-interactive` for automation, `--force`
  to overwrite `elastic-agent.yml` without prompting. The closest prior art, and the source of the
  two behaviours deliberately inverted here (default and overwrite).
- [`dialoguer` on crates.io](https://crates.io/crates/dialoguer) — 0.12.0, published 2025-08-23, MIT.
- [`inquire` on crates.io](https://crates.io/crates/inquire) — 0.9.4, published 2026-02-24.
- [Comparison of Rust CLI prompts: cliclack, dialoguer, promptly, inquire](https://fadeevab.com/comparison-of-rust-cli-prompts/)
  — the field, side by side.
- [`std::io::IsTerminal`](https://doc.rust-lang.org/stable/std/io/trait.IsTerminal.html) — in `std`
  since 1.70; no `atty`/`is-terminal` dependency is needed for the TTY guard.
- [Command Line Applications in Rust — Communicating with machines](https://rust-cli.github.io/book/in-depth/machine-communication.html)
  and [Improving CLIs with isatty](https://blog.jez.io/cli-tty/) — the convention that a program
  behaves differently, and predictably, when it is not talking to a person.
- The OpenTelemetry `opampsupervisor`, whose configuration is hand-written and which offers no
  scaffolding command — the same absence of an upstream answer ADR-0020 found for self-update.

## Consequences

- **Positive:** a fresh host goes from a downloaded binary to a working, registered service with one
  command, identically on Ubuntu, RHEL, macOS, and Windows. The default-endpoint service that looks
  installed and manages nothing stops being reachable by accident. The credential is typed into a
  hidden prompt instead of a command-line flag, so it never lands in shell history or a process list.
- **Negative / trade-offs:** point 3 changes what `install` does today. Without `--config` it
  currently bakes `client.toml` resolved against the working directory; it will bake
  `<root>/client.toml`. The old behaviour only ever worked when `install` was run from exactly the
  directory holding the file, which is not a property a service can depend on — but a host installed
  that way must name the path explicitly or move the file, and the change belongs in the changelog
  rather than in a release note nobody reads. One new dependency (`dialoguer`) and the terminal
  handling it brings.
  A configuration cannot be generated without installing — an operator who wants only the file must
  write it by hand or install and then uninstall. The questionnaire becomes a second place that has
  to follow the configuration schema, bounded to the handful of keys it asks about. `--interactive`
  is unavailable where it would be most tempting and least appropriate — container builds, image
  bakery, unattended provisioning — which is the point, but it means those paths still ship a config
  file by their own means.
- **Follow-ups:** whether `uninstall` should offer to remove a configuration file it wrote (today it
  deletes nothing, deliberately, and that asymmetry is now visible); whether the same questionnaire
  should back a later `config validate` / `config show`; and the operator manual's install
  walkthrough, which documents the new flag and the written path at implementation time.
