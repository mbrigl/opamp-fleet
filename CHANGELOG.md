# Changelog

Operator-facing changes to the Server and the Client — what a running deployment has to be told
about, in particular anything that must be edited or moved before an upgrade. The reasoning behind
each change lives in the ADR it names ([`docs/adr/`](docs/adr/)); this file says only what to do.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). A version is the one
`[workspace.package] version` in `Cargo.toml` names, which the release pipeline creates the
`version/*` tag from ([ADR-0026](docs/adr/0026-version-from-cargo-toml.md), superseding the
version-source decision of [ADR-0009](docs/adr/0009-version-derivation-and-baking.md)). A section
carries a date once its tag exists.

> **Where this file starts.** Entries begin with ADR-0021. The work before that point — package
> delivery, Selector-targeted packages and Configurations, the Client's own self-update, and the
> rest — is not backfilled here; it is in the git log and in the ADRs. The first four releases were
> all cut on 2026-08-09, so the dates below say less than the order does.

## [0.2.0] — unreleased

### Fixed

- **`server --version` now names the build it is, not the release it is heading for.** It printed the
  bare number from `Cargo.toml` — `server 0.1.3` — where the Client on the same commit reported
  `0.1.3-dev+ade2775`. So a Server binary could not be told apart from the release it was on its way
  to, and named no commit; two binaries of one workspace disagreed about their own version.

  Both ends now read the same helper, `opamp::version::current()`, which
  [ADR-0009](docs/adr/0009-version-derivation-and-baking.md) always required and the Server had
  opted out of ([ADR-0045](docs/adr/0045-the-version-helper-lives-in-the-shared-crate.md)). A
  development build reports `0.2.0-dev+<commit>` and a released one `0.2.0+<commit>`.

  **Anything matching the Server's `--version` output exactly has to be relaxed** — a check for
  `server 0.2.0` no longer matches a development build. Nothing else changes: the Client's string is
  what it always was, and no Agent, configuration or stored file is touched.

- **The fleet view shows an Agent's Instance UID again.** It had been the line under the agent name
  until [ADR-0033](docs/adr/0033-an-agents-type-and-its-instance-name-are-two-attributes.md) gave a
  row both the operator's name for an Agent and the type it reports, and the UID moved into the name
  link's tooltip — where nothing on a printed, screenshotted or scrolled-past row carried it. It is a
  third line in the name cell now, below the type, and one click selects the whole of it.

  That UID is what every REST call names an Agent by (`/api/v1/agents/{instance_uid}/…`), so reading
  a row in order to act on it needed the value in front of you rather than under the pointer. Nothing
  else changed: the fleet filter already searched the UID, and the row actions already carried it.

## [0.1.3] - 2026-08-09

### Changed

- **Delete in the package form removes one artifact, not the whole package.** It deletes the
  platform named in the form — the artifact the selected chip stands for — and leaves the other
  platforms of that name alone. Before, one press on a form filled from a `linux-amd64` chip took
  the `darwin-arm64` and `windows-amd64` builds with it.

  The package itself goes when its last artifact does, so nothing is left behind either. Deleting
  uninstalls nothing: an Agent keeps running what it took, exactly as retracting does
  ([ADR-0043](docs/adr/0043-a-package-is-published-before-it-is-offered.md)).

  `DELETE /api/v1/packages/{name}` is unchanged and still deletes the whole package; the form now
  sends the `?os=…&arch=…` form of it that
  [ADR-0031](docs/adr/0031-per-platform-package-variants.md) added.

### Removed

- **The ↩ Roll back button is gone from the package form.** The store still remembers the version
  each artifact replaced ([ADR-0019](docs/adr/0019-one-step-back.md)) and
  `POST /api/v1/packages/{name}/rollback?os=…&arch=…` still puts it back — only the button is
  removed. The package list still shows `0.157.0 ← 0.156.0`, so what "back" would be is still on
  screen; asking for it is now a request rather than a press.

## [0.1.2] - 2026-08-09

### Added

- **The Server can label an Agent** ([ADR-0042](docs/adr/0042-server-set-labels.md)).
  `PUT /api/v1/agents/{instance_uid}/labels` sets key/value pairs that join what a Selector matches,
  and the bundled UI has a `🏷 labels…` action on every fleet row.

  This removes the last per-host wiring. The attribute a staged rollout wants — `rollout = "canary"`
  — could only live in `[attributes]` in `client.toml`, so moving a host between rings meant editing
  a file on that host and restarting it. Now it is one API call, and it aims **both** halves of the
  targeting: the Configuration an Agent is sent and the package it is offered. A canary rollout of a
  new binary is a Selector of `rollout = canary` on the package plus a label on the hosts that should
  get it first.

  It takes effect immediately — a connected Agent is pushed what its new ring gets.

  **A label may not restate an attribute the Agent reports**; that is refused with `409`, naming the
  key. `os.type` and `host.arch` decide which artifact fits a machine and `service.name` decides
  which packages fit it at all, so a label that could outrank them would let a slip offer a host a
  binary built for another one. Fix a wrong reported value where it comes from, in that host's
  `client.toml`.

  Labels never travel to the Agent, are stored on the Server, and survive a restart. **Forgetting an
  Agent does not clear them**, so a host that comes back is in the ring it was put in; clearing them
  is its own call with an empty map. They are keyed by Instance UID, so an Agent the Server re-keys
  starts with none.

