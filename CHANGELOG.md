
### Changed

- **`working_dir` and `reload_signal` are gone from every block.** A Managed Process now starts in
  the directory its program lives in — this Supervisor's `program/`, or the tree root — instead of
  inheriting whatever directory the service manager left the Client in, usually `/`. And whether a
  program re-reads its configuration on a signal is the program's own convention, which a kind
  holds: it is the one setting whose wrong value stays invisible, because a signal the process
  ignores looks exactly like an apply that worked. **What to do:** delete both lines. An agent
  under `command` now applies a configuration by restarting; if its in-place reload matters, it
  wants a kind of its own.

- **Timing is the fleet's policy, then the agent's correction of it.** `stop_timeout_secs` and
  `apply_grace_secs` join `retain_previous_secs` as fleet-wide settings, in a new `[supervisors]`
  section; a wrapped kind overrides them where its agent demands it (Icinga 2 needs sixty seconds
  to shut down), and only a block of an unwrapped kind may state its own. **What to do:** move a
  value you set on several blocks into `[supervisors]`; delete it from a wrapped block, where the
  kind now holds it.

  ```toml
  [supervisors]
  stop_timeout_secs = 10
  apply_grace_secs = 3
  ```

- **`[supervisor.attributes]` is gone.** Tagging one Agent among several on a host is a Server
  **label**: keyed by that Agent's `instance_uid`, matched by the same Selectors, set with one API
  call and effective at once. **What to do:** delete the table and
  `PUT /api/v1/agents/<uid>/labels` instead. The Client-wide `[attributes]` are unchanged — they
  describe the host, and every Agent on it still carries them.

### Added

- **A Client reports the Supervisor kinds it carries**, one non-identifying attribute per kind
  (`supervisor.kind.telegraf = "true"`), so a Supervisor set can be aimed with a Selector at the
  Clients that can actually run it — rather than a Server learning from a `FAILED` that it aimed at
  a Client too old to have the plugin.

- **The Client makes the directories a delivered agent writes into.** An agent the fleet installs
  arrives on a host nobody prepared, and several create nothing themselves — Icinga 2 exits when
  `DataDir` is absent, the GLPI Agent exits when `--vardir` is. A kind now names those directories
  and the Client makes them **before every spawn**, owner-only, so an installation cannot end in a
  crash loop over a missing directory and one removed under a running fleet comes back on the next
  restart. **What to do:** nothing — and on a GLPI host you no longer create `agent-state` by hand
  before the first start. An `agent-state` an earlier release had you create as another user still
  needs to belong to the Client's service account.

### Fixed

- **A Client that updates itself into a configuration it cannot read now rolls back.** The
  self-update's probation ([ADR-0020](docs/adr/0020-the-client-updates-itself-from-a-signed-package.md)) counts a failed start
  and returns the host to the previous version after three attempts — but it never saw this one: a
  run resolved its configuration *before* it resolved the update in flight, so a new version that
  refuses the file on this host exited before the attempt was counted. The service manager then
  restarted that version for ever, and the Server heard nothing, because the Client never reached
  it. The resolution now also runs on the failing path, finding the marker through `--state-dir`
  (which an installed service always passes) or the file's own `state_dir`. **What to do:** nothing.
  A host in that state today recovers as soon as it runs a version carrying this fix — and until
  then, the way out is to correct the file or to point `current` back by hand.

- **The Client says what it is at startup, and what its TLS will use.** Two lines before any work:
  the running version, the configuration file, the state directory, the endpoint and the number of
  Supervisors — the version appeared in no log line until now, so a file a self-update left behind
  ([ADR-0020](docs/adr/0020-the-client-updates-itself-from-a-signed-package.md),
  [ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)) could not be attributed to
  the version that wrote it — and then the trust and the client certificate actually in force,
  which is a Server-issued one no `supervisor.toml` mentions when there is one
  ([ADR-0026](docs/adr/0026-admission-by-a-client-certificate-alone.md)). A
  configuration file that is not there is now said out loud as well, because a mistyped `--config`
  otherwise starts cleanly on defaults and supervises nothing. **What to do:** nothing; anything
  parsing the log gains lines, and loses none.

