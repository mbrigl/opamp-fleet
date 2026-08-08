# Changelog

Operator-facing changes to the Server and the Client — what a running deployment has to be told
about, in particular anything that must be edited or moved before an upgrade. The reasoning behind
each change lives in the ADR it names ([`docs/adr/`](docs/adr/)); this file says only what to do.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Versions are the ones
[ADR-0009](docs/adr/0009-version-derivation-and-baking.md) derives from `version/*` tags.

> **Where this file starts.** Entries begin with ADR-0021. The work between `version/0.1.0` and
> that point — package delivery, Selector-targeted packages and Configurations, the Client's own
> self-update, and the rest — is not backfilled here; it is in the git log and in the ADRs.

## [Unreleased]

### Changed — breaking

- **`accepts_packages` is no longer a `[[supervisor]]` key** and a configuration still carrying it
  **fails at startup**. Whether a Managed Process takes Server-offered package updates is now
  decided by how its program is named
  ([ADR-0021](docs/adr/0021-supervisor-directory-and-path-implied-package-consent.md)):

  | `binary` / `command` | Meaning |
  |---|---|
  | a bare file name (`otelcol-contrib`) | the program lives in `<supervisor_dir>/<name>/program/`, a directory the Client owns — it **takes** package updates |
  | an absolute path (`/usr/local/bin/otelcol`) | the machine's program — it is supervised but never written to |
  | anything else (`./x`, `bin/x`) | a startup error |

  **To keep updates working on a host that had `accepts_packages = true`:** move the program into
  `<supervisor_dir>/<name>/program/`, reduce the configured path to its bare file name, and delete
  the `accepts_packages` line. **To stop at supervision instead:** delete the line and leave the
  absolute path. Either way it is one edit per host — the Client will not start until it is made.

  This also fixes the case that motivated the change: with an absolute path into a directory the
  Client cannot write (`/usr/local/bin` under a non-root Client), an update could be configured but
  never succeed, and it failed at rollout time on every matched host rather than at startup on one.

  **On Windows, "absolute" means the path names a drive.** `\Program Files\otelcol\otelcol.exe`
  carries a root but no drive, so it resolves against whichever drive the process happens to be
  on — it used to be spawned that way and is now refused at startup, with a message saying what is
  missing. Write `C:\Program Files\otelcol\otelcol.exe`.

- **A bare program name is no longer looked up in `$PATH`.** `command = "fluent-bit"` used to mean
  "find it on the path" and now names a file in that Supervisor's `program/` directory. This is
  silent — the process starts from a different path rather than erroring — so check any block whose
  program is not an absolute path. The startup log states, per Supervisor, which program it resolved
  to and whether packages are accepted.

### Added

- **Released builds of the Client, one archive per platform.** A release publishes
  `opamp-client-<version>-<os>-<arch>.7z` for Linux and macOS on `x86_64` and `aarch64`, and Windows
  on `x86_64`, together with `SHA256SUMS`
  ([ADR-0025](docs/adr/0025-release-pipeline-and-artifacts.md)). Until now there was nothing to
  install but a build of your own.

  **The version is `[workspace.package] version` in `Cargo.toml`**, and the pipeline creates the
  `version/*` tag from it ([ADR-0026](docs/adr/0026-version-from-cargo-toml.md)) — so a release is
  "merge the bump, run the workflow", and no tag is typed by hand. It refuses rather than guesses:
  a tag that already names another commit is never moved, and a binary that does not report the
  version its artifacts are named after fails the run.

  **Each archive is also a package artifact**: it holds the Client under the name the install layout
  gives it, so the file is uploaded exactly as downloaded and the published SHA-256 is the one an
  Agent verifies. When you hand one to a Server for a Client self-update, `?version=` must carry the
  **full** version the binary reports — `1.2.3+a1b2c3d`, build metadata included — because the
  staged binary's self-check compares the two exactly. The file name carries only the base version;
  the full string is in the release notes and in `client --version`.

- **`supervisor_dir`** (optional, top-level) places the per-Supervisor directories; the default is
  `<state_dir>/supervisors`, which is where they have always been. Set it to keep the Managed
  Processes' programs off a `noexec` mount, or off a volume sized for state rather than for a few
  hundred megabytes of Collector. Moving it on a running host leaves the old tree behind —
  `instance-uid` included — so each Supervisor re-registers as a **new** Agent on the Server;
  nothing migrates automatically.

- **`${supervisor_dir}` and `${config_dir}` in a `command` Supervisor's `args`, `working_dir`, and
  `env` values** ([ADR-0022](docs/adr/0022-supervisor-path-placeholders-in-process-arguments.md)),
  so a Foreign Agent's command line is derived from the same place the Client derives it from:

  ```toml
  args = ["-c", "${config_dir}/fluent-bit-conf"]
  ```

  An absolute path still works and is still wrong the moment `supervisor_dir` moves or the
  Supervisor is renamed — the process then starts happily on a file nobody writes to, with nothing
  reporting a problem. The shipped example carried exactly that mistake and now uses the
  placeholder. Any other `${…}` is passed to the process untouched, so an agent's own variable
  syntax keeps working; the flip side is that a misspelled placeholder is handed over rather than
  refused. The program itself (`binary`, `command`) is never substituted.

