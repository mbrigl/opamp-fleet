# CLT-telegraf-kind: The bundled UI colours an Agent's effective configuration with Shiki, and Telegraf gets a kind of its own that reloads it the platform's way, delivered as upstream's single-file archive

- **Status:** 🟡 proposed
- **Date:** 2026-11-15
- **Deciders:** Markus Brigl
- **Applies to:** the effective-configuration cell and dialog of the Agents tab in `crates/fleet-server/static/index.html`, the vendored `crates/fleet-server/static/shiki.js` and the route that serves it in `crates/fleet-server/src/api.rs`, the token colours in `crates/fleet-server/static/ui.css`, `ui-carbon.css` and `ui-quiet.css`, the script that builds the vendored file under `scripts/`, `NOTICE`, `crates/fleet-agent/src/supervisor/telegraf.rs`, its tests in `crates/fleet-agent/tests/telegraf_supervisor.rs`, the `telegraf` feature of `crates/fleet-agent/Cargo.toml`, the refusal of a `program_path` a wrapped kind derives in `crates/fleet-agent/src/supervisor/block.rs`, `telegraf_plans`, `TELEGRAF_PLATFORMS`, the release-notes checksum and the `.asc` check of opamp-fleetctl package fetch in `crates/fleet-tools/src/fetch.rs`, `crates/fleet-tools/trust/influxdata-archive.gpg`, `config/examples/telegraf-conf.toml`, and `docs/artifacts/telegraf.md`
- **Supersedes:** [SRV-ui-colours-with-shiki](SRV-ui-colours-with-shiki.md)

## Context

This ADR carries every clause of [SRV-ui-colours-with-shiki](SRV-ui-colours-with-shiki.md) and
changes one: the grammar map of clause 3 gains `telegraf` → TOML. It adds the `telegraf` kind as
clauses 6 to 12, after SRV-ui-colours-with-shiki's, so every clause of SRV-ui-colours-with-shiki
keeps its number. SRV-ui-colours-with-shiki's own Context follows, then the kind's.

This draft first stood alone, with the kind's clauses numbered from 1. Its clause map, from that
draft (commit 4ec63bdbb) to this one:

| Draft clause | Here | Subject |
|---|---|---|
| 1 | 6 | the `telegraf` kind knows the invocation and has no settings |
| 2 | 7 | the reload is the kind's, platform-correct by construction |
| 3 | 8 | a single-file package, installed as published |
| 4 | 9 | Telegraf does not speak OpAMP to the Client |
| 5 | 10 | the Configuration name is part of the contract |
| 6 | 11 | the artifact document held by two tests |
| 7 | 3 | the UI colours a Telegraf configuration as TOML |
| — | 12 | Telegraf does not write the cgroup tree (new) |

### The bundled UI's colouring

The Agents tab shows each Agent's effective configuration in a table cell and in a dialog. These
texts are long YAML or TOML files, hard to read without colour. The human wants them coloured
with TextMate grammars, the ones VS Code uses, and the colouring done locally in the browser. Only
the effective configuration is coloured; the Configuration editor is not.

Forces:

- The bundled UI is static assets embedded in the Server binary, plain HTML, CSS and JS, with
  **no frontend toolchain**, neither in the Dev Container nor in CI
  ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)). Redoc is vendored as one pre-built
  file and named in `NOTICE`; that is the precedent.
- The Operator plane's CSP is `script-src 'self'` plus the hashes of the inline scripts, and no
  `'wasm-unsafe-eval'`. TextMate grammars are written for the Oniguruma regex engine, which
  usually runs as WebAssembly. Allowing WebAssembly would weaken the policy (Q-1).
- Shiki is the library that brings VS Code's TextMate grammars to the browser. It has a
  JavaScript regex engine (`@shikijs/engine-javascript`) that translates the grammars' Oniguruma
  patterns into JavaScript regular expressions, with no WebAssembly. Shiki 4.5.0 is published
  under the MIT licence.
- Shiki ships as ES modules with dependencies. It has no single pre-built browser file like
  Redoc's, so one has to be built once.
- The supply-chain check (Q-3, `cargo-deny`) covers Rust crates only. A vendored JavaScript file
  is outside it, as Redoc is.