### Changed

- **A package is published before it is offered — uploading only stages it**
  ([ADR-0043](docs/adr/0043-a-package-is-published-before-it-is-offered.md)). A newly created
  package is a **draft**: its artifact is stored, its type and Selector can be set, and it reaches
  no Agent until it is released.

  ```console
  $ curl -X PUT -H 'Content-Type: application/json' -d '{"published": true}' \
         http://<server>:4320/api/v1/packages/otelcol/publication
  ```

  Uploading used to *be* the rollout: the next Agent that fitted took the artifact, even while the
  package was still half described. Five platforms' artifacts can now be uploaded, typed and aimed,
  and then released together — and the moment a rollout starts is one named act rather than the side
  effect of a file transfer.

  `{"published": false}` **retracts** it: the offer stops for Agents that have not taken the package,
  and **nothing is uninstalled** — an Agent keeps running what it installed, exactly as when a
  Selector stops matching it (ADR-0017). The protocol has no revert; this is not a recall.

  **Nothing that is already running stops.** A package stored before this state existed loads as
  published, so an upgrade withdraws no rollout in flight. And **replacing the artifact of a
  published package still distributes on upload** — that is the ordinary in-place upgrade; stage a
  replacement by retracting first.

  What changes for scripts: creating a *new* package now needs one more call. `targeted_agents`
  keeps counting what a package would reach, drafts included, because checking the aim before
  starting is what staging is for — `published` beside it is where "may the fleet have it" is read.
  In the UI, *Upload* and *Update* leave a package staged and **Offer** releases it; on a released
  package that button reads *Retract*, and the package list marks a draft.

- **The package form in the UI is one intent per button, and every one of them states the Agent
  type.** A package with no type is stored, looks uploaded, and is offered to nobody (ADR-0034) —
  the form used to make that the easiest state to produce by hand, with the type one optional-looking
  field among several and a separate button to remember afterwards.

  Three actions replace *Upload & offer*, *Use source url*, *Set selector* and *Set agent type*:

  | | what it sends |
  | --- | --- |
  | **Upload** | the chosen file as the artifact for the platform named, then the Agent type and the Selector — a complete package from one press, created if the name is new |
  | **Update** | the same without new bytes: a source url replaces the artifact with one hosted elsewhere ([ADR-0018](docs/adr/0018-packages-imported-from-a-url.md)), and the type and Selector are set either way |
  | **Offer** | whom the package reaches, and nothing else: the type that arms it and the Selector that aims it. No upload, so correcting a rollout that reached nobody is one press |

  None of them runs without an Agent type, so the UI can no longer create a package that reaches
  nobody. *Close* is gone — the **Packages** button that opens the card also closes it.

  **A package chip toggles.** Clicking one fills the form from it; clicking the selected one again
  lets go, and the form describes no package in particular. Nothing is sent and nothing is deleted
  either way. The selection lives in the list, so undoing it is a press in the list rather than a
  button standing among the ones that write.

  The **Agent type** field now offers the types the fleet actually reports, with how many Agents
  report each. The comparison is raw and has no canonical set to fall back on (ADR-0034), so a typo
  is a rollout that silently never starts — picking from the list is the spelling that matches. A
  type no Agent has reported yet can still be typed in.

  The **name** now stands alone on the first line, and is filled in rather than asked for: from the
  chosen artifact's file name, which states the package it belongs to (ADR-0025, ADR-0032), else a
  source url's last segment, else the Agent type folded into the ADR-0010 name grammar. It is
  derived again when a request is actually sent, so an artifact chosen and uploaded in one motion is
  named after itself. A name typed by hand outranks all three, and one that names a package that
  exists is not renamed by a correction to that package's type.

  Nothing about the API changed: the artifact, the source, the type and the Selector are still four
  requests over four sub-resources, and a package uploaded by script is still untyped until
  `PUT /api/v1/packages/{name}/type` is called. The type is never guessed from a name or a file
  name — that is the alternative ADR-0034 weighed and rejected.

### Fixed