- **More is visible at `debug` when an install or a Managed Process misbehaves.** A tree member left
  unpacked because it sits outside the program's own directory
  ([ADR-0018](docs/adr/0018-signed-package-delivery-from-allowed-sources.md)) is now named, not only counted; the Collector
  Supervisor states which config-map entries it hands the Collector on every (re)start; and the
  `command` Supervisor states the fully expanded invocation of its Foreign Agent
  ([ADR-0032](docs/adr/0032-a-host-can-keep-its-supervisor-set-from-the-server.md)) — its
  environment by variable name only, never by value. **What to do:** nothing, unless you are
  chasing one of those, in which case `RUST_LOG=debug` now answers it.

- **Both of the Server's listeners now hang up on a connection that never finishes its request**
  ([ADR-0023](docs/adr/0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)). A peer gets 30 seconds for
  its request line and headers and 10 seconds for the TLS handshake; until now it got forever,
  because hyper's own 30-second default is silently discarded while no timer is installed and
  neither axum nor axum-server installs one. The bound is on connection *setup* only: an established
  WebSocket session, a package download, and a package upload are all unaffected, whatever they take.
  Shutdown also drains both planes within ten seconds instead of dropping them (TLS) or waiting on
  every open Agent connection (plain). **What to do:** nothing — no configuration key changed. Only
  a client that needs more than 30 seconds to send its *headers* would notice, and none exists here.
- **The install path lost two levels, and `--instance` is gone.** The Client installed under
  `<base>/opamp-fleet/client/<instance>` — a product level, a component level asserting `client`
  where the program is called `supervisor`, and an instance level holding the constant `default` on
  every host anyone ever installed. It now installs under `<base>/<product>` alone:
  `/opt/opamp-fleet` and `/var/lib/opamp-fleet` on Linux, `%ProgramData%\opamp-fleet` on Windows,
  `/Library/Application Support/opamp-fleet` on macOS
  ([ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)). The product's name is
  fixed at build time (`OPAMP_FLEET_PRODUCT_NAME`, default `opamp-fleet`), and a second
  installation on one host is a **second build** rather than a runtime flag — the flag was
  reachable from no delivery path we ship, was read by nothing at runtime, and could not be
  recovered by anything once typed.
  **What to do:** nothing, on any host installed from a published release — there are none. The
  service is now called `opamp-fleet` rather than `supervisor`, so `systemctl start supervisor`
  becomes `systemctl start opamp-fleet`; `service uninstall|start|stop|status` take no name at all;
  and the `PATH` command is `opamp-fleet`. `--instance` is refused outright rather than ignored.

- **`service install` takes `--data-root`.** `--root` still names one directory and, given alone,
  still collapses the layout and the data into it. `--data-root` names the second half, which is
  what the Linux system-scope split needs — the executable layout under `/opt` because SELinux
  never lets systemd execute a binary labelled `var_lib_t`, the configuration and state under
  `/var/lib`.

- **The MSI puts the program in `Program Files` and everything else under `%ProgramData%`.**
  `INSTALLFOLDER` is now `C:\Program Files\opamp-fleet` and holds the delivered `supervisor.exe`
  and nothing more; the versioned layout, `supervisor.toml` and `state/` go to
  `%ProgramData%\opamp-fleet`. This is the Windows form of the line the `.deb` and `.rpm` already
  drew between `/usr/libexec` and `/opt`: the self-update rewrites the layout at runtime, and
  `Program Files` is not a tree a service account should be able to write — which it would have had
  to, since `--run-as` hands the layout to the account the service runs as. A host installed by the
  MSI and one unpacked from the archive now put the same things in the same places.

- **The `.deb`, `.rpm` and MSI are named after the product.** The package identity is `opamp-fleet`
  and the payload lands in `/usr/libexec/opamp-fleet/supervisor`, so two variant builds can be
  installed side by side without claiming one another's files.

### Removed

- **A `[[supervisor]]` block can no longer name a program on the machine.** `binary` and `command`
  take a bare file name and nothing else; an absolute path — and the Windows drive-relative form
  with it — is refused at startup with a message naming the way across
  ([ADR-0032](docs/adr/0032-a-host-can-keep-its-supervisor-set-from-the-server.md)). A Managed Process is
  always one this Client installed, so every Supervisor now declares `AcceptsPackages` and the
  capability is a constant of the Client rather than something derived from a path.
  **What to do:** an agent the machine carries — a distribution-packaged GLPI Agent, a
  machine-installed Icinga 2 — is brought under management by repacking it and uploading it as a
  Set, which is the route the GLPI Agent and Icinga 2 pages already document. A block naming an
  absolute path will stop the Client at startup rather than starting without it.

### Changed

