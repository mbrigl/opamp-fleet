# ADR-0030: The Server's configuration: TOML layered with its environment

- **Status:** 🟢 accepted
- **Date:** 2026-10-23
- **Deciders:** Markus Brigl
- **Applies to:** the loading of `server.toml` in `crates/fleet-server/src/config.rs` and the argument parsing in `crates/fleet-server/src/main.rs`, every subcommand of the `server` binary that reads its configuration, the loading of `supervisor.toml` in `crates/fleet-agent/src/config.rs`, the `[rest.auth]` and `[telemetry_offer]` sections and their `basic_users_file` and `headers_file` keys, `config/server.toml`, `docs/ARCHITECTURE.md`, and every document that tells an operator how to configure the Server

## Context

Both binaries read a TOML file: the Server `server.toml`, the Client `supervisor.toml`, each
parsed with the `toml` crate into `serde` structs that refuse unknown keys
([ADR-0008](0008-five-crates-on-tokio-and-axum.md)). A file is what an installer lays down and an
operator diffs, and TOML carries comments and the Client's per-Supervisor structure.

The Server runs in a container. A container is configured by its orchestrator, and the
orchestrator's natural channel is the environment: a variable per setting, changed per deployment
without rebuilding the image or mounting a file. Every setting of `server.toml` must be reachable
that way, the PostgreSQL connection and the object store of
[ADR-0029](0029-the-servers-state-and-its-stores.md) among them. The Client runs as an
operating-system service on the host it manages, not in a container.