- **A Client that was downgraded by hand went on reporting the version it had been updated to.**
  After a self-update, `service uninstall` followed by installing an *older* Client left the fleet
  view showing the package pill of the newer version — beside a **Version** column that correctly
  named the old one. The record of what the Client installed lives in
  `<state_dir>/installed-package.json`, and `service uninstall` deliberately deletes neither the
  install layout nor the state, so the older Client came up on top of the record its successor
  wrote.

  The wrong line in the view was the smaller half. The Server gates re-offering on the hash inside
  that record, so the host was **silently out of the rollout for good**: it believed it already ran
  the offered package and would never take it again.

  A record that does not name the version this binary *is* is now discarded at startup, with a
  warning in the log naming both versions. The Client then reports no package, the Server offers it
  again, and **the host is updated back to the published version.** That is the point — the Server
  decides what the fleet runs. To keep a host on an older Client, retract the package first
  (`PUT /api/v1/packages/{name}/publication` with `{"published": false}`,
  [ADR-0043](docs/adr/0043-a-package-is-published-before-it-is-offered.md)); retracting uninstalls
  nothing, so the host stays where it is.

  This is the Client's own package only. A Managed Process's package record is unchanged: only the
  program itself knows its version there, and it is reported by the version probe.

## [0.1.1] - 2026-08-09

### Added

- **A Client running as a service now writes its own log to disk**
  ([ADR-0041](docs/adr/0041-the-client-logs-to-a-file-in-service-mode.md)), at
  `<state_dir>/logs/`, one file per day with seven days kept.

  **On Windows this closes a hole**: the SCM discards a service's stderr, so a Client installed
  there had no readable log at all — a service that would not start left nothing behind to explain
  why. The file is written on Linux and macOS too, where it duplicates `journalctl` and
  Console/`log show`, so that the answer to "where are the logs" is the same on every platform and
  in a container, where neither exists.

  Running the Client in the foreground writes no file; stderr is already in front of you.

  It is not a replacement for `ReportsOwnLogs` (ADR-0036): that ships to a destination the Server
  offers, over a connection that must already work, and the failures most worth reading are the ones
  where it does not.

  The new `[logging]` section moves the directory, changes the retention, or switches it off:

  ```toml
  [logging]
  dir = "/var/log/opamp"   # default: <state_dir>/logs
  keep = 7                 # daily files kept, then deleted
  enabled = false          # write nothing
  ```

  **`keep = 0` is refused at startup** rather than read as "keep everything": on a fleet host the
  unbounded setting is the one that fills a disk, so switching the log off is spelled
  `enabled = false`. A log directory that cannot be written is reported and the Client runs anyway.

- **A package says how many Agents it reaches.** `GET /api/v1/packages` gains `targeted_agents`,
  and the package list in the UI shows `⚠ reaches no agent` when it is zero.

  This closes a silent failure the follow-ups of ADR-0031, ADR-0033 and ADR-0034 all named: a
  package can target nobody — through an Agent type that is unset or misspelled, artifacts for
  platforms nobody runs, or a Selector that matches no one — and none of those is an upload error.
  The package stored fine and reached no one, and nothing said so until somebody noticed the version
  had not moved.

  The count is the Server's own resolution of the offer, not a second calculation beside it, so it
  cannot claim a reach the fleet does not get. It counts the fleet **as reported so far**: a package
  staged ahead of the hosts it is meant for reads `0` legitimately, which is why it is a number to
  read rather than a rejected upload.

## [0.1.0] - 2026-08-09

### Added

- **An Agent can be forgotten** ([ADR-0039](docs/adr/0039-forgetting-an-agent.md)).
  `DELETE /api/v1/agents/{instance_uid}` drops what the Server knows about one Agent, and the
  bundled UI has a `✕ forget` action on every fleet row. A decommissioned host no longer occupies a
  row forever.

  **It reaches no host.** Nothing is stopped, nothing is uninstalled, and no credential is revoked —
  a credential here proves fleet membership, not one Agent's identity, so there is none to revoke.
  A Client that is still configured for this Server therefore reappears on its next report. To
  remove an agent for good, stop it on the machine; forgetting only tidies the view.

  It is refused with `409` while the Agent is still reporting — that is, while it is connected *and*
  something has been heard from it within the staleness budget. Forgetting drops the hashes that stop
  the Server re-offering, so a live Agent would be sent its configuration again, and a managed
  process restarts when one arrives. Stop the agent, or wait for it to fall silent.

  An Agent that was forgotten and comes back is offered its configuration, its connection settings,
  and its packages again. Packages cost nothing — the Client re-installs nothing it already has —
  but the configuration is applied again, which for a managed agent is one restart.

- **A fleet row now says when an Agent stopped talking**
  ([ADR-0038](docs/adr/0038-an-agent-that-stops-reporting-goes-stale.md)). `AgentView` gains
  `stale`, and the bundled UI shows it beside the connection pill.

  It is a second fact, not a replacement: `connected` still means "a connection carrying this Agent
  is open" — behind a Gateway, the *Gateway's* — and `stale` means nothing has been heard from the
  Agent itself for longer than its budget. `connected: true, stale: true` is exactly the gatewayed
  case, and it was invisible before.

  Only an Agent declaring `ReportsHeartbeat` can go stale: that capability is the promise that makes
  silence meaningful. The budget is the offered `heartbeat_interval_secs` times three, or
  `stale_after_secs` in `server.toml` (default 90) when no interval is offered.

  Nothing changes for a stale Agent — it keeps its configuration, its packages, and its identity,
  and its next report clears the flag. Nothing is stored and no timer runs.