- **The REST API and the UI moved to their own port, on loopback**
  ([ADR-0023](docs/adr/0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md),
  superseding the single-listener decision of
  [ADR-0025](docs/adr/0025-five-crates-a-publishable-communication-layer-and-toml-configuration-axum-without-its-websocket.md)). The Server now serves two planes,
  split by audience. The **Agent plane** keeps `listen` (`0.0.0.0:4320`): the OpAMP endpoint and the
  package download an offer's `download_url` points at. The **Operator plane** is new — `[rest]
  listen`, `127.0.0.1:4321` by default — and carries the REST API, the API docs, and the bundled UI.
  Nothing authenticates that plane yet ([`[auth]`](config/server.toml) guards the OpAMP endpoint and
  nothing else), so its reachability is its only protection, and it carries the authority to
  reconfigure and re-package the whole fleet: hence loopback. Authenticating it is now a decision
  about one listener instead of a per-path exemption on a shared one — which is the point of the
  move.
  **What to do:** change the address in every operator tool, script, and bookmark from
  `:4320/api/v1/…` to `:4321/api/v1/…`, and open the UI at `http://<server>:4321/`. To reach it from
  another host, either tunnel (`ssh -L 4321:127.0.0.1:4321 <server-host>`) or put
  `[rest]` / `listen = "0.0.0.0:4321"` in `server.toml` deliberately. **Clients need no change at
  all** — the endpoint, the offered `download_url`, and `advertised_url` all keep working as they
  are. The two addresses must differ; equal ones are refused at startup by name.

### Added

  [GLPI Agent recipe](docs/manual/glpi-agent.md).
- **A `command` Supervisor can reload instead of restart**
  ([ADR-0010](docs/adr/0010-supervisor-mode-and-its-kinds.md)). A `[[supervisor]]` block of
  `type = "command"` may set `reload_signal = "HUP"` (`"USR1"` and `"USR2"` are also accepted,
  with or without a `SIG` prefix): a configuration change is then applied by sending that signal,
  and the process keeps running with its in-flight state. If the signal cannot be delivered or
  the process dies on it, the Supervisor falls back to the restart, so the apply still lands.
  Linux/macOS only — on Windows a set key is refused at startup.
  **What to do:** nothing; the key is opt-in. Set it only for a program that genuinely re-reads
  its configuration on the signal.

### Changed