- The Server knows no configuration format (Non-Goal "Replacing an agent's configuration
  language"). Choosing a grammar for display is presentation and changes nothing the Server
  stores, sends or decides.

### The `telegraf` kind

The fleet manages InfluxData's Telegraf beyond the Collector. It should be an ordinary Managed
Process: a package the Client installs into its own Supervisor directory, updates with a health
gate and rolls back, a Configuration rolled out from the Server, a restart issued from it. A kind
knows its own agent ([CLT-supervisor-mode-and-kinds](CLT-supervisor-mode-and-kinds.md)): what
follows from the artifact and the platform is compiled in, and a block holds only what an operator
decides.

Telegraf is a single-file program. Its whole invocation is Telegraf's own: the program name,
`--config`, `--version`, and `SIGHUP` to re-read its configuration. The reload is the sharpest case
for a kind: a reload signal is Unix-only, so a block naming one is refused on Windows and no one
block could serve a mixed fleet.

## Decision

We will colour an Agent's effective configuration in the bundled UI with Shiki 4.5.0 and its
JavaScript regex engine, vendored as one pre-built file whose version and SHA-256 are pinned and
checked by a test, and whose colours come from each look's stylesheet, Telegraf's as TOML; and we
will supervise Telegraf with a `telegraf` kind whose block is `type` and `name` alone, reload it
the way its platform allows, and deliver it as upstream's own archives, installed as published.

1. **One vendored file, built once, never in CI.**
   - `crates/fleet-server/static/shiki.js` is a single browser script built from `shiki@4.5.0`
     with `@shikijs/engine-javascript`, the grammars of clause 3 and Shiki's CSS-variables theme.
     It exposes one function on `window`. The file starts with a header naming the packages,
     their versions and the script that built it.
   - `scripts/vendor-shiki.sh` rebuilds it. A developer runs it on their own machine, and only
     when the version is to change. CI and the Dev Container need no `node`; the bundled UI keeps
     no frontend toolchain ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)).
   - The Server embeds the file and serves it from its own origin as `/shiki.js`, like the
     stylesheets. The CSP does not change: `script-src 'self'` covers it, and nothing needs
     WebAssembly or `eval`.

2. **The version and the hash are pinned and checked.**
   - The file's SHA-256 and the version are written beside the build script.
   - A test fails when the embedded file's hash differs from the pinned one. The file therefore
     cannot change without the pin changing in the same commit.
   - `NOTICE` names Shiki and every package and grammar bundled, with their licences. Shiki and its
     packages are MIT. The YAML and TOML grammars come from TextMate's bundles under TextMate's own
     permissive notice, the INI grammar from VS Code under MIT.
   - The bundle's header carries both licence texts.

3. **The grammar follows the Agent type, and anything else stays plain.** The UI chooses the
   grammar from the Agent's reported `service.name`:
   - `otelcol`, `otelcol-contrib` and any name starting `otelcol` → YAML;
   - this fleet's own Client (`supervisor`) → TOML;
   - Telegraf (`telegraf`, the kind of clause 6) → TOML, its configuration language;
   - `fluent-bit` → INI;
   - any other type → no colouring, the text shown plain.

   Only YAML, TOML and INI are bundled. A grammar is added when a supported kind needs it. The
   `telegraf` kind needs none: its TOML is the grammar the UI already bundles.

4. **The colours are the look's.** The theme emits `var(--shiki-…)` colours. Each of `ui.css`,
   `ui-carbon.css` and `ui-quiet.css` defines them for its light and dark theme. A coloured
   configuration therefore follows the look and the theme the operator chose.
   - The theme is Shiki's CSS-variables theme with token rules of this project's on top. The
     CSS-variables theme alone leaves TOML table headers and keys, and everything in INI but its
     comments, in the foreground colour. The added rules map the TOML and INI scopes for table and
     section headers, keys, values and operators onto the same `--shiki-token-*` variables. No
     stylesheet needs a variable of its own for them.
   - An unquoted INI value, and a `key: value` or Fluent Bit `Name cpu` line, stay in the
     foreground colour. The bundled INI grammar gives them no scope of their own, and a rule on
     the whole `source.ini` would colour every line alike.

5. **Colouring is display only, and it never blocks.**
   - The UI colours the text after the table is drawn, on the cell and in the dialog.
   - If the script fails to load or a grammar throws, the plain text stays.
   - The UI escapes the text as for any other cell. Shiki builds its HTML from tokens of the text,
     not from markup in it.