- **Gateway Mode** ([ADR-0037](docs/adr/0037-gateway-mode.md)): a Client can now stand at a network
  boundary, accept OpAMP from other Clients, and carry them upstream over a small pool of
  connections. This is the last of the specification's goals to be built.

  ```toml
  [gateway]
  listen = "0.0.0.0:4320"
  upstream_connections = 10       # a cap, not a count
  [gateway.tls]                   # optional; the downstream hop's own TLS
  cert_file = "gateway.pem"
  key_file = "gateway-key.pem"
  client_ca_file = "client-ca.pem"
  ```

  Point the Clients behind it at the Gateway's address instead of the Server's — nothing else about
  them changes, and the Server sees them as the Agents they are. Both transports are served
  downstream, so a polling Client works as well as a WebSocket one.

  **The pool costs what it uses.** `upstream_connections` is a ceiling: connections are opened as
  Agents appear, so a Gateway in front of three Agents holds three. Each Agent sticks to its
  connection for as long as it lives.

  **A Gateway makes no authentication decisions.** It forwards each peer's credential upstream
  untouched. Mutual TLS is per hop: `[gateway.tls]` verifies the Agents connecting *to* it, while
  the identity it presents *to the Server* is its own, from the top-level `[tls]` or the CSR flow.
  The Gateway's upstream endpoint must be `ws://` or `wss://` — a polling connection could not carry
  the Server's pushes to the Agents behind it, and the configuration says so at startup.

  **Two limits to know.** An Agent whose Client vanishes without saying goodbye stays "connected" in
  the fleet view until someone notices: the Gateway forwards no `agent_disconnect` it did not
  receive, because that would put words in an Agent's mouth. And when a pooled connection drops, the
  Server marks every Agent that rode it disconnected until each reports again — one heartbeat
  interval where one is configured.

- **Every Agent can now report its own telemetry** — metrics, logs, and traces — to a destination
  the **Server** names ([ADR-0036](docs/adr/0036-agents-report-their-own-telemetry.md)).

  Add a `[telemetry_offer]` section to `server.toml` with any of `metrics_endpoint`,
  `traces_endpoint`, `logs_endpoint` — full OTLP/HTTP URLs **with path**, e.g.
  `https://collector.example:4318/v1/metrics` — plus optional `[telemetry_offer.headers]` for an
  access token. Each signal is offered independently, and only to Agents that declare it.

  Nothing is configured on the Client, and that is deliberate: the capability means "report to the
  destination the Server specifies", so a destination in `client.toml` would be a private extension
  wearing its name. With no offer, nothing is sent and nothing is built.

  **What it sends.** Process metrics every 30 seconds — CPU, memory, uptime — for the Client's own
  process and for each Managed Process it started; the Client's own log output as OTLP records, with
  stderr unchanged; and one span per control-loop operation that already has a lifecycle (a
  configuration being applied, a package being installed, a self-update). The OTLP Resource carries
  the Agent's identifying attributes, so one host's several Agents stay apart at the receiving end.

  **What it does not send:** a Collector's *internal* telemetry. This Client must not touch a
  Managed Process's configuration (ADR-0011), so what it reports is what it observes from the
  outside. Configure the Collector for its own internals as you would without OpAMP.

  **A cleartext destination is refused, not warned about.** `http://` beyond the loopback interface
  is rejected and reported back to the Server, because the stream carries identifying attributes and
  whatever the Client logs. The protocol explicitly permits this refusal.