- **The OpAMP Protocol Baseline moved to `v0.20.0`** ([PR #385](https://github.com/open-telemetry/opamp-spec/pull/385)).
  Upstream renamed the `AgentConfigFile` message to `AgentConfigObject` and clarified that an empty
  configuration-map key is always allowed. This is a **wire-compatible** change — the field numbers
  and the `config_map` shape are unchanged, so a Server and Client on either version interoperate,
  and this project already keyed the map by the Configuration name and never rejected an empty one.
  The vendored schema now lives at `crates/opamp/proto/v0.20.0/`, and the generated Rust type is
  `opamp::proto::AgentConfigObject`. See [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md).
  **What to do:** nothing — no operator action, and nothing changes on the wire.

### Changed

- **The Linux service executes from `/opt`.** A default system install's executable layout —
  `versions/` and the `current` pointer — now lives at `/opt/opamp-fleet/client/<instance>`
  instead of under `/var/lib`, where SELinux-enforcing hosts (Fedora, RHEL, SUSE 16) never let
  systemd start it (`status=203/EXEC`); `client.toml` and `state/` stay at
  `/var/lib/opamp-fleet/client/<instance>`, and `--root` still puts everything under the one
  directory it names ([ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)).
  **What to do:** nothing on a packaged (`.deb`/`.rpm`) host — the upgrade re-registers the unit
  against `/opt`, restarts the service if it was running, moves no data, and cleans the orphaned
  binaries out of `/var/lib`. A *manual* Linux system install (`.7z`, no `--root`) should re-run
  `opamp-fleet-client service install` once after the update, then delete the leftover
  `versions/` and `current` under `/var/lib/opamp-fleet/client/<instance>`.

- **The Client's OpAMP endpoint no longer follows HTTP redirects; artifact downloads follow a bounded
  chain.** The OpAMP endpoint is a fixed, operator-configured address, so its HTTP transport and the
  connection-settings probe now refuse redirects — a redirect there could only bounce an
  authenticated session elsewhere. Artifact downloads still follow redirects (a mirror is often a CDN
  that bounces to signed storage, ADR-0018) but are now bounded to a short chain; integrity still
  rests on the content hash and signature, never on where the bytes came from. No operator action
  required unless an OpAMP endpoint was, unusually, served behind an HTTP redirect.

- **The Agent's state and configuration directories are kept owner-only.** The persisted state
  directory and the `config/` directory the Managed Process reads from were created at the umask
  default, and a config-map entry read by path (a `${file:...}` reference, ADR-0011) can be a
  certificate or a key — so on a multi-user host that material was world-readable. The directories
  are now `0700` and the stored configuration protobuf and each entry file `0600`; the Managed
  Process runs as the same user and still reads its own config. No operator action required.

- **A Gateway now says why it hung up on an oversized message.** The Baseline answers a message past
  the size limit with a WebSocket close of `1009 Message Too Big`, and
  [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md) claims it as implemented. The Server's endpoint did
  it; the Gateway's dropped the connection with no status at all, so a downstream Client saw a reset
  socket and could not tell an oversized report from a Gateway that had died — and retried into the
  same wall. An oversized frame is also a close now rather than a message quietly dropped while the
  socket read on.

- **A Gateway now accepts a gzipped report.** Accepting `Content-Encoding: gzip` is a Baseline MUST
  for anything serving OpAMP, and a Client in Gateway Mode
  ([ADR-0034](docs/adr/0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)) *is* an OpAMP server to the Agents behind it. The
  Server's endpoint implemented the rule; the Gateway's did not, and handed the compressed bytes
  straight to the protobuf decoder — so an Agent that compressed its reports worked against the
  Server and was answered `400 unreadable report` the moment a Gateway was put in front of it.

  The size limit applies **after** decompression, which is the other half of that MUST: a few
  kilobytes of gzip must not buy the hop gigabytes of memory. A body that decompresses past
  `max_message_size_bytes` is refused with `413`, and decompression stops at the limit rather than
  running to completion first.

  Both endpoints now read one implementation of the rule
  ([ADR-0025](docs/adr/0025-five-crates-a-publishable-communication-layer-and-toml-configuration-axum-without-its-websocket.md)). Nothing to change on any host — an
  affected Agent works through a Gateway as soon as it runs this version.

- **`server --version` now names the build it is, not the release it is heading for.** It printed the
  bare number from `Cargo.toml` — `server 0.1.3` — where the Client on the same commit reported
  `0.1.3-dev+ade2775`. So a Server binary could not be told apart from the release it was on its way
  to, and named no commit; two binaries of one workspace disagreed about their own version.

  Both ends now read the same helper, `opamp::version::current()`, which
  [ADR-0017](docs/adr/0017-versions-resolved-in-the-internal-crate.md) always required and the Server had
  opted out of ([ADR-0017](docs/adr/0017-versions-resolved-in-the-internal-crate.md)). A
  development build reports `0.2.0-dev+<commit>` and a released one `0.2.0+<commit>`.

  **Anything matching the Server's `--version` output exactly has to be relaxed** — a check for
  `server 0.2.0` no longer matches a development build. Nothing else changes: the Client's string is
  what it always was, and no Agent, configuration or stored file is touched.

- **A Client running as a service now writes its own log to disk**
  ([ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)), at
  `<state_dir>/logs/`, one file per day with seven days kept.

  **On Windows this closes a hole**: the SCM discards a service's stderr, so a Client installed
  there had no readable log at all — a service that would not start left nothing behind to explain
  why. The file is written on Linux and macOS too, where it duplicates `journalctl` and
  Console/`log show`, so that the answer to "where are the logs" is the same on every platform and
  in a container, where neither exists.

  Running the Client in the foreground writes no file; stderr is already in front of you.

  It is not a replacement for `ReportsOwnLogs` (ADR-0022): that ships to a destination the Server
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

- **Gateway Mode** ([ADR-0034](docs/adr/0034-client-modes-and-a-gateway-that-passes-packages-only-to-the-hosts-they-were-offered-to.md)): a Client can now stand at a network
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
  ([ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)).
  Nothing about existing invocations changes: the flag is opt-in, an existing file is kept rather
  than overwritten, and `--interactive` without a terminal on stdin fails instead of blocking a
  provisioning run. The credential is typed into a hidden prompt, so it stays out of the shell
  history and the process list, and on Unix the file is created mode `0600`. Installing *without*
  the flag now prints a warning when the configured path holds no file — the silence was the bug.
- **Released builds of the Client, one archive per platform.** A release publishes
  `opamp-fleet-client-<version>-<os>-<arch>.7z` for Linux and macOS on `x86_64` and `aarch64`, and Windows
  **The version is `[workspace.package] version` in `Cargo.toml`**, and the pipeline creates the
  `version/*` tag from it ([ADR-0017](docs/adr/0017-versions-resolved-in-the-internal-crate.md)) — so a release is
  "merge the bump, run the workflow", and no tag is typed by hand. It refuses rather than guesses:
  a version that already has a tag or a release is spent, and the run says so before it builds
  anything — including a dry run, which is the run meant to catch a forgotten bump — and a binary
  that does not report the version its artifacts are named after fails the run.
  Agent verifies. When you hand one to a Server for a Client self-update, `?version=` takes the
  release number — the one in the file name.
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

- **`opamp-fleet-client service install` without `--config` now bakes `<root>/client.toml` into the unit**,
  inside the install root, instead of `client.toml` resolved against whatever the working directory
  happened to be
  ([ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)). A service
  manager's working directory is `/` or `System32`, so the old default pointed at a file the
  service could not have been relying on unless the install was run from exactly the right
  directory. **If you installed by running `install` from the directory holding your
  `client.toml`,** name it explicitly —
  `opamp-fleet-client service install --config /etc/opamp/client.toml` —
  or move the file to `<root>/client.toml`; the install prints the path it registered either way.
- **What a Client reports as its version now names the release it is heading *for*, not the one it
  descends *from*.** The base comes from `Cargo.toml` and git decides only the rest
  ([ADR-0017](docs/adr/0017-versions-resolved-in-the-internal-crate.md)): a build with no release tag on its commit
  reports `0.1.0-dev+<hash>` where it used to report `0.0.0-dev+<hash>`. Nothing to do — but the
  fleet view, `opamp-fleet-client --version`, and the name of the versioned install directory all
  shift with it,
  so a host that has not changed will still look different after an upgrade. A commit carrying a
  `version/*` tag that names a *different* version than `Cargo.toml` no longer builds at all, rather
  than producing a binary that disagrees with its own tag.
- **On macOS, a Client installed as a service could never update itself** — every offer was refused
  with "this Client does not run from a versioned install layout", and a torn `current` pointer was
  never repaired either. The service is registered against `<root>/current/client` (ADR-0028), and
  asking the operating system what is running answers with that path on macOS and with the version
  directory behind it on Linux; only the second shape says where in the layout the binary sits. The
  path is now resolved before the layout is looked for, so both platforms answer the same. Nothing
  to change on a host: an affected Client picks its updates up as soon as it runs this version.

- **The service is registered as `opamp-fleet-client` on every platform**
  ([ADR-0028](docs/adr/0028-the-client-as-an-installed-service-with-a-secure-first-configuration.md)). It used to be
  `opamp-fleet-client.default.service` on systemd and `io.opamp-fleet.client.default` on launchd and
  the Windows SCM — two names nobody chose, both falling out of how a reverse-DNS label happens to
  be split. Now: `systemctl status opamp-fleet-client`, `launchctl list opamp-fleet-client`,
  `sc query opamp-fleet-client`. A named instance appends its own name
  (`opamp-fleet-client-prod`); the default instance carries the bare one.

  **An already-installed service is not found under the new name.** Run
  `service uninstall` with the *old* binary, then `service install` with the new one. The install
  layout and the state directory are untouched by either.

  On Windows the services list now shows **OpAMP Fleet Client** — the readable name ADR-0028
  promised and never actually set.

- **A version is compared and shown without its build metadata**
  ([ADR-0017](docs/adr/0017-versions-resolved-in-the-internal-crate.md)). Two
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
  ([ADR-0029](docs/adr/0029-releases-installers-and-the-name-supervisor-secure-by-default.md)). One name from the download
  to the process in `ps` to the Agent in the fleet view.

  **Nothing in a fleet has to be migrated, because nothing has been released yet** — this is the one
  moment the change is free. A *development* service installed under the old layout does have to be
  re-registered: its unit points at `<root>/current/client`, which the new build no longer produces.
  Run `opamp-fleet-client service uninstall` with the old binary, then
  `opamp-fleet-client service install` with the new one.

  The Cargo package stays `client`, so `cargo run -p client` and `cargo build -p client` are
  unchanged — only the binary they produce is renamed. The service label
  (`io.opamp-fleet.client.<instance>`) is unchanged too.