6. **The `telegraf` kind knows the invocation and has no settings.** Program `telegraf` plus
   `EXE_SUFFIX`; arguments `--config ${config_dir}/telegraf-conf`; `--version` as version probe
   and preflight, read as strict SemVer; `service_name = "telegraf"`. The block is
   `type = "telegraf"` and `name`; a block carrying `args`, `version_args`, `reload_signal`, `env`
   or `working_dir` fails at startup naming what supplies the value, and so does one restating a
   value the kind derives, its program key `command` or `program_path`
   ([CLT-supervisor-mode-and-kinds §13](CLT-supervisor-mode-and-kinds.md)).

7. **Telegraf's reload is the kind's, platform-correct by construction:** `SIGHUP` on Unix, the
   Runner's restart on Windows. Neither is written in a block, so one Supervisor set serves a mixed
   fleet.

8. **Telegraf is a single-file package, installed as published.** Assets are
   `telegraf-<version>_<os>_<arch>.tar.gz` (`.zip` for Windows, opened as
   [PKG-signed-packages-and-delivery §62](PKG-signed-packages-and-delivery.md) decides) from
   `https://dl.influxdata.com/telegraf/releases/`. Versions are the `v<major>.<minor>.<patch>`
   tags of `influxdata/telegraf`. The CDN has no listing, so the platform list is the tool's own
   (`TELEGRAF_PLATFORMS`: linux amd64/arm64/386, darwin amd64/arm64, windows amd64/arm64;
   upstream spells `386` as `i386`). It holds the builds whose Platform is beyond doubt.
   `linux_armhf` and `linux_armel` stay out: upstream does not state the ARM level they are built
   for, and [PKG-signed-packages-and-delivery §6](PKG-signed-packages-and-delivery.md) keeps `armv6`
   apart from `arm32`, so an `arm32` entry would be offered to a host it does not run on.
   - **Each release file is verified against two origins.** The SHA-256 is the one the GitHub
     release notes of the tag list in their table, the row found by the release file's exact name.
     It is required: an release file the notes name nowhere is refused, because only the host that
     serves it would then vouch for it. The `.DIGESTS` file beside the release file on the CDN, its
     line looked up by name, must agree where it exists. From 1.39.0 on InfluxData puts one beside
     every release file, and there a `404` refuses the release file, so that whoever serves the CDN
     cannot switch the cross-check off by deleting the file. Before 1.39.0 it exists for a few
     release files of a few releases from 1.36.4 on, and a `404` is its absence.
   - **Each release file carries InfluxData's signature.** The `.asc` beside the release file is
     verified as [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md)
     verifies an upstream's OpenPGP signature, against InfluxData's package-signing key, which the
     tool carries (`crates/fleet-tools/trust/influxdata-archive.gpg`), with the primary key
     `24C9 75CB A61A 024E E1B6 3178 7C3D 5715 9FC2 F927` pinned. The SHA-256 checks stay. From
     1.34.4 on InfluxData puts an `.asc` beside every release file, and there a `404` refuses the
     release file. Before 1.34.4 a `404` is the signature's absence, as for `.DIGESTS`, and the
     digests decide. Where an `.asc` exists it must verify, at any version.
   - **The kind names where the archive holds the program:** `usr/bin/telegraf` on Unix and
     `telegraf.exe` on Windows, below the version-named directory, as the `member` that
     [PKG-signed-packages-and-delivery §55](PKG-signed-packages-and-delivery.md) lets a kind name.
     The file name alone does not decide it: the Unix tarballs also hold `etc/logrotate.d/telegraf`
     and directories named `telegraf`. Only the program is unpacked, so the kind has no
     `program_path`.

9. **Telegraf does not speak OpAMP to the Client.** The kind refuses an `endpoint_port`; its
   Endpoint is bound and nothing connects.

10. **The Configuration name is part of the contract.** `opamp-fleetctl package fetch` uploads
    `telegraf-conf` (body `config/examples/telegraf-conf.toml`) beside the package when the Server
    holds none of that name, never overwriting an existing one and distributing nothing. The name is
    the file name the kind points its agent at, so renaming it on one side only leaves the agent
    reading a file that does not exist.

