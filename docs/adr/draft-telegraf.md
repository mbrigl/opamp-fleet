# ADR-draft-telegraf: Telegraf gets a kind of its own that reloads it the platform's way, and its package is upstream's single-file archive as published

- **Status:** 🟡 proposed
- **Date:** 2026-10-10
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-agent/src/supervisor/telegraf.rs`, its tests in `crates/fleet-agent/tests/telegraf_supervisor.rs`, the `telegraf` feature of `crates/fleet-agent/Cargo.toml`, `telegraf_plans`, `TELEGRAF_PLATFORMS`, the release-notes checksum and the `.asc` check of opamp-fleetctl package fetch in `crates/fleet-tools/src/fetch.rs`, `crates/fleet-tools/trust/influxdata-archive.gpg`, `config/examples/telegraf-conf.toml`, `docs/artifacts/telegraf.md`, the `telegraf` entry of the UI's grammar map in `crates/fleet-server/static/index.html`

## Context

The fleet manages InfluxData's Telegraf beyond the Collector. It should be an ordinary Managed
Process: a package the Client installs into its own Supervisor directory, updates with a health
gate and rolls back, a Configuration rolled out from the Server, a restart issued from it. A kind
knows its own agent ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)): what follows from the
artifact and the platform is compiled in, and a block holds only what an operator decides.

Telegraf is a single-file program. Its whole invocation is Telegraf's own: the program name,
`--config`, `--version`, and `SIGHUP` to re-read its configuration. The reload is the sharpest case
for a kind: a reload signal is Unix-only, so a block naming one is refused on Windows and no one
block could serve a mixed fleet.

## Decision

We will supervise Telegraf with a `telegraf` kind whose block is `type` and `name` alone, reload it
the way its platform allows, and deliver it as upstream's own archives, installed as published.

1. **The `telegraf` kind knows the invocation and has no settings.** Program `telegraf` plus
   `EXE_SUFFIX`; arguments `--config ${config_dir}/telegraf-conf`; `--version` as version probe
   and preflight, read as strict SemVer; `service_name = "telegraf"`. The block is
   `type = "telegraf"` and `name`; a block carrying `args`, `version_args`, `reload_signal`, `env`
   or `working_dir` fails at startup naming what supplies the value.

2. **Telegraf's reload is the kind's, platform-correct by construction:** `SIGHUP` on Unix, the
   Runner's restart on Windows. Neither is written in a block, so one Supervisor set serves a mixed
   fleet.

3. **Telegraf is a single-file package, installed as published.** Assets are
   `telegraf-<version>_<os>_<arch>.tar.gz` (`.zip` for Windows, opened as
   [ADR-0027](0027-packages-signatures-and-delivery.md) clause 62 decides) from
   `https://dl.influxdata.com/telegraf/releases/`. Versions are the `v<major>.<minor>.<patch>`
   tags of `influxdata/telegraf`. The CDN has no listing, so the platform list is the tool's own
   (`TELEGRAF_PLATFORMS`: linux amd64/arm64/386, darwin amd64/arm64, windows amd64/arm64;
   upstream spells `386` as `i386`).
   - **Each asset is verified against two origins.** The SHA-256 is the one the GitHub release
     notes of the tag publish in their table, the row found by the asset's exact name. It is
     required: an asset the notes name nowhere is refused, because only the host that serves it
     would then vouch for it. The `.DIGESTS` file beside the asset on the CDN, its line looked up
     by name, must agree where it is published. From 1.39.0 on InfluxData publishes one beside
     every asset, and there a `404` refuses the asset, so that whoever serves the CDN cannot
     switch the cross-check off by deleting the file. Before 1.39.0 it exists for a few assets of
     a few releases from 1.36.4 on, and a `404` is its absence.
   - **Each asset carries InfluxData's signature.** The `.asc` beside the asset is verified as
     clause 29 of
     [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
     verifies an upstream's OpenPGP signature, against InfluxData's package-signing key, which the
     tool carries (`crates/fleet-tools/trust/influxdata-archive.gpg`), with the primary key
     `24C9 75CB A61A 024E E1B6 3178 7C3D 5715 9FC2 F927` pinned. The SHA-256 checks stay. From
     1.34.4 on InfluxData publishes an `.asc` beside every asset, and there a `404` refuses the
     asset. Before 1.34.4 a `404` is the signature's absence, as for `.DIGESTS`, and the digests
     decide. Where an `.asc` is published it must verify, at any version.
   - **The kind names where the archive holds the program:** `usr/bin/telegraf` on Unix and
     `telegraf.exe` on Windows, below the version-named directory, as the `member` that clause 55
     of [ADR-draft-packages-signatures-and-delivery](draft-packages-signatures-and-delivery.md)
     lets a kind name. The file name alone does not decide it: the Unix tarballs also hold
     `etc/logrotate.d/telegraf` and directories named `telegraf`. Only the program is unpacked, so
     the kind has no `program_path`.

4. **Telegraf does not speak OpAMP to the Client.** The kind refuses an `endpoint_port`; its
   Endpoint is bound and nothing connects.

5. **The Configuration name is part of the contract.** `opamp-fleetctl package fetch` uploads
   `telegraf-conf` (body `config/examples/telegraf-conf.toml`) beside the package when the Server
   holds none of that name, never overwriting an existing one and distributing nothing. The name is
   the file name the kind points its agent at, so renaming it on one side only leaves the agent
   reading a file that does not exist.

6. **The kind has an artifact document held by two tests.** `docs/artifacts/telegraf.md` states
   source, assets, integrity, treatment, form in the tree, what the Client derives, and what goes
   red when upstream moves something — one test on the packing side, one on the client side,
   `cfg`-gated per platform where the facts differ.

7. **The UI colours a Telegraf configuration as TOML.** Telegraf's configuration language is
   TOML, so the UI's grammar map ([ADR-0031](0031-the-ui-colours-configurations-with-shiki.md)
   clause 3) names the type `telegraf` beside the Client's own `supervisor` with the TOML grammar
   the UI already bundles. The kind adds no grammar.

