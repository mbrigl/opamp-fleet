# ADR-0041: A Client running as a service logs to a file, on every platform

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

**On Windows, a Client installed as a service has no logs at all.** Everything this Client says goes
to stderr through one `tracing_subscriber::fmt` layer installed in
[`main.rs`](../../crates/client/src/main.rs), and the Windows SCM discards a service's stderr.
[ADR-0010](0010-client-os-service-and-cli.md) recorded the consequence when it decided the service
integration — *"The SCM discards the service's stderr, so Windows service logs are effectively lost
for now"* — and left *"log-to-file for service mode (Windows especially)"* as a follow-up.
[`README.md`](../../README.md) repeats it to this day in the manual smoke checklist.

Windows is not a lesser target here. [ADR-0030](0030-one-service-name-on-every-platform.md) gave it
the same service name as every other platform, [ADR-0027](0027-interactive-install-writes-the-first-configuration.md)
gives it the same interactive install, and it has an elevation model of its own that ADR-0010 went to
some trouble to satisfy. It is a supported platform on which a failed start currently produces
nothing anyone can read.

**The obvious substitute does not cover the case.** Since [ADR-0036](0036-agents-report-their-own-telemetry.md)
the Client can bridge its own `tracing` output to OTLP logs — but only to a destination *the Server
offers*, over a connection that must already be up. The failures that most need a log are exactly
the ones where it is not: a malformed `client.toml`, a certificate the Client will not load, an
endpoint that refuses it, a self-update that will not come back. Own-telemetry is for a Client that
is working; this is for one that is not.

**Three forces bound the answer.**

- **The log must be bounded.** This runs on a hundred machines that nobody watches. An unbounded
  file on a fleet host eventually fills a disk, and a monitoring agent that takes the host down is
  worse than one that says nothing.
- **The destination is not known when logging starts.** `tracing` takes one subscriber per process
  and [`main.rs`](../../crates/client/src/main.rs) installs it before `cli::parse()` runs, so the
  instance — and therefore the state directory the file belongs in — is not known yet. ADR-0036 hit
  this exact wall and solved it with a `reload::Layer` slot filled later.
- **A rotation implementation is a dependency or a hazard.** Rolling files, pruning old ones, and
  surviving concurrent writes is not code worth writing here, and getting it subtly wrong in the
  component that is supposed to explain failures is its own failure mode.

## Decision

We will write the Client's own log to a **rotating file in its state directory whenever it runs as a
service**, on every platform, bounded by a retention it cannot exceed, and switchable from
`client.toml`.

1. **Service mode turns it on; the foreground does not.** `run --service` writes the file; an
   ordinary `opamp-fleet-client run` in a terminal keeps writing to stderr alone, because somebody
   is already reading it there. The condition is "no terminal is watching", which is exactly what
   service mode means.

2. **Every platform, not Windows alone.** systemd and launchd do capture stderr, so on Linux and
   macOS the file duplicates what `journalctl` and `log show` already hold — deliberately. ADR-0030
   made the same trade for the service name: one behaviour an operator can carry between platforms is
   worth more than the bytes a platform-specific rule would save, and the support instruction becomes
   one sentence instead of three. The duplicate is bounded by point 4 and can be switched off by
   point 5.

3. **It lives in the state directory, beside the rest of the instance's state.** ADR-0010's layout
   already gives every instance a private directory that survives updates and that `uninstall`
   deliberately does not delete, which is precisely the lifetime a log wants — a log that vanished
   with the failed install would be missing when it is needed. Elastic Agent puts its own logs under
   its data directory for the same reason.

4. **Bounded by rotation and retention, always.** The file rotates **daily** and the Client keeps a
   fixed number of days (default 7), deleting the rest. There is no configuration that removes the
   bound — `keep = 0` is refused rather than read as "forever", because "forever" is the setting
   that fills a disk on a host nobody is looking at.

   This bounds **age and file count, not bytes**, and that is a conscious narrowing of Elastic's
   model, which rotates at a size (10 MB by default) and keeps `keepfiles`. A day is bounded here in
   practice because the Client is quiet — a report every 30 s at `info` — and because its one
   pathological case, a Server it cannot reach, backs off exponentially rather than looping hot. If
   a real deployment ever produces a day large enough to matter, a size cap is a follow-up, and it is
   an easier one to add than a retention policy would have been to remove.