- **`opamp-package-sign pack`** builds a package artifact from a single-file program and prints its
  SHA-256, and **`opamp-package-sign sha256`** hashes an existing one — the value
  `PUT /api/v1/packages/{name}/source` needs for an artifact the Server will not hold
  ([ADR-0018](docs/adr/0018-packages-imported-from-a-url.md)). Until now the project could open the
  two container formats but gave an operator no supported way to produce one, and an encrypted
  `.7z` in particular had no answer at all.

  ```console
  $ opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail
  $ opamp-package-sign pack --format 7z --archive-key "$KEY" --out promtail-3.0.0.7z ./promtail
  ```

  The member inside the archive is named after the packed file, which is what a Supervisor looks
  for; `--program-name` covers an upstream build whose file name differs. A `.tar.gz` is
  reproducible — modification time, owner, and group are zeroed — so repacking the same program
  does not produce a new hash and therefore no rollout. **There is no `zip`, and adding one is not
  a matter of a flag:** an artifact that is neither gzip nor 7z is taken to *be* the program, so a
  `.zip` would be installed over the binary unopened.

- **A user manual** at [`docs/manual/`](docs/manual/README.md) — Server and Client documented
  option by option, plus an end-to-end [rollout walkthrough](docs/manual/rollout.md) that installs
  and configures a Foreign Agent entirely from the Server.

- **`program_path` in a `[[supervisor]]` block delivers an agent that is more than one file**
  ([ADR-0023](docs/adr/0023-multi-file-packages.md)). An executable plus the shared objects it
  loads — Fluent Bit is the case — could not be a package before, because exactly one archive
  member was installed. Naming where the program sits inside the package unpacks the whole archive
  instead:

  ```toml
  [[supervisor]]
  type = "command"
  name = "fluent-bit"
  command = "fluent-bit"            # unchanged: the bare name is still the consent
  program_path = "bin/fluent-bit"   # where the program sits inside the package
  ```

  The tree lands in `<supervisor_dir>/<name>/program/tree/`, and the one it replaced is kept as
  `program/tree.rollback` until the new one has survived `apply_grace_secs` — put back **whole** if
  it has not. The path is matched from its end, so the version-named directory a release wraps
  everything in needs no mention and the value stays right at the next release.

  **Without `program_path` nothing changes**: one member, one file, same layout, same rollback.

  Unpacking a tree means the archive names paths, so every member is checked before anything is
  written and one bad member refuses the whole archive: a `..` or absolute path, a symbolic or hard
  link, more than 10 000 members, or more than 2 GiB unpacked. A `.tar.gz` carries file modes and
  is the right format for a tree; a `.7z` is opened too, but only the program is made executable.

### Fixed

- **On macOS, a Client installed as a service could never update itself** — every offer was refused
  with "this Client does not run from a versioned install layout", and a torn `current` pointer was
  never repaired either. The service is registered against `<root>/current/client` (ADR-0010), and
  asking the operating system what is running answers with that path on macOS and with the version
  directory behind it on Linux; only the second shape says where in the layout the binary sits. The
  path is now resolved before the layout is looked for, so both platforms answer the same. Nothing
  to change on a host: an affected Client picks its updates up as soon as it runs this version.

- **A Client that had just updated itself reported the update as failed, and then downloaded the
  artifact again — over and over.** After the restart the Server keeps offering the package until
  the Agent reports a terminal status for it; the Client answered "the offered version is the one
  already running" as an *error*, which is not terminal, so the offer came back and the whole
  artifact was fetched again every couple of seconds for as long as both ends were up. On a fleet
  that is a self-inflicted flood against the Server, and a successful self-update that shows as
  `InstallFailed` in the fleet view. The version already running is now reported `Installed`, which
  is both true and what the Baseline asks for: an Agent that already has the offered version "does
  not need to do anything". No configuration changes; a Client that was in this state leaves it as
  soon as it runs this version.

- **The fleet view now shows why an Agent refused a package offer**, in the new `package_error`
  field of `GET /api/v1/agents`. An offer refused outright has no package status to carry the
  reason — which is exactly what happens when the Client's own Agent is offered a package
  `[self_update]` did not name (ADR-0020) — so the reason was reported by the Client, stored by the
  Server, and shown nowhere. It is now also logged.

### Changed

- A Supervisor's package downloads are staged in its own directory
  (`<supervisor_dir>/<name>/packages/`) instead of `<state_dir>/packages/`, which the Client's own
  Agent keeps using. Any `*.staged` file left in the old location by an interrupted download is
  orphaned and can be deleted.
- A package artifact that is a bare program is now **moved** into place rather than copied, saving a
  second full write of it. An artifact that is an archive is still unpacked, so an upstream
  Collector release (`.tar.gz`) is unaffected.