**Out of scope:** following InfluxData's key at run time and a revocation its carried keyring does
not state, which clause 29 of
[ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)
leaves out; an account for Telegraf other than the one
[ADR-0037](0037-the-accounts-the-service-runs-under.md) gives the Client or its second service,
for which the kind has no key; and what Telegraf collects and where it sends it, which is the
operator's `telegraf-conf`.

## Alternatives considered

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

## Consequences

- Positive: one block for every platform and for a mixed fleet; the reload is right everywhere
  without the operator choosing, and keeps Telegraf's state across a configuration change where
  signals exist.
- Positive: the artifact travels exactly as upstream published it, verified against upstream's own
  digest.
- Negative: an agent packed differently does not fit; the answer is the packing tool, not a key.
  An extra flag needs a code change or the agent's own configuration.
- Negative: one more plugin to keep, with a document and two tests.
- Negative: a fetch trusts GitHub's release notes and InfluxData's CDN together, and is refused
  for a release whose notes carry no SHA-256 table. It costs one more request against GitHub's
  unauthenticated rate limit per run.
- Positive: from 1.34.4 on an asset is held to InfluxData's own key as well, so neither GitHub
  nor the CDN, nor both together, can swap it unnoticed.
- Negative: the tool carries InfluxData's key and has to follow it: when InfluxData signs with a
  subkey the carried keyring lacks, every fetch of the new releases is refused until a release of
  the tool carries the new key. A fetch of 1.34.4 or later needs `gpgv` on the host that runs it,
  which Debian and Ubuntu install with `apt`, and macOS and Windows do not install by default.
- Negative / trade-offs: Telegraf runs under the Client's account
  ([ADR-0037](0037-the-accounts-the-service-runs-under.md)), which a Linux package makes non-root,
  so an input that needs root fails there. The unit sets nothing that implies `no_new_privs`
  (ADR-0037 clause 8), so `use_sudo` with a `sudoers` rule for the account works; `--run-as root`
  is the other way. Where a process account is registered (ADR-0037 clause 18), Telegraf runs
  under that account, which is never root, and the rule names it. On Windows a first registration
  runs the service as its own virtual account `NT SERVICE\<PRODUCT_NAME>`, and a re-registration
  keeps the account the SCM holds, `LocalSystem` included (ADR-0037 clause 1); Telegraf runs under
  that account, and `--run-as` chooses another. The kind has no key for either.

## Enforcement

- `crates/fleet-agent/src/supervisor/telegraf.rs`: `the_block_has_no_settings`,
  `the_recipes_keys_are_refused_by_name`, `the_invocation_points_at_the_delivered_configuration`,
  `the_defaults_are_the_artifacts` (the client side of clause 6), and
  `the_program_is_taken_from_its_place_in_upstreams_archive` (clause 3: the archive's layout, with
  the further entries named `telegraf` ahead of the program).
- `crates/fleet-agent/tests/telegraf_supervisor.rs`:
  `a_two_line_telegraf_block_runs_reports_and_applies` — the two-line block runs, reports its
  version, and applies a Configuration delivered under the name `--config` points at;
  `telegraf_applies_a_configuration_by_sighup_in_place` (Unix) and
  `telegraf_applies_a_configuration_by_restarting_on_windows` (Windows) — clause 2.
- `crates/fleet-tools/src/fetch.rs`:
  `telegraf_urls_carry_upstreams_spelling_and_the_platform_this_fleet_names` (the packing side of
  clause 6), `the_release_notes_digest_is_read_by_the_assets_name`,
  `an_artifact_is_held_to_its_release_notes_and_to_the_digests_beside_it`,
  `digests_that_are_not_published_leave_the_release_notes_to_decide`,
  `the_carried_key_verifies_influxdatas_own_signature` (the carried key and the pinned primary,
  and not the subkey that signed; the verification itself is tested with clause 29 of
  [ADR-draft-the-operator-program-opamp-fleetctl](draft-the-operator-program-opamp-fleetctl.md)),
  `a_missing_signature_refuses_only_a_release_that_signs_every_asset` and
  `a_published_signature_that_does_not_verify_refuses_the_artifact_at_any_version` (clause 3),
  `a_published_checksum_is_read_from_either_shape`,
  `every_agent_carries_a_storable_default_configuration`,
  `a_default_configuration_the_server_already_holds_is_not_written_over`.

- `crates/fleet-agent/src/service/manager.rs`:
  `an_account_unit_sets_nothing_that_implies_no_new_privs` and
  `the_second_services_unit_runs_the_program_under_the_process_account` — neither the Client's unit
  nor the second service's sets `NoNewPrivileges=`, so `use_sudo` keeps working (Consequences).
- `crates/fleet-server/tests/rest_api.rs`:
  `the_ui_colours_an_effective_configuration_by_the_agents_type` — the UI maps `telegraf` to TOML
  (clause 7).

**Not mechanically decidable:** whether upstream still reloads Telegraf on `SIGHUP` is a run-time
property no repository test can see; the artifact document names this row as a warning.