11. **The kind has an artifact document held by two tests.** `docs/artifacts/telegraf.md` states
    the eight sections of [CLT-supervisor-mode-and-kinds §20](CLT-supervisor-mode-and-kinds.md):
    source, assets, integrity, treatment, shape in the delivered tree, what the Client derives, the
    Configurations, and what goes red when upstream moves something — one test on the packing
    side, one on the client side, `cfg`-gated per platform where the facts differ.

12. **Telegraf does not write the cgroup tree.** Every system unit carries
    `ProtectControlGroups=yes`
    ([CLT-accounts-service-runs-under §8](CLT-accounts-service-runs-under.md)), which makes
    `/sys/fs/cgroup` read-only, and a kind of its own states of its agent whether that takes
    anything from it. It takes nothing from Telegraf: its `cgroup` input, the one made for the
    cgroup tree, reads the files under `/sys/fs/cgroup` that its `paths` select and writes none.

**Out of scope:** colouring or editing the Configuration body; a code editor; a grammar per
Configuration chosen by the operator; formatting (re-indenting) any configuration; WebAssembly in
the browser.

**Out of scope for the `telegraf` kind:** following InfluxData's key at run time and a revocation
its carried keyring does not state, which
[OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md) leaves out; an account for
Telegraf other than the one [CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)
gives the Client or its second service, for which the kind has no key; and what Telegraf collects
and where it sends it, which is the operator's `telegraf-conf`.

## Alternatives considered

- **highlight.js or Prism.** They are small (20–50 KB) and ship as single files. Their regular
  expressions are their own, not TextMate grammars, and colour YAML and TOML less exactly. The
  human asked for TextMate.
- **Shiki with the Oniguruma engine.** It is the reference engine, but it needs WebAssembly, so
  the CSP would need `'wasm-unsafe-eval'`. The JavaScript engine reaches the same grammars
  without it.
- **Loading Shiki from a CDN.** It needs no vendoring. It breaks the UI on a host without
  internet, hands the page to a third party, and would need the CSP opened to that origin.
- **A node toolchain in CI that builds the file on every build.** It keeps the file reproducible at
  all times. [DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md) rejects
  this cost for a rudimentary UI, and the pinned hash (clause 2) catches an unreviewed change.
- **Colouring on the Server.** Shiki could run there through a JavaScript runtime or a Rust port
  of TextMate (`syntect`). It adds a dependency to the Server binary for a display concern, and
  colours that would not follow the look.
- **A `command` recipe per platform.** Rejected: the block cannot be written as one block for a
  mixed fleet at all, because its reload signal is refused on Windows.
- **One block with placeholders** (`${exe_suffix}` and friends). Rejected: it turns the operator's
  file into a template of the artifact's layout.
- **Let the kind accept `args`** as an escape hatch. Rejected: a kind that needs one does not know
  its agent; an operator's choices belong in the agent's own configuration, and `command` remains
  for a genuinely different invocation.
- **A generic "single-file agent" kind** parameterised by name. Rejected: its parameters would be
  the keys the `telegraf` kind removes — `command` under a shorter name.
- **A kind that drives the native service managers** (`systemctl`, `sc.exe`). Rejected: supervising
  a process the Client did not spawn means no watchdog, no health gate, no bounded stop.

## Sources / Prior art

- npm registry: `shiki` 4.5.0, published 2026-10-01, licence MIT; its dependencies include
  `@shikijs/engine-javascript` and `@shikijs/langs`; the CSS-variables theme comes from Shiki's
  core, so `@shikijs/themes` is not bundled.
- Shiki documentation: the JavaScript regex engine and the CSS-variables theme.
- [DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md) on the bundled UI
  without a frontend toolchain, and its alternative "A frontend framework and bundler for the UI";
  `NOTICE` (Redoc vendored as a pre-built file); the Operator plane's CSP in
  `crates/fleet-server/src/api.rs`.
