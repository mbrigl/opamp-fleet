# ADR-0022: A Foreign Agent is pointed at its own directory by placeholder, never by a path an operator has to keep in sync

- **Status:** 🟢 accepted
- **Date:** 2026-08-07
- **Deciders:** Markus Brigl

## Context

[ADR-0021](0021-supervisor-directory-and-path-implied-package-consent.md) made the per-Supervisor
directory relocatable (`supervisor_dir`). That created a hole it did not close: a Foreign Agent is
told where its configuration is **through its own command line**, and those arguments are passed
verbatim (`CommandSettings::args` → `ProcessSpec::args` in `crates/client/src/supervisor/command.rs`).
The example in [`config/supervisor.toml`](../../config/supervisor.toml) is the demonstration:

```toml
command = "/opt/fluent-bit/bin/fluent-bit"
args = ["-c", "/var/lib/opamp-fleet/client/default/state/supervisors/fluent-bit/config/fluent-bit-conf"]
```

Set `supervisor_dir`, and the written configuration moves while that argument does not. Fluent Bit
then starts — successfully — on a file the Server no longer writes to. Nothing errors: the process
is healthy, its Agent reports healthy, and the fleet's configuration silently stops arriving. An
operator who has just relocated a directory is not looking for that.

The Collector plugin has no such problem: its `--config` flags are built from the Supervisor's
`config_dir`, so they follow the directory wherever it goes. Only the operator-written command line
of a Custom Supervisor can drift, and it can drift in three ways — a path that is wrong from the
start, one that `supervisor_dir` moves out from under, and one that a *rename* of the Supervisor
breaks.

Two constraints shape what may be done about it. ADR-0008 fixes the configuration as hand-edited
TOML, so whatever this adds must be readable in a file and fail loudly when it is wrong. And
ADR-0021 made **the written shape of the program's path** the whole of a Supervisor's consent to
package updates — which means the program path in particular must not be built by substitution,
or the consent would depend on a value the file does not show.

## Decision

We will substitute a small, fixed set of **placeholders naming the Supervisor's own directories**
into a Custom Supervisor's `args`, `working_dir`, and `env` values — and never into the program
path.

1. **Two placeholders, both directories the Client alone decides the location of:**

   | Placeholder | Expands to |
   |---|---|
   | `${supervisor_dir}` | `<supervisor_dir>/<name>` — everything that Supervisor owns |
   | `${config_dir}` | `<supervisor_dir>/<name>/config` — where the received configuration's entry files are written (ADR-0012) |

   The fluent-bit example becomes `args = ["-c", "${config_dir}/fluent-bit-conf"]`, which cannot
   drift: both halves are now derived from the same place.

2. **The program is excluded**, in `binary` and `command` alike. This follows systemd, where
   specifiers are expanded in a unit's arguments but explicitly *not* in the executable path — and
   here there is a second reason: under ADR-0021 the shape of that value decides whether the Agent
   declares `AcceptsPackages`, so a substituted program path would make a fleet-visible capability
   depend on something the file does not literally say.

3. **An unrecognized `${…}` is left verbatim.** It is not an error and not silently emptied. A
   Foreign Agent's own configuration language may use the same syntax — Fluent Bit's does — and a
   Client that ate or rejected those would break a working deployment to catch a typo. The names are
   substituted; everything else is the process's business.

4. **Substitution happens once, at startup**, on the values as written. Nothing re-expands when a
   configuration arrives, because none of these paths change while the Client runs.

## Alternatives considered

- **Default `working_dir` to the Supervisor's own directory**, so relative arguments simply resolve
  there. Fewer moving parts and no new syntax — this was the first proposal — but it changes the
  meaning of every existing `command` block silently, and it points a foreign process's *working
  directory* at a tree whose layout the Client owns. A process that drops a file beside itself would
  litter that directory, and one that writes `config` or `program` would collide with it. Buying a
  smaller diff with a shared directory is the wrong trade.
- **Resolve a relative `working_dir` against the Supervisor's directory** (reusing ADR-0021's rule
  on a second key) — attractive for its consistency, but ADR-0021's bare name resolves into
  `program/`, and a working directory wants the Supervisor root. The same-looking rule would mean
  two different things depending on the key, which is worse than a second mechanism that admits it
  is one.
- **Export the paths as environment variables** (`OPAMP_CONFIG_DIR`, as systemd's `StateDirectory=`
  exports `$STATE_DIRECTORY`) — nothing expands variables in `argv`, so it would not reach the case
  this exists for. Worth revisiting for agents that read their environment, but not as the answer
  here.
- **Leave it, and document that an absolute path must be kept in sync with `supervisor_dir`** — the
  failure is silent and the two settings live in the same file, so the only thing keeping them
  consistent would be the operator's memory.
- **A general templating engine** over the whole configuration — far past any present need, and it
  turns a configuration file into a program.

## Sources / Prior art

- **systemd unit specifiers** — `%S` (state directory), `%t` (runtime directory) and friends expand
  in a unit's arguments, and the manual states the executable path itself may not contain them: the
  same split this ADR draws, for a related reason.
  <https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html>
- **systemd `StateDirectory=`** creates the directory and exports `$STATE_DIRECTORY` — the
  environment-variable alternative, rejected above because `argv` is not expanded.
- **OpenTelemetry `opampsupervisor`** writes the merged Collector configuration to a directory it
  owns and passes it with `--config`, exactly as this project's Collector plugin does; it has no
  Foreign-Agent equivalent, and expansion in its own configuration is still an open request
  upstream (`opentelemetry-collector-contrib#36269`).
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>

## Consequences

- Positive: the silent failure disappears. A Foreign Agent's configuration path is derived from the
  same value the Client derives it from, so relocating `supervisor_dir` — or renaming a Supervisor —
  cannot leave the process reading a file nobody writes.
- Positive: the example configuration stops shipping a hard-coded absolute path that is wrong for
  every host that does not use the defaults.
- Negative / trade-offs: a typo in a placeholder name (`${config-dir}`) is passed through to the
  process rather than refused, which is the opposite of how ADR-0008 treats an unknown *key*. That
  is deliberate — the alternative breaks agents whose own syntax overlaps — but it is an
  inconsistency in the configuration's behaviour, and the documentation has to say so where the
  placeholders are listed.
- Negative / trade-offs: two ways to spell the same path now exist, since an absolute path still
  works. The example leads with the placeholder; nothing forces it.
- Negative / trade-offs: this is a second mechanism for "a path inside the Supervisor's directory",
  beside ADR-0021's rule for the program. They are deliberately not unified (see the alternatives),
  but a reader meets both and has to learn which applies where.
- Follow-ups: whether the same placeholders should be available in the Collector plugin's extra
  `args`, which today has no need for them; and whether a Foreign Agent that reads its environment
  should additionally be handed these paths as variables.