5. **`[logging]` in `client.toml`, and it is the machine's.** `dir` moves the file, `keep` changes
   how many days are kept, and `enabled = false` turns it off for an operator whose platform already
   collects stderr and who does not want the copy. It is **not** remotely configurable and never
   arrives over the wire: a Server that could redirect or silence a Client's own log could hide its
   own effects, and the Client's configuration is the machine's (the rule ADR-0014 draws).

6. **The file layer is dropped into a reload slot, like the OTLP bridge.** `main.rs` keeps installing
   stderr first and reserves a second empty `reload::Layer`; once `cli::parse()` has named the
   instance and the layout is known, the file layer is loaded into it. The same mechanism ADR-0036
   already established, for the same reason — and it means the messages emitted before that point
   still reach stderr rather than disappearing into an ordering bug.

7. **`tracing-appender` does the rolling.** It is `tokio-rs/tracing`'s own appender, the sibling of
   the `tracing-subscriber` this project already depends on, and it provides daily rotation with a
   retained-file limit directly. Writing this by hand to avoid one dependency would put custom file
   rotation inside the component whose job is to explain failures.

8. **A log that cannot be written never stops the Client.** If the directory is not writable — a
   permission the installer did not grant, a full disk — the Client says so on stderr once and runs
   without the file. A monitoring agent that refuses to start because it could not open its own log
   has turned a diagnostic into an outage.

## Alternatives considered

- **The Windows Event Log**, the platform-native answer, which Elastic Agent supports as an output
  (`syslog`, `file`, `stderr`, `eventlog`). Rejected on cost and on evidence: it needs an event source
  registered at install time and a message resource for anything better than raw text, it is a second
  code path that only one platform exercises, and the OpenTelemetry Collector's own Event Log output
  is reported as *"somewhat broken"* by the people who use it. A file is inspectable with the tools
  everyone already has, and is identical on all three platforms.
- **A file only on Windows**, since Linux and macOS already capture stderr. Genuinely smaller, and the
  duplicate storage of point 2 disappears. Rejected: it makes the answer to "where are the logs"
  depend on the platform, which is what ADR-0030 argued against for the service name, and it leaves
  the operator who runs the Client in a container — where neither journald nor an SCM is present —
  with the same hole this ADR is closing.
- **Rely on the OTLP own-logs bridge** (ADR-0036) and require operators to configure a destination.
  Rejected in the Context: it needs a Server that is reachable and an offer already made, so it cannot
  explain the failures that prevent exactly that. It remains the right channel for a healthy fleet.
- **Rotate by size instead of by day**, as Elastic does. Considered and narrowed to point 4's daily
  rotation: size rotation is the stronger bound but `tracing-appender` does not offer it, so taking it
  means either a second dependency or hand-written rolling. The Client's volume does not warrant either
  today, and the follow-up is cheap.
- **Let `keep = 0` mean unlimited retention**, the convention many tools use. Rejected: on a fleet host
  the unbounded setting is the one that eventually causes an incident, and it should not be reachable by
  typing a zero.
- **Make logging remotely configurable**, so an operator can raise a Client's log level from the Server
  while chasing a problem. Attractive, and the natural extension. Rejected here: it is the Client's own
  configuration, which ADR-0014 keeps on the machine, and a Server able to silence or redirect a
  Client's log could conceal what it did. If it is ever wanted, it wants its own decision.
- **Write the log into the version directory rather than the state directory**, keeping everything one
  install produced together. Rejected: a self-update would then scatter the history across version
  directories and ADR-0019's rollback would take the log with it — the state directory is the thing
  that deliberately outlives both.