- **Mutual TLS, with client certificates this Server issues itself**
  ([ADR-0035](docs/adr/0035-mutual-tls-and-the-server-issued-client-certificate.md)). Goal 17 is
  complete: the connection is encrypted, and the peer at each end can now be proved.

  **On the Server**, `[tls]` gains an optional `client_ca_file`. With it set, every request to
  `/v1/opamp` must arrive over a connection carrying a client certificate that bundle verifies.
  Client authentication stays optional at the TLS layer, because the same listener serves the REST
  API and the UI — a browser presents nothing and is unaffected.

  **Every configured proof must succeed** — this is the rule to read twice. `[auth]` alone behaves
  exactly as before. `client_ca_file` alone makes the endpoint certificate-only. **Both configured
  means both required**, not either one. So switching mutual TLS on can never widen admission; what
  it can do is lock out a host that has no certificate yet, which is why the order is: let the fleet
  enrol first, then set `client_ca_file`.

  **On the Client**, `[tls]` gains `cert_file` and `key_file` for an operator-provisioned identity —
  including the bootstrap certificate a fresh host enrols with. `ca_file` is now optional, so a
  `[tls]` section may carry only an identity and keep the public roots.

  **The Server can issue the certificates.** A new `[client_ca]` section in `server.toml`
  (`cert_file`, `key_file`, `validity_days`, default 90) makes the Server a local CA. A Client that
  has no certificate — or holds one two thirds through its life — generates a key **that never
  leaves the host**, sends a signing request, and receives the certificate as an ordinary
  connection-settings offer, which it proves by connecting with before the old one is replaced. It
  asks only when the Server declares that it signs, so a Server without `[client_ca]` is never
  asked.

  **Enrolling before enforcing** is therefore the migration: add `[client_ca]`, let the fleet come
  back with certificates (they appear in each Client's state directory as `client-cert.pem`), then
  add `client_ca_file` and, when every host is on a certificate, delete `[auth]`. A fleet that will
  run Gateways keeps `[auth]`: a Gateway terminates TLS, so the credential is the only per-Agent
  proof that survives the hop.

  **There is no revocation.** Short validity and renewal are what bound a certificate; ejecting a
  host faster than its certificate expires means rotating the CA. And an expired certificate locks
  a host out even with a valid credential — a Client switched off longer than its validity needs
  `client_ca_file` unset for as long as it takes to re-enrol.

- **A released `.7z` unpacks as an executable on Linux and macOS.** The member is packed with a
  Unix mode of `0755` (7-Zip's Unix-attribute convention), so `7z x` yields a binary that runs
  instead of one that needs a `chmod +x` nobody wrote down. `--format tar.gz` already did this in
  its tar header; the two containers now agree. Nothing changes for a package the Server delivers —
  the Client sets the mode itself when it installs one — and nothing changes for the Windows
  artifact, where the bit means something else and 7-Zip does not write it either.

- **`opamp-fleet-client service install --interactive` writes the first configuration.** A freshly downloaded
  Client has no `client.toml` — the release artifact is the bare binary — and installing without one
  produced a service that started, dialled `127.0.0.1`, and managed nothing. The flag asks for what
  a fresh host cannot guess (endpoint, Agent name, credential, a private CA when the endpoint is
  `wss://`/`https://`, and last, defaulting to *no*, consent for the Server to update this Client's
  own binary), writes the file, and validates it before registering the service
  ([ADR-0027](docs/adr/0027-interactive-install-writes-the-first-configuration.md)).

  Nothing about existing invocations changes: the flag is opt-in, an existing file is kept rather
  than overwritten, and `--interactive` without a terminal on stdin fails instead of blocking a
  provisioning run. The credential is typed into a hidden prompt, so it stays out of the shell
  history and the process list, and on Unix the file is created mode `0600`. Installing *without*
  the flag now prints a warning when the configured path holds no file — the silence was the bug.

- **Released builds of the Client, one archive per platform.** A release publishes
  `opamp-fleet-client-<version>-<os>-<arch>.7z` for Linux and macOS on `x86_64` and `aarch64`, and Windows
  on `x86_64`, together with `SHA256SUMS`
  ([ADR-0025](docs/adr/0025-release-pipeline-and-artifacts.md)). Until now there was nothing to
  install but a build of your own.

  **The version is `[workspace.package] version` in `Cargo.toml`**, and the pipeline creates the
  `version/*` tag from it ([ADR-0026](docs/adr/0026-version-from-cargo-toml.md)) — so a release is
  "merge the bump, run the workflow", and no tag is typed by hand. It refuses rather than guesses:
  a version that already has a tag or a release is spent, and the run says so before it builds
  anything — including a dry run, which is the run meant to catch a forgotten bump — and a binary
  that does not report the version its artifacts are named after fails the run.

  **Each archive is also a package artifact**: it holds the Client under the name the install layout
  gives it, so the file is uploaded exactly as downloaded and the published SHA-256 is the one an
  Agent verifies. When you hand one to a Server for a Client self-update, `?version=` takes the
  release number — the one in the file name.

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

### Changed

- **The connection-settings hash now covers the whole offer**, not just its OpAMP part — it has to,
  now that one offer can also carry telemetry destinations
  ([ADR-0036](docs/adr/0036-agents-report-their-own-telemetry.md)).

  One consequence on upgrade: every Agent's stored hash stops matching, so the Server sends its
  standing offer once more and each Agent verifies and re-applies it. That is one extra exchange per
  Agent, no reconnect and no downtime, and it settles by itself.

- **A connection-settings offer carrying `tls` or `proxy` is no longer acknowledged `APPLIED`.**
  The Client never implemented those two fields and dropped them silently while reporting success,
  so a Server offering either was told the settings were in force when they were not. It now applies
  everything it does honour — endpoint, credential, heartbeat, certificate — and reports `FAILED`
  with an `error_message` naming what it dropped
  ([ADR-0035](docs/adr/0035-mutual-tls-and-the-server-issued-client-certificate.md)).

  Nothing to do on any host. A Server whose `[connection_offer]` never carried those fields — this
  project's Server cannot — sees no change at all.

- **The Windows services list now shows a description**: **OpAMP Fleet Client for Windows**. It is a
  field of its own beside the display name, and nothing that registers a service fills it — so the
  Client had a display name and an empty Description column. It is the same text on every instance;
  the display name beside it (`OpAMP Fleet Client (prod)`) is what distinguishes them.

  An already-installed service keeps its empty description until it is registered again — the field
  is written at install time. `service uninstall` then `service install` fills it.

- **A Windows install without Administrator now says so before it writes anything.** `service
  install` asks the service control manager up front whether this process may register a service at
  all, and stops with the one thing that fixes it — open a shell with "Run as administrator" — if it
  may not.

  Before, the refusal came from `sc create` in the middle of the install, as a bare (and localised)
  `OpenSCManager` access-denied error, and only *after* the layout had been written: `%ProgramData%`
  lets an ordinary user create folders, so a staged version directory and a `current` junction were
  left behind by an install that had registered nothing. Delete such a root, or just re-run the
  install from an elevated shell.

  There is no UAC prompt, on any `service` verb: a running process cannot raise its own rights, so
  an elevated shell is the way in. The earlier, unreleased retry of a refused `sc.exe` call through
  `Start-Process -Verb RunAs` is gone — it sat *after* the registration, which is what gets refused
  first, so it could never fire.

- **Every Agent now reports the attributes the protocol names.** Alongside `os.type` and
  `host.arch`, an Agent reports `os.name`, `os.version`, `host.name`, and `host.id` — so a Selector
  can target a distribution release, or pin one machine by its host name.

  **`host.name` in particular was promised and missing.**
  [ADR-0017](docs/adr/0017-selector-targeted-packages.md) offers "a Selector matching that host's
  `host.name`" as *the* way to hold one host to one artifact; no Agent reported it, so such a
  Selector silently matched nothing. It works now.

  An attribute the host cannot answer is **left out rather than reported empty** — a container
  without `/etc/machine-id` reports no `host.id` — so a Selector on one reaches exactly the hosts
  that have it. Nothing has to be changed on any host; the new attributes appear on the next
  connection.

- **`service_namespace` in `client.toml`**, for the one attribute the protocol makes conditional on
  the environment ("if it is used in the environment where the Agent runs"). It is reported as an
  *identifying* attribute of every Agent this Client presents, which is where the protocol puts it —
  unlike `[attributes]`, which tags an Agent. Optional; absent reports nothing.

- **`opamp-fleet-client service install` without `--config` now bakes `<root>/client.toml` into the unit**,
  inside the install root, instead of `client.toml` resolved against whatever the working directory
  happened to be
  ([ADR-0027](docs/adr/0027-interactive-install-writes-the-first-configuration.md)). A service
  manager's working directory is `/` or `System32`, so the old default pointed at a file the
  service could not have been relying on unless the install was run from exactly the right
  directory. **If you installed by running `install` from the directory holding your
  `client.toml`,** name it explicitly —
  `opamp-fleet-client service install --config /etc/opamp/client.toml` —
  or move the file to `<root>/client.toml`; the install prints the path it registered either way.

- **What a Client reports as its version now names the release it is heading *for*, not the one it
  descends *from*.** The base comes from `Cargo.toml` and git decides only the rest
  ([ADR-0026](docs/adr/0026-version-from-cargo-toml.md)): a build with no release tag on its commit
  reports `0.1.0-dev+<hash>` where it used to report `0.0.0-dev+<hash>`. Nothing to do — but the
  fleet view, `opamp-fleet-client --version`, and the name of the versioned install directory all
  shift with it,
  so a host that has not changed will still look different after an upgrade. A commit carrying a
  `version/*` tag that names a *different* version than `Cargo.toml` no longer builds at all, rather
  than producing a binary that disagrees with its own tag.

- A Supervisor's package downloads are staged in its own directory
  (`<supervisor_dir>/<name>/packages/`) instead of `<state_dir>/packages/`, which the Client's own
  Agent keeps using. Any `*.staged` file left in the old location by an interrupted download is
  orphaned and can be deleted.
- A package artifact that is a bare program is now **moved** into place rather than copied, saving a
  second full write of it. An artifact that is an archive is still unpacked, so an upstream
  Collector release (`.tar.gz`) is unaffected.

### Fixed

- **An Agent that installed a package went on reporting the version it replaced.** The package
  itself was reported correctly — `Installed`, with the new version, in the fleet view's package
  pill — but the Agent's own `service.version`, which is the fleet table's **Version** column, still
  named the old one. On a first install onto an empty `program/` it named nothing at all, and the
  column stayed empty beside a package the Server had just seen installed.

  Only the program knows its own version, so the Client asks it: it runs the Managed Process's
  version flag (a Collector's `--version`, or a `command` Supervisor's configured `version_args`)
  and reports what it prints. That question was asked once, when the Supervisor started — never
  again after a swap replaced the binary it had asked. A Collector carrying the `opampextension`
  corrected itself the moment it next started and self-reported; one without the extension, and one
  with no configuration to run on yet, had nothing that ever would. Restarting the Client was the
  only cure.

  The program is now asked again after every successful swap, and the two sources — the probe and
  the extension's self-report — are merged per attribute instead of each replacing the other.
  Nothing to change on a host: an affected Agent reports its version within seconds of the next
  install, and a Client restart still fixes an Agent that installed before this version.

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

### Changed — breaking

- **A package now states the Agent type it is built for, and reaches no Agent of another**
  ([ADR-0034](docs/adr/0034-a-package-states-the-agent-type-it-is-built-for.md)). ADR-0031 made the
  Server refuse to send an artifact to a machine it cannot run on; this does the same for an Agent it
  was not built for. A Promtail artifact can no longer be swapped over a Collector because someone
  forgot the Selector.

  **Every existing package is inert until its type is set**, including one in the middle of a
  rollout. The Server starts normally and logs each untyped package by name; the package view marks
  it. Set the type to the `service.name` its Agents report:

  ```console
  $ curl -X PUT -H 'Content-Type: application/json' \
         -d '{"service_name": "otelcol-contrib"}' \
         http://<server>:4320/api/v1/packages/otelcol/type
  ```

  The value is compared **raw** — there is no canonical set of Agent types to normalise against — so
  a typo is a rollout that never starts rather than an error. Read the type off the Agent's fleet row
  before typing it. It belongs to the package name, not to an artifact, so it is set once for all
  platforms.

  An Agent that reports no `service.name` at all is now offered no package, the same rule ADR-0031
  applies to a missing platform. Every Client this project ships reports one.

  **New route** `PUT /api/v1/packages/{name}/type`; `PackageView` gains `service_name`.

- **`service.name` now reports the Agent *type*, not the Agent's name**
  ([ADR-0033](docs/adr/0033-an-agents-type-and-its-instance-name-are-two-attributes.md)). The name an
  operator gives an Agent moved to a new attribute, `service.instance.name`. Before, a
  `[[supervisor]]` block's `name` was reported as `service.name` — the slot the protocol reserves
  for "a reverse FQDN that uniquely identifies the Agent type" — and a Collector carrying the
  `opampextension` overwrote it with its own type the moment it connected, so every Collector of one
  distribution collapsed onto one name in the fleet view.

  **Any Selector matching `service.name` must be checked.** It now matches a type
  (`otelcol-contrib`, `opamp-fleet-client`), not a Supervisor's name, so one written against a name
  silently stops matching and its Configuration or package quietly stops being delivered. Nothing
  detects this for you. Point it at the new attribute instead:

  ```console
  $ curl -X PUT -H 'Content-Type: application/json' \
         -d '{"selector": {"service.instance.name": "otelcol-edge-01"}}' \
         http://<server>:4320/api/v1/configurations/edge-config/selector
  ```

  Aiming at *what an Agent is* is what `service.name` is now good for — one Selector of
  `{"service.name": "otelcol-contrib"}` reaches every Collector of that distribution, with nothing to
  configure per host.

  The interactive install asks for the same value it always did, under the name it actually
  reports now: `This Agent's name (service.instance.name)`.

  **`client.toml` needs no change**, and no Agent changes its identity: `instance_uid` and
  `service.instance.id` are untouched, so nothing is re-registered. A Managed Process that reports no
  type of its own now presents its program's file name as one; the new optional `service_name` key in
  a `[[supervisor]]` block states a better one.

  **The REST API's `AgentView` gained `service_instance_name`**, and `service_name` keeps its key
  while changing what it holds. Anything reading `service_name` as a display name should read
  `service_instance_name` and fall back to `service_name`, which is what the bundled UI now does.

- **A package now holds one artifact per platform, and `os`/`arch` are required**
  ([ADR-0031](docs/adr/0031-per-platform-package-variants.md)). An Agent is offered only the artifact
  built for the operating system and architecture it reported, and never another — so uploading a
  Windows build no longer installs it over every Linux host in the fleet.

  **Every Server has to be migrated before it will start.** A package stored without a platform is
  refused at startup, naming the file. For each one, upload it again with its platform, or delete it:

  ```console
  $ curl -X PUT --data-binary @otelcol-linux-amd64.tar.gz \
         "http://<server>:4320/api/v1/packages/otelcol?version=0.109.0&os=linux&arch=amd64"
  ```

  **Four routes change.** `PUT /api/v1/packages/{name}` and `PUT …/{name}/source` require `os` and
  `arch`; `POST …/{name}/rollback` and `GET …/{name}/file` require them in the query. `DELETE
  …/{name}` still deletes the whole package, and now takes an optional `?os=…&arch=…` for one
  artifact. Generated clients must be regenerated.

  **`GET /api/v1/packages` answers a new shape.** `version`, `addon`, `source_url` and
  `previous_version` moved out of the package and into a `variants` array, one entry per platform,
  each with its own `os`, `arch` and rollback history. `selector` stays on the package: the Selector
  aims, the platform fits.

  A rollback names one platform and moves only that one — a canary taken back on Linux must not push
  macOS off a version it never left.

- **The Client reports `host.arch` as `amd64`/`arm64`**, the semantic-convention values the protocol
  points at, where it used to report Rust's `x86_64`/`aarch64`
  ([ADR-0031](docs/adr/0031-per-platform-package-variants.md)).

  **A Selector written against `host.arch` must be edited on the Server** — `{"host.arch": "x86_64"}`
  now matches nothing. Change it to `amd64` (or `aarch64` → `arm64`); nothing changes on any host.

  This also closes a quiet defect: a Managed Process's attributes are folded over the Supervisor's,
  and the Collector's `opampextension` already reported `amd64`. The same machine therefore changed
  architecture depending on whether a Collector happened to run on it, and Selectors written against
  either spelling broke without anything having changed.

- **Release artifacts are named `<name>_<version>_<os>_<arch>.7z`** —
  `opamp-fleet-client_1.2.3_linux_amd64.7z`, `opamp-fleet-client_1.2.3_darwin_arm64.7z`. Both halves
  of that changed: the platform tokens, which used to say `macos` and `x86_64`
  ([ADR-0031](docs/adr/0031-per-platform-package-variants.md)), and the separator between the four
  fields, which used to be `-`
  ([ADR-0032](docs/adr/0032-release-artifacts-separate-their-fields-with-underscores.md)) — both
  superseding the naming in [ADR-0025](docs/adr/0025-release-pipeline-and-artifacts.md).

  **Anything scripted against the old names breaks**, including a glob like `*-linux-amd64.7z`;
  releases already published keep the names they have, and nothing is renamed.

  The gain is that the name says what it holds without being guessed at. The two platform fields are
  exactly the pair an Agent reports, so uploading a release needs no translation table — and because
  neither a package name (`[a-z0-9-]`) nor a version (`1.2.3-dev`) can contain `_` while both contain
  `-`, the four fields can be read off the file name by splitting it. That is what lets the release
  notes publish an upload loop that takes `os` and `arch` out of each file rather than being handed
  the name and version it has to strip first.

- **The service is registered as `opamp-fleet-client` on every platform**
  ([ADR-0030](docs/adr/0030-one-service-name-on-every-platform.md)). It used to be
  `opamp-fleet-client.default.service` on systemd and `io.opamp-fleet.client.default` on launchd and
  the Windows SCM — two names nobody chose, both falling out of how a reverse-DNS label happens to
  be split. Now: `systemctl status opamp-fleet-client`, `launchctl list opamp-fleet-client`,
  `sc query opamp-fleet-client`. A named instance appends its own name
  (`opamp-fleet-client-prod`); the default instance carries the bare one.

  **An already-installed service is not found under the new name.** Run
  `service uninstall` with the *old* binary, then `service install` with the new one. The install
  layout and the state directory are untouched by either.

  On Windows the services list now shows **OpAMP Fleet Client** — the readable name ADR-0010
  promised and never actually set.

- **A version is compared and shown without its build metadata**
  ([ADR-0029](docs/adr/0029-a-version-is-compared-and-shown-without-its-build-metadata.md)). Two
  things change, and one of them is an API break.

  **Uploading a Client package now takes the release number.** `?version=1.2.3` matches a binary
  reporting `1.2.3+a1b2c3d`, because the commit a build came from is provenance, not identity — and
  it is the one part of the string nobody can type at upload time. (A `+` in a URL query decodes to
  a space, so the old requirement to pass the full string could only be met as `%2B`. That trap is
  gone.) The pre-release is **not** ignored: a `1.2.3-dev` build offered as `1.2.3` is still
  refused. The full string keeps working where you already pass it.

  **`AgentView.service_version` now holds the release, not the build.** It is
  `MAJOR.MINOR.PATCH`, with the pre-release when there is one; what the Agent reported verbatim
  moved to the new **`service_build`** field beside it. **If you read `service_version` from
  `/api/v1/fleet/agents` and need the commit, read `service_build`.** The bundled UI shows the
  release in its Version column, the build on hover, and searches both. An Agent whose reported
  version is not a version at all — a Foreign Agent numbering itself its own way — is shown
  unchanged in both fields.

- **The Client ships as `opamp-fleet-client`.** The release artifact is
  `opamp-fleet-client-<version>-<os>-<arch>.7z`, the file it installs is `opamp-fleet-client`
  (`.exe` on Windows), and the version directory beside it is
  `versions/opamp-fleet-client-<version>-<commit>/`
  ([ADR-0028](docs/adr/0028-the-client-is-named-opamp-fleet-client.md)). One name from the download
  to the process in `ps` to the Agent in the fleet view.

  **Nothing in a fleet has to be migrated, because nothing has been released yet** — this is the one
  moment the change is free. A *development* service installed under the old layout does have to be
  re-registered: its unit points at `<root>/current/client`, which the new build no longer produces.
  Run `opamp-fleet-client service uninstall` with the old binary, then
  `opamp-fleet-client service install` with the new one.

  The Cargo package stays `client`, so `cargo run -p client` and `cargo build -p client` are
  unchanged — only the binary they produce is renamed. The service label
  (`io.opamp-fleet.client.<instance>`) is unchanged too.

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