Secrets must not travel in the environment. A container's environment is shown by `docker
inspect`, readable in `/proc/<pid>/environ` by the same user, copied into crash reports and
inherited by every child process. Docker's secrets reach a container as files under
`/run/secrets/`, and its official images read a `…_FILE` variable that names such a file. The
Server keeps every secret it needs in a file named by a `*_file` key: the PostgreSQL password,
the S3 secret key, the CA keys. Two tables of `server.toml` may hold sensitive values inline:
`[rest.auth.basic_users]`, whose Argon2id hashes an attacker can attack offline
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)), and
`[telemetry_offer.headers]`, which typically carries an access token for the receiving backend.

The keys of those two tables are data — user names and header names — not schema. An environment
variable name is upper-case and limited to letters, digits and `_` in practice, so a user name
like `fleet-admin` or a header like `X-Scope-OrgID` does not survive the trip through a variable
name.

A reader without this conversation would expect two things this ADR decides against: that the
environment may carry a password because twelve-factor apps do so, and that an unknown variable
with the Server's prefix is ignored, as most programs ignore the environment they do not know.

## Decision

We will configure both binaries from TOML and layer the Server's configuration as defaults, then
an optional `server.toml`, then every `FLEET_SERVER__…` variable of its environment, each
variable setting one key of the same document and checked as strictly as the file, and let the
environment carry paths to secrets but never a secret itself.

1. **Both binaries are configured from TOML; the Server's configuration may also come from its
   environment.** `server.toml` and `supervisor.toml` are parsed with the `toml` crate into
   `serde` structs. The Client takes its path from `--config`, defaulting to the file of that
   name in the working directory, runs on defaults when that file does not exist, and reads no
   setting from the environment. The Server builds one TOML document from three sources, each
   later one winning per key: its built-in defaults; `server.toml`, when one is read; and its
   environment, as clauses 2 to 6 say. `RUST_LOG` filters log output and configures nothing else
   in either binary.

2. **Which file the Server reads.** `--config <path>` names the file; without it,
   `FLEET_SERVER_CONFIG` names it. A file named either way that does not exist fails startup,
   naming the path. Without both, the Server reads `server.toml` in the working directory when
   it exists and starts from defaults and its environment when it does not. Every subcommand that
   reads the configuration — `server pki status`, `server migrate` and the ones that follow —
   reads the same layered document.

3. **One variable per key, named by its path.** A variable `FLEET_SERVER__` followed by the key's
   TOML path, its segments joined by `__` and each segment upper-cased, sets that key:
   `FLEET_SERVER__DATA_DIR` sets `data_dir`, `FLEET_SERVER__REST__TLS__CERT_FILE` sets
   `[rest.tls] cert_file`, `FLEET_SERVER__DATABASE__POSTGRES__HOST` sets `[database.postgres]
   host`. The name maps back by lower-casing each segment. `FLEET_SERVER_CONFIG`, with one
   underscore, is the file of clause 2 and no key.

4. **A value is a TOML value, and a bare word is a string.** The value is parsed as a TOML value:
   `5432` is an integer, `true` a boolean, `["a", "b"]` an array, `"text"` a string, and an
   inline table `{ … }` sets a table whole. A value that does not parse as TOML is taken as the
   string it is, so `/run/secrets/pg` needs no quotes. A string that parses as another type — a
   token label `1234`, say — is quoted: `"1234"`. A value whose type does not fit its key fails
   startup, naming the variable and the type the key expects.

5. **Tables keyed by data are set whole, and the sensitive ones from a file.** A table whose keys
   are names an operator chooses cannot be reached key by key from the environment.
   - `[rest.auth] basic_users_file` names a TOML file holding the `basic_users` table, readable
     by the Server's account alone. Naming it beside `[rest.auth.basic_users]` in the same
     document fails startup, naming both. The environment may set
     `FLEET_SERVER__REST__AUTH__BASIC_USERS_FILE` but not
     `FLEET_SERVER__REST__AUTH__BASIC_USERS`, which fails startup naming the file key.
   - `[telemetry_offer] headers_file` names a TOML file holding the `headers` table, under the
     same rules: never beside `headers`, and from the environment only as a path.
   - `server.toml` itself may still carry both tables inline; an operator who keeps the file
     outside the container decides how it is protected.
   - A table of this kind added later gets a `…_file` key under the same rules.

6. **The environment carries paths to secrets, never secrets.** Every secret the Server reads is
   named by a key ending in `_file`, and the file holds the secret. No key that holds a secret
   value exists, and none is added; a secret a later decision needs gets a `*_file` key. The
   environment sets such a key to a path, typically under `/run/secrets/`.

7. **The environment is checked as strictly as the file.** The variables are merged into the
   document before it is deserialized, so the merged document passes the same
   `deny_unknown_fields` checks and the same startup checks as a file. A `FLEET_SERVER__…`
   variable that names no key fails startup, naming the variable, as an unknown key in the file
   does. An empty segment (`FLEET_SERVER__REST____TLS`) fails startup the same way. At startup
   the Server logs, at info level, the name of every key its environment set, and never a value.

8. **No other variable configures the Server.** The `AWS_*` variables are read as
   [ADR-0029](0029-the-servers-state-and-its-stores.md) clauses 17 and 18 say: for credentials
   only, never for an endpoint or an option. `FLEET_TEST_*` variables are read by tests alone.
   Where ADR-0029 says the S3 connection is configured in `server.toml` alone, the environment of
   this ADR counts as `server.toml`: `FLEET_SERVER__BLOBS__S3__ENDPOINT` sets the endpoint, and
   `AWS_ENDPOINT` still does not.

9. **The merge is the Server's own.** `config.rs` builds a `toml::Table` from the file, overlays a
   table built from the environment, and deserializes the result once. No dependency is added.

**Out of scope:** an environment layer for the Client, whose `supervisor.toml` stays its whole
configuration — it runs as an operating-system service on the host it manages, not in a
container; reloading the configuration while the Server runs; a `…_FILE` companion for every key,
since only secrets need one and every secret is already a file.

## Alternatives considered

- **YAML.** The OpenTelemetry ecosystem's format, but with indentation and implicit-typing
  failure modes in hand-edited files, and `serde_yaml` is unmaintained.
- **JSON, or environment variables and flags only.** JSON has no comments; flags cannot carry the
  Client's per-Supervisor structure, and a file is what an installer lays down and an operator
  diffs.
- **The file as the whole configuration, no setting read from the environment.** It keeps one
  place to look, but a container would then need a file built into its image or mounted for every
  change its orchestrator makes. It stands for the Client, which runs in no container.
- **`figment` with its `Env` provider.** It maps `APP_A__B` to `a.b` with `split("__")`,
  lower-cases names, and parses values with the same TOML-like rules this ADR takes. It is a new
  dependency with its own provider stack, for a merge of two tables and a parse of each value
  that `toml` already does. Rejected.
- **The `config` crate.** Layered sources and environment overrides, but a larger surface —
  several formats, its own value type, a builder — and it loses `serde`'s field names in some
  error messages, which the loud validation of
  [ADR-0008](0008-five-crates-on-tokio-and-axum.md) relies on. Rejected.
- **A fixed list of documented variables.** Each needs code and documentation, and a key added
  later is out of reach of a container until someone adds its variable. Rejected.
- **Secrets as plain variables beside their `…_file` keys** (`…_PASSWORD` as well as
  `…_PASSWORD_FILE`). Convenient, but the value then shows in `docker inspect`, in
  `/proc/<pid>/environ` and in crash reports. Docker itself keeps secrets out of the environment
  for this reason. Rejected.
- **Ignore unknown `FLEET_SERVER__…` variables.** It tolerates a typo, which then leaves a setting
  silently at its default — the failure that the strict parse of
  [ADR-0008](0008-five-crates-on-tokio-and-axum.md) exists to prevent. Rejected.
- **A single underscore as separator.** Keys like `data_dir` and `cert_file` contain underscores
  themselves, so `FLEET_SERVER_REST_TLS_CERT_FILE` has no unique reading. Rejected.
- **Generate the file from the environment in the image's entrypoint.** It needs a shell or a
  template tool in the image and duplicates the schema in a script. Rejected.

## Sources / Prior art

- [TOML v1.0](https://toml.io/), the [`toml` crate](https://crates.io/crates/toml), and the
  [`serde_yaml` deprecation notice](https://github.com/dtolnay/serde-yaml);
  [`opampsupervisor` configuration](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the YAML prior art not followed for this project's own files.
- [The Twelve-Factor App, III. Config](https://12factor.net/config): config in environment
  variables, changeable per deploy without changing code.
- [Docker, Manage sensitive data with Docker
  secrets](https://docs.docker.com/engine/swarm/secrets/):
  secrets mounted under `/run/secrets/<name>`, not set as environment variables because those can
  leak, and the `…_FILE` convention of Docker's official images.
- [`figment` 0.10.19,
  `providers::Env`](https://docs.rs/figment/latest/figment/providers/struct.Env.html):
  `split`, lower-casing, and TOML-like value parsing where an undelimited string is taken as
  written.
- The code as it stands: `ServerConfig::load` and the `deny_unknown_fields` structs in
  `crates/fleet-server/src/config.rs`, `parse_args` in `crates/fleet-server/src/main.rs`.

## Consequences

- Positive: a container is configured by its orchestrator alone; no file needs to be built into
  the image or mounted, and every key, present and future, is reachable without code per
  variable.
- Positive: a typo in a variable name stops the Server instead of leaving a setting at its
  default, and the log says which keys the environment set.
- Positive: no secret appears in the container's environment; secrets stay files, as
  orchestrators deliver them.
- Negative / trade-offs: operators coming from OpenTelemetry meet a second format for the fleet
  tooling itself.
- Negative / trade-offs: two places can set a key, and the environment silently wins over the
  file. The startup log's list of keys set by the environment is what tells an operator which
  won.
- Negative / trade-offs: a string that looks like a number needs quotes inside the variable's
  value, which is easy to forget; the startup error names the variable when it happens.
- Negative / trade-offs: an explicitly named configuration file that does not exist stops the
  Server.
- Negative / trade-offs: any other process started with a `FLEET_SERVER__…` variable in its
  environment, such as a test runner, refuses to start the Server on an unknown one.
- Follow-ups: whether `supervisor.toml` gains the same layering should the Client ever run in a
  container.

## Enforcement

Tests that carry `Verifies: ADR-0030`:

- [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `the_environment_overrides_the_files_keys_and_keeps_the_rest`,
  `a_key_is_set_by_one_upper_case_variable`,
  `without_a_file_the_configuration_is_built_from_defaults_and_the_environment`,
  `a_named_configuration_file_that_does_not_exist_fails_startup_naming_it`,
  `an_unknown_variable_fails_startup_naming_it`,
  `a_value_of_the_wrong_type_fails_startup_naming_the_variable_and_the_type`,
  `a_bare_word_is_a_string_and_a_quoted_number_too`,
  `a_table_file_beside_its_table_fails_startup_naming_both`,
  `the_environment_names_credentials_by_file_alone`,
  `a_basic_users_file_with_valid_hashes_admits_its_users`,
  `a_mistake_in_the_file_names_its_line_whatever_the_environment_sets`,
  `the_startup_log_names_the_keys_the_environment_set_and_no_value`.
- [`crates/fleet-server/tests/pki_cli.rs`](../../crates/fleet-server/tests/pki_cli.rs):
  `status_reads_its_configuration_without_config_from_the_variable_or_the_environment`
  (clause 2), `the_server_logs_the_keys_its_environment_set_before_it_serves` (clause 7).