## Sources / Prior art

- **[Elastic Agent logging](https://www.elastic.co/docs/reference/fleet/elastic-agent-standalone-logging-config)**
  and [installation layout](https://www.elastic.co/guide/en/fleet/master/installation-layout.html) — the
  closest working precedent. It writes its own logs under its data directory
  (`C:\Program Files\Elastic\Agent\data\elastic-agent-*\logs\elastic-agent-*.ndjson` on Windows), can
  emit to `syslog`, `file`, `stderr` or `eventlog`, and rotates **by size** — 10 MB by default — keeping
  a configurable number of files, with names carrying the date and a sequence suffix. Points 3 and 4
  follow it; point 4 deliberately takes the age-and-count bound instead of the size one, and says why.
- **[OpenTelemetry Collector issue #5300, "HowTo: Telemetry Logging to File when running as windows
  service"](https://github.com/open-telemetry/opentelemetry-collector/issues/5300)** — this exact
  problem, filed against the Collector, and the confirmation that it is a general one rather than a
  peculiarity of this project. The Collector's answer is a configured output path
  (`service::telemetry::logs::output_paths`), which is the shape point 5 takes. The same discussion is
  where the Collector's Windows Event Log output is described as unreliable, which is the evidence
  behind rejecting that route.
- **[`tracing-appender`](https://crates.io/crates/tracing-appender)** `0.2.5` — `tokio-rs/tracing`'s own
  rolling file appender, beside the `tracing-subscriber` `0.3.23` already in the tree. Daily rotation
  with a retained-file limit out of the box; no size-based rotation, which is what narrowed point 4.
- This repository: [ADR-0010](0010-client-os-service-and-cli.md), which recorded the gap and reserved
  this follow-up, and whose layout point 3 uses; [ADR-0036](0036-agents-report-their-own-telemetry.md),
  whose `reload::Layer` slot point 6 copies and whose OTLP bridge this complements rather than replaces;
  [ADR-0030](0030-one-service-name-on-every-platform.md), whose one-behaviour-per-platform argument
  point 2 reuses; [ADR-0014](0014-server-driven-connection-settings.md), for the rule that the Client's
  own configuration stays on the machine.

## Consequences

- Positive: a Windows service failure becomes readable. The platform that had nothing gets the same
  answer as the others, and the README's manual checklist stops having to warn that logs are lost.
- Positive: the answer to "where are the logs" is one sentence on every platform, including in a
  container, where neither journald nor an SCM is present.
- Positive: it covers the failures own-telemetry structurally cannot — the ones that happen before or
  instead of a working connection.
- Negative / trade-offs: on Linux and macOS the file duplicates what the platform's journal already
  holds, costing disk for a copy most operators will never read. Bounded by point 4 and switchable by
  point 5, but it is a real cost paid by every host to keep one behaviour uniform.
- Negative / trade-offs: **the bound is on days and files, not bytes.** A pathologically chatty day
  produces a large file, and nothing stops it before the next rotation. The reasoning in point 4 is
  that the Client is quiet and backs off when it cannot reach the Server; if that reasoning is ever
  wrong, it is wrong on a host nobody is watching, which is where it hurts most.
- Negative / trade-offs: a new dependency in the Client, on a crate whose whole job is writing files
  in the background. It is from the same project as the logging stack already in use, which is the
  reason it is acceptable rather than an argument that it is free.
- Negative / trade-offs: logs on disk are a data question. Anything the Client logs about a
  configuration or a package now persists on the host for a week rather than living in a journal an
  operator's policy already governs. Nothing secret is logged today, and this decision makes it more
  important that it stays that way.
- Follow-ups: a size cap beside the day count, if a deployment ever produces a day worth capping;
  whether `uninstall` should offer to remove a log directory it created, which is the same asymmetry
  ADR-0027 already noted for the configuration file it writes; and remotely raising a Client's log
  level while chasing a problem, which is a decision of its own for the reasons this one rejects it.
