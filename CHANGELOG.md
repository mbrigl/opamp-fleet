
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

- **A package behind an authenticated mirror could not be downloaded.** The Server has always passed
  a referenced Set entry's headers to the Agent verbatim — the credential an operator configures for
  a private source ([ADR-0018](docs/adr/0018-signed-package-delivery-from-allowed-sources.md)) — and the Client
  dropped them, so the fetch came back `401` and the rollout failed with an opaque transport error.
  The headers now ride the `GET`, as the protocol asks. Because such a header is a credential given
  for *one* host, a download that carries any now follows its redirect chain itself and re-attaches
  them only while scheme, host and port are unchanged: HTTP clients strip only `Authorization`,
  `Cookie` and `Proxy-Authorization` across origins, so a custom token would otherwise have been
  handed to wherever a mirror redirected. Values are never logged, and a header that is not a valid
  HTTP header now fails the download naming its key.
  **What to do:** nothing, unless a referenced entry's download was failing — upgrade the Client and
  retry the rollout. Ordinary uploaded packages are unaffected; a CDN mirror that redirects to signed
  storage keeps working, since a download with no headers follows redirects exactly as before.

- **Own telemetry crashed its exporter thread instead of exporting.** The first export panicked with
  `there is no reactor running, must be called from the context of a Tokio 1.x runtime`, and the
  signal died with the thread. The SDK does not export on the async runtime: each batch processor
  and the metrics reader run on a **dedicated OS thread** and block on the export there, so the
  asynchronous HTTP client the exporters were given had no reactor to work with. That was true from
  the day own telemetry landed — it simply could not be reached until a destination was actually
  offered. The exporters now dispatch each request onto this process's runtime and await the
  result, so the socket work happens where the reactor is.
  **What to do:** nothing. If own telemetry appeared to do nothing before, it should now arrive —
  verified end to end against a local OTLP receiver: logs and metrics both, metrics on their 10 s
  interval, no panic.

- **A Server offering only telemetry destinations was ignored, and re-offered for ever.** With a
  `[telemetry_offer]` and no `[connection_offer]`, the Server sends a connection-settings message
  carrying no OpAMP settings — which is what the protocol asks it to do, and what its own test
  asserts. The Client required OpAMP settings to be present and dropped the message whole: no
  acknowledgement, so the Server's hash gate never closed and it re-sent the offer on every
  exchange, while own metrics, traces and logs never started. Such an offer is now applied in place
  and acknowledged, without a verification connection and without a reconnect — the protocol names
  three classes of destination with deliberately different sequences, and scopes its
  verify-by-connecting requirement to the OpAMP settings alone
  ([ADR-0027](docs/adr/0027-connection-settings-offered-without-a-credential-and-server-capabilities.md)). A telemetry
  endpoint change no longer disconnects the fleet either.
  **What to do:** `[telemetry_offer]` alone is now enough; a `[connection_offer]` added only to work
  around this can be removed. A Server that can offer anything at all — settings, telemetry, or a
  `[client_ca]` — now declares `OffersConnectionSettings`, where before it declared the bit only for
  `[connection_offer]` and exercised the capability undeclared for the other two.

- **The Client kept reporting what a Server said it could not accept.** Capability negotiation is a
  MUST in both directions, and only two of the seven Server bits changed any behaviour here: package
  status went to Servers without `AcceptsPackagesStatus`, connection-settings status to Servers
  without `OffersConnectionSettings`. Both are now gated by one stated rule — optimistic until the
  Server has declared anything, binding once it has
  ([ADR-0027](docs/adr/0027-connection-settings-offered-without-a-credential-and-server-capabilities.md)). A Server that
  has actually *sent* an offer still gets its acknowledgement whatever its bitmask says, because
  withholding it would leave that Server re-offering for ever. `remote_config_status` is
  deliberately never gated; the reasons are recorded at the code and in the conformance matrix so
  the non-gate is not mistaken for an omission.
  **What to do:** nothing against this project's Server, which declares what it exercises. This
  matters for third-party Servers (ADR-0009): one implementing only the two mandatory bits now sees
  a Client that respects that.

- **A refused own-telemetry destination was acknowledged as applied.** A destination the Client would
  not use — cleartext beyond loopback, or one carrying `tls`/`proxy` settings — was written to the
  log and the offer was still reported `APPLIED`, so the fleet showed telemetry flowing that was not.
  The refusal now reaches the Server as a `FAILED` status naming what was dropped, including one
  found at startup when persisted settings are put back in force. An offered client `certificate` is
  now honoured rather than ignored ([ADR-0022](docs/adr/0022-own-telemetry-over-tls-1-3-and-plaintext-only-to-the-loopback.md)):
  the exporter presents it, paired with the key this Client generated for its signing request. A
  `private_key` *in the offer* is refused by name — this Client's private key never leaves its host
  and is never accepted from the Server.
  **What to do:** check the fleet view after upgrading. A destination that was quietly refused will
  now show as `FAILED` with the reason; that is the gap becoming visible, not a new failure.