- [Telegraf configuration](https://docs.influxdata.com/telegraf/v1/configuration/) — `--config`,
  and reload on `SIGHUP`.
- InfluxData's release archives at `dl.influxdata.com` with their `.DIGESTS` files and `.asc`
  signatures; the `.DIGESTS` of the seven assets this fleet fetches were probed for the 40 releases
  from 1.31.3 to 1.40.2 on 2026-10-28: all seven from 1.39.0 on, none or a few before (the first,
  1.36.4 linux/amd64; none in 1.38.0 to 1.38.3),
  and the archives' layout, read from the 1.36.4 linux/amd64, darwin/arm64 and windows/amd64
  archives on 2026-10-25.
- InfluxData's package-signing key at `https://repos.influxdata.com/influxdata-archive.key`, read
  on 2026-10-28: primary `24C9 75CB A61A 024E E1B6 3178 7C3D 5715 9FC2 F927` (2023-01-18, no
  expiry), signing subkeys `9D53 9D90 D332 8DC7 D6C8 D3B9 D8FF 8E1F 7DF8 B07E` (expired
  2026-01-17) and `AC10 D744 9F34 3ADC EFDD C2B6 DA61 C26A 0585 BD3B` (2025-07-10 to 2029-01-17).
  [Package Signing Key Rotation](https://www.influxdata.com/blog/package-signing-key-rotation/)
  (InfluxData, updated 2026-01-06) names the same primary and both subkeys, and says new builds
  are signed with the new subkey.
- The `.asc` beside the linux/amd64 and windows/amd64 archives of every release tag, probed on
  2026-10-28: published for both from 1.34.4 to 1.40.2 without a gap, and for every platform of
  1.34.4, 1.35.0 and 1.40.2; before 1.34.4 only for the linux/amd64 archive of a few releases
  (1.13.4, 1.33.0, 1.34.1 among them). Every one read was made by the 2023 subkey up to 1.37.0
  and by the 2025 subkey from 1.37.1; `gpgv` verified those of 1.40.2 and 1.36.0 with the key
  above.
- The release notes of `influxdata/telegraf` on GitHub, read through the API on 2026-10-25 for
  1.20.0, 1.24.0, 1.28.0, 1.30.0, 1.33.0, 1.35.0, 1.36.3, 1.36.4 and 1.38.0: each carries a table
  of `<arch> | <os> | <asset as a link> | <sha256>` rows, the name and the digest in backticks
  in 1.20.0 and 1.24.0 and bare from 1.28.0.
- Telegraf's `cgroup` input, `plugins/inputs/cgroup/cgroup_linux.go` and its README on the
  `master` branch of `influxdata/telegraf`, read on 2026-11-15: the plugin reads the files its
  `paths` select with `os.ReadFile`, finds them with `os.Stat` and `filepath.Glob`, and writes
  nothing.

## Consequences

- Positive: effective configurations are readable, coloured with the grammars operators know from
  VS Code, in the look's own colours, with no change to the CSP and no network access.
- Negative / trade-offs: the Server binary grows by the size of the bundle, about 190 KB.
- Negative / trade-offs: the JavaScript file is outside `cargo-deny`. The pinned hash makes a
  change visible but does not scan for vulnerabilities; updating Shiki is a manual step with the
  script.
- Negative / trade-offs: the JavaScript engine may fail on a rare grammar pattern. The text then
  stays plain (clause 5).
- Positive: one block for every platform and for a mixed fleet; the reload is right everywhere
  without the operator choosing, and keeps Telegraf's state across a configuration change where
  signals exist.
- Positive: the artifact travels exactly as upstream published it, verified against upstream's own
  digest.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests.
- Negative: a fetch trusts GitHub's release notes and InfluxData's CDN together, and is refused
  for a release whose notes carry no SHA-256 table. It reads the release's notes where a
  Collector's fetch reads its assets, so a run makes as many requests against GitHub's
  unauthenticated rate limit as a Collector's: the tags and one release.
- Positive: from 1.34.4 on an asset is held to InfluxData's own key as well, so neither GitHub
  nor the CDN, nor both together, can swap it unnoticed.
- Negative: the tool carries InfluxData's key and has to follow it: when InfluxData signs with a
  subkey the carried keyring lacks, every fetch of the new releases is refused until a release of
  the tool carries the new key. A fetch of 1.34.4 or later needs `gpgv` on the host that runs it,
  which Debian and Ubuntu install with `apt` and macOS does not install by default.
- Negative / trade-offs: Telegraf runs under the Client's account
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md)), which a Linux package
  makes non-root, so an input that needs root fails there. The unit sets nothing that implies
  `no_new_privs` (CLT-accounts-service-runs-under §8), so `use_sudo` with a `sudoers` rule for the
  account works; `--run-as root` is the other way. Where a process account is registered
  (CLT-accounts-service-runs-under §18), Telegraf runs under that account, which is never root, and
  the rule names it. On Windows a first registration runs the service as its own virtual account
  `NT SERVICE\<PRODUCT_NAME>`, and a re-registration keeps the account the SCM holds, `LocalSystem`
  included (CLT-accounts-service-runs-under §1); Telegraf runs under that account, and `--run-as`
  chooses another. The kind has no key for either.
- Follow-ups:
  - Colouring the Configuration body while it is edited.
  - A supply-chain check that covers vendored JavaScript as well as Rust crates.

## Enforcement

Clauses 1 to 5, by the tests that carry `Verifies: SRV-ui-colours-with-shiki` until this ADR is
accepted, in [`crates/fleet-server/tests/rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs):

- `the_ui_serves_the_pinned_shiki_bundle`: the embedded `shiki.js` has the pinned SHA-256, and
  `GET /shiki.js` answers the file with a JavaScript content type, under the unchanged CSP.
- `the_ui_colours_an_effective_configuration_by_the_agents_type`: the UI loads `/shiki.js` and
  maps `otelcol*` to YAML, `supervisor` and `telegraf` to TOML, and `fluent-bit` to INI (clause
  3), and each of the three stylesheets defines the CSS-variables theme's colours for light and
  dark.

Clauses 6 to 12, by the tests that carry `Verifies: CLT-telegraf-kind`:

- `crates/fleet-agent/src/supervisor/telegraf.rs`: `the_block_has_no_settings`,
  `the_recipes_keys_are_refused_by_name`, `the_invocation_points_at_the_delivered_configuration`,
  `the_defaults_are_the_artifacts` (the client side of clause 11),
  `restating_a_derived_value_is_refused` (clause 6: the program key `command` and
  `program_path`, which the core refuses for every wrapped kind, as
  [CLT-supervisor-mode-and-kinds §13](CLT-supervisor-mode-and-kinds.md) asks), and
  `the_program_is_taken_from_its_place_in_upstreams_archive` (clause 8: the archive's layout, with
  the further entries named `telegraf` ahead of the program).
- `crates/fleet-agent/tests/telegraf_supervisor.rs`:
  `a_two_line_telegraf_block_runs_reports_and_applies` — the two-line block runs, reports its
  version, and applies a Configuration delivered under the name `--config` points at;
  `telegraf_applies_a_configuration_by_sighup_in_place` (Unix) and
  `telegraf_applies_a_configuration_by_restarting_on_windows` (Windows) — clause 7.
- `crates/fleet-tools/src/fetch.rs`:
  `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` (the packing side of
  clause 11), `the_release_notes_digest_is_read_by_the_assets_name`,
  `an_artifact_is_held_to_its_release_notes_and_to_the_digests_beside_it`,
  `digests_that_are_not_published_leave_the_release_notes_to_decide`,
  `the_carried_key_verifies_influxdatas_own_signature` (the carried key and the pinned primary,
  and not the subkey that signed; the verification itself is tested with
  [OPS-operator-program-fleetctl §29](OPS-operator-program-fleetctl.md)),
  `a_missing_signature_refuses_only_a_release_that_signs_every_asset` and
  `a_published_signature_that_does_not_verify_refuses_the_artifact_at_any_version` (clause 8),
  `a_published_checksum_is_read_from_either_shape`,
  `every_agent_carries_a_storable_default_configuration`,
  `a_default_configuration_the_server_already_holds_is_not_written_over`.
- `crates/fleet-agent/src/service/manager.rs`:
  `an_account_unit_sets_nothing_that_implies_no_new_privs` and
  `the_second_services_unit_runs_the_program_under_the_process_account` — neither the Client's unit
  nor the second service's sets `NoNewPrivileges=`, so `use_sudo` keeps working (Consequences).

**Not mechanically decidable:** whether upstream still reloads Telegraf on `SIGHUP` is a run-time
property no repository test can see; the artifact document names this row as a warning. That
Telegraf writes nothing under `/sys/fs/cgroup` (clause 12) is a property of its code, read from
its source and not tested here.