- **Own metrics were reported six times more slowly than the protocol recommends.** The Baseline's
  recommended reporting interval for own metrics is 10 seconds; this Client sampled every 30 s and
  left the OpenTelemetry SDK's periodic reader at its 60 s default, so a backend saw a value once a
  minute. Both are now 10 s, driven by one constant — exporting more often than sampling would only
  ship each value repeatedly.
  **What to do:** expect roughly six times the metric volume per Agent from a fleet that has an
  `own_metrics` destination offered. Traces and logs are unaffected; neither is on an interval.

- **Buffered spans and log records were lost on every stop.** The daemon returned without flushing
  its OTLP exporters, so the records explaining a shutdown — the ones that matter after a crash and
  restart — never left the host. Both exit paths now flush first.

- **Throttling and backoff follow the protocol.** A `503` or `429` on plain HTTP was treated as a
  generic error and the next poll went out on the ordinary interval; `Retry-After` is now waited out
  (30 s when the Server names no interval). A `413` no longer arms a full state report, which had
  made the *next* request larger than the one just refused. And the reconnect backoff now carries
  jitter, so a fleet coming back after a Server restart spreads over each interval instead of
  arriving on the same instants.
  **What to do:** nothing. A Server that never throttles sees no change.


### Added

- **Basic authentication for the REST API and the UI**
  ([ADR-0026](docs/adr/0026-admission-by-a-client-certificate-alone.md)). `[rest.auth.basic_users]`
  in `server.toml` — `user = "password"`, several allowed — guards the **whole** Operator plane:
  `/api/v1/…`, the OpenAPI document, `/api/v1/docs`, and the UI at `/`. A request without a matching
  credential is answered `401` with a `WWW-Authenticate: Basic` challenge, which is what makes a
  browser ask for the password, so the bundled UI needs no login page and no session. Absent, the
  plane stays open exactly as before — the loopback default of ADR-0023 is what protects it then.
  **What to do:** nothing, unless you publish that plane. If you do, add the section — and pair it
  with `[tls]` or a TLS-terminating proxy, since Basic sends the password on every request; the
  Server warns at startup if you have not. Existing tooling needs no new flag: the credential rides
  the URL (`curl -u user:pass …`, `--server http://user:pass@host:4321`). Two limits, stated
  plainly: everyone listed can do everything (authentication, not authorization), and passwords sit
  in `server.toml` verbatim, as `[auth]`'s already do. The Agent plane is untouched — Agents and
  package downloads carry no operator credential and never will.

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

### Fixed

- **`opamp-package-fetch` says why an upload was refused, instead of "cannot reach".** The Server
  decides some uploads before it reads a byte of the artifact — an identity nobody created, a Set
  already rolled out and therefore immutable
  ([ADR-0014](docs/adr/0014-rollout-and-what-reaches-an-agent.md)), a package store at its ceiling
  ([ADR-0018](docs/adr/0018-signed-package-delivery-from-allowed-sources.md)). With hundreds of megabytes
  already in flight that answer races the upload, the connection resets, and what the tool could
  report was a transport error naming the one thing that was *not* the problem: the Server had
  answered, and said why. It now asks a second time with an empty artifact — refused in its own
  right, so the probe can store nothing — and reports the Server's status and message. Transport
  errors that are genuine read better too: the cause beneath `reqwest`'s own layer is printed
  rather than swallowed, on downloads as well as uploads.
  **What to do:** nothing. An upload that has been failing with `cannot reach …` will name its
  reason on the next run.

## [0.3.0] - 2026-08-15

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

- **A Managed Process's package updates no longer loop, and keep a fallback**
  ([ADR-0018](docs/adr/0018-signed-package-delivery-from-allowed-sources.md)). Three changes to
  how a Supervisor applies a package (ADR-0018):
  - A **first** install that will not start is no longer discarded — the verified program is kept in
    place and reported `InstallFailed`. Previously it was removed, which emptied `program/` and set
    the Server re-offering the same artifact in a download-crash-rollback loop.
  - A program that **keeps failing to start** is held after a few attempts instead of being
    restarted forever (the give-up the Client's own self-update already uses). The Agent reports
    `not restarting: the program keeps failing to start`.
  - A **successful** update keeps the version it superseded for a window before deleting it, so an
    operator has a fallback. New `[updates] retain_previous_secs` (default one day), overridable per
    `[[supervisor]]` block with `retain_previous_secs`; `0` restores the old delete-on-success.

  **What to do:** nothing to keep working. If a rollout of a package that crashes on start had been
  looping, it now stops on its own; and a superseded version now occupies disk for up to a day per
  Supervisor — lower `retain_previous_secs`, globally or per block, on a host that cannot spare it.


### Changed

- **Saving a Configuration no longer distributes it** ([ADR-0014](docs/adr/0014-rollout-and-what-reaches-an-agent.md)).
  `PUT /api/v1/configurations/{name}` now stores a **draft**; releasing it is its own act,
  `PUT /api/v1/configurations/{name}/publication` with `{"published": true}`, and editing a
  published Configuration stages the change (`pending_changes: true`) until the next publication.
  In the bundled UI the button that used to read *Save & distribute* is now *Save*, and *Publish*
  is what changes the fleet. Retracting (`{"published": false}`) removes the entry from every
  composed config map, which matching Agents apply.
  **What to do:** Configurations stored before the upgrade load as published and stay in force —
  running fleets are untouched. Scripts that `PUT` a Configuration and expect delivery need the
  one extra publication call.

### Added

- **A Configuration can state the Agent type it is for**
  ([ADR-0011](docs/adr/0011-configurations-and-the-rest-api.md)). The optional
  `service_name` field is compared raw against the `service.name` an Agent reports, before the
  Selector; unset keeps today's meaning, every type. The bundled UI offers the types the fleet
  currently reports as suggestions.
  **What to do:** nothing — existing Configurations are untyped and match as before. Prefer the
  field over a `service.name` Selector pair when creating new ones.

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

  No operator action required.

- **The package-source probe can no longer be aimed at internal addresses (SSRF).** `PUT
  /api/v1/packages/{name}/source` probes the operator-supplied URL once; that URL and its headers
  are entirely caller-supplied, so the probe could be pointed at the cloud metadata endpoint
  (`169.254.169.254`) or other internal services and the answer reflected back. The probe now
  refuses a URL that resolves to a link-local, shared/CGNAT, or other never-routable address, and it
  no longer follows redirects (which could bounce a public URL onto such an address). Loopback and
  RFC 1918 / unique-local addresses stay reachable on purpose — an operator's mirror (ADR-0018)
  legitimately lives on an internal network. No operator action required unless a source URL
  deliberately used a link-local or CGNAT host.

- **The body-less state-changing `POST` routes reject cross-site browser requests (CSRF).**
  `POST …/restart` and `POST …/rollback` are CORS "simple requests" a cross-origin page could fire
  at a logged-in operator's browser without a preflight. They now require Fetch Metadata to mark the
  request same-origin — a browser stamps `Sec-Fetch-Site` and forbids page scripts from forging it,
  so a cross-site call is refused with `403`. Non-browser clients (`curl`, a portal) send no such
  header and are unaffected; no API client or token changes. This is not operator authentication,
  which remains a separate decision (ADR-0026).

- **The package store has a whole-store size ceiling.** The upload route bounded a single artifact
  by `max_package_size_bytes` but nothing bounded the *store*, so a caller could fill the disk by
  uploading artifact after artifact under distinct names. Uploads are now also refused (`507`) once
  the stored artifacts reach the new `max_total_package_bytes` (default 16 GiB). **What to do:**
  nothing, unless a fleet's package set legitimately exceeds 16 GiB — then raise
  `max_total_package_bytes` in `server.toml`.

- **The Server-rotated connection credential is no longer left world-readable.** The
  `connection-settings.pb` in the state directory holds the `Authorization` value the Server rotates
  in (ADR-0027), which outranks the one in `client.toml`. It was written at the umask default
  (typically `0644`), so on a multi-user host any local user could read the live fleet credential.
  It is now written `0600` and its state directory `0700`. No operator action required.

- **The enrolment private key is written owner-only from the start.** `client-key.pem` (ADR-0026)
  was created at the umask default and narrowed to `0600` only afterwards, leaving a brief window in
  which another local user could read it. The mode is now set in the open call, closing the window.
  No operator action required.

- **A referenced package's private-source token is stored owner-only on the Server.** A referenced
  source (ADR-0018) can carry headers — a bearer token for a private artifact host — that were
  persisted in the package store at the umask default, readable by other local users on the Server
  host. The store directory is now `0700` and its metadata files `0600`. The token remains, by
  design, cleartext at rest and delivered to every targeted Agent; the API and store docs now say so.
  **What to do:** prefer a narrowly-scoped, rotatable token for a private source.

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

- **The artifact staging directory is kept owner-only.** A downloaded artifact is verified and then
  re-opened by the installer; the staging directory was created at the umask default, so on a
  multi-user host another local user could swap the file in that window and defeat the hash and
  signature check it had already passed (TOCTOU). The directory (`packages/` under the Agent's state
  or supervisor directory) is now `0700`. No operator action required.

- **A Client that installs packages without a verification key now says so at startup.** Package
  signing is opt-in (ADR-0018): with no `[packages] verification_key`, an offered package or
  self-update is accepted on the Server-supplied content hash alone, with no signature binding the
  bytes to a key the operator holds. That is unchanged — but a Client that accepts packages (a
  managed process's, or its own self-update) without a key now logs a warning at startup, so the
  weaker posture is a knowing choice rather than a silent default. **What to do:** to require an
  Ed25519 signature, set `[packages] verification_key` (see `opamp-package-sign`); otherwise nothing.

- **A package or self-update download now has a size ceiling.** The artifact was streamed to disk
  with no bound, so a malicious or compromised Server could answer the download with an endless body
  and fill the staging filesystem before the content hash — checked only once the whole stream lands
  — could reject it. The download is now capped at the new `max_artifact_size_bytes` (default one
  gibibyte, matching the Server's own per-package limit), enforced against an over-large
  `Content-Length` up front and while a chunked body streams in. **What to do:** nothing, unless a
  fleet distributes artifacts larger than 1 GiB — then raise `max_artifact_size_bytes` in
  `client.toml`.

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

- **Delete in the package form removes one artifact, not the whole package.** It deletes the
  platform named in the form — the artifact the selected chip stands for — and leaves the other
  platforms of that name alone. Before, one press on a form filled from a `linux-amd64` chip took
  the `darwin-arm64` and `windows-amd64` builds with it.

  The package itself goes when its last artifact does, so nothing is left behind either. Deleting
  uninstalls nothing: an Agent keeps running what it took, exactly as retracting does
  ([ADR-0014](docs/adr/0014-rollout-and-what-reaches-an-agent.md)).

  `DELETE /api/v1/packages/{name}` is unchanged and still deletes the whole package; the form now
  sends the `?os=…&arch=…` form of it that
  [ADR-0019](docs/adr/0019-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) added.

### Removed

- **The ↩ Roll back button is gone from the package form.** The store still remembers the version
  each artifact replaced ([ADR-0019](docs/adr/0019-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md)) and
  `POST /api/v1/packages/{name}/rollback?os=…&arch=…` still puts it back — only the button is
  removed. The package list still shows `0.157.0 ← 0.156.0`, so what "back" would be is still on
  screen; asking for it is now a request rather than a press.

### Added

### Changed

  Three actions replace *Upload & offer*, *Use source url*, *Set selector* and *Set agent type*:
  **A package chip toggles.** Clicking one fills the form from it; clicking the selected one again
  lets go, and the form describes no package in particular. Nothing is sent and nothing is deleted
  either way. The selection lives in the list, so undoing it is a press in the list rather than a
  button standing among the ones that write.

  name — that is the alternative ADR-0019 weighed and rejected.

### Fixed

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

- **Mutual TLS, with client certificates this Server issues itself**
  ([ADR-0026](docs/adr/0026-admission-by-a-client-certificate-alone.md)). Goal 17 is
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

- **`program_path` in a `[[supervisor]]` block delivers an agent that is more than one file**
  ([ADR-0018](docs/adr/0018-signed-package-delivery-from-allowed-sources.md)). An executable plus the shared objects it
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


- **A connection-settings offer carrying `tls` or `proxy` is no longer acknowledged `APPLIED`.**
  The Client never implemented those two fields and dropped them silently while reporting success,
  so a Server offering either was told the settings were in force when they were not. It now applies
  everything it does honour — endpoint, credential, heartbeat, certificate — and reports `FAILED`
  with an `error_message` naming what it dropped
  ([ADR-0026](docs/adr/0026-admission-by-a-client-certificate-alone.md)).

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
  [ADR-0019](docs/adr/0019-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md) offers "a Selector matching that host's
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
  never repaired either. The service is registered against `<root>/current/client` (ADR-0028), and
  asking the operating system what is running answers with that path on macOS and with the version
  directory behind it on Linux; only the second shape says where in the layout the binary sits. The
  path is now resolved before the layout is looked for, so both platforms answer the same. Nothing
  to change on a host: an affected Client picks its updates up as soon as it runs this version.

- **A package now holds one artifact per platform, and `os`/`arch` are required**
  ([ADR-0019](docs/adr/0019-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md)). An Agent is offered only the artifact
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
  ([ADR-0019](docs/adr/0019-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md)).

  **A Selector written against `host.arch` must be edited on the Server** — `{"host.arch": "x86_64"}`
  now matches nothing. Change it to `amd64` (or `aarch64` → `arm64`); nothing changes on any host.

  This also closes a quiet defect: a Managed Process's attributes are folded over the Supervisor's,
  and the Collector's `opampextension` already reported `amd64`. The same machine therefore changed
  architecture depending on whether a Collector happened to run on it, and Selectors written against
  either spelling broke without anything having changed.


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

