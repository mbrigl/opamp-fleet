# ADR-0019: A Supervisor downloads, verifies, unpacks, swaps and health-gates a package, and rolls back only to a predecessor

- **Status:** 🟢 accepted
- **Date:** 2026-08-13
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/packages.rs, crates/fleet-agent/src/archive.rs, crates/fleet-agent/src/install.rs, crates/fleet-agent/src/supervisor/process.rs, the package handling in crates/fleet-agent/src/supervisor/agent.rs, the `[packages]` and `[updates]` sections and the `program_path` and `retain_previous_secs` keys of `supervisor.toml`

## Context

The Server updates the program a Managed Process runs: it verifies each Package before it is
applied, reports the outcome, and rolls back on failure; a failed update is reported, not silent.
The Baseline's mechanism is a hash-gated sync. The Agent reports
`PackageStatuses.server_provided_all_packages_hash`; on a mismatch the Server sends
`PackagesAvailable` — per package a type, a version, a per-package `hash` and a
`DownloadableFile{download_url, content_hash, signature, headers}` — and the Agent downloads,
verifies, installs and reports each package through `Downloading → Installing → Installed |
InstallFailed`.

What the Server stores, how a Package is identified and how a Deployment aims it and carries its
signature are [ADR-0020](0020-the-package-store.md) and
[ADR-0030](0030-packages-and-deployments.md); when an Agent is offered one is
[ADR-0027](0027-rollout-and-what-reaches-an-agent.md). This record is the Agent's half and the
contract an artifact has to meet.

The forces:

- **A running process cannot reliably replace its own binary**, so the work crosses a process
  boundary (the specification's *Updater*). For a Managed Process that boundary already exists:
  the Supervisor owns its process's stop, spawn and apply-grace health gate
  ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)). The Client's own binary is the other case
  ([ADR-0021](0021-the-client-updates-itself.md)).
- **Verification, not transport, protects the host.** An artifact URL may point anywhere — at the
  Server's download route or at a mirror or release page — so what stands between the bytes and an
  executed program is the content hash and, where the operator holds a key, an Ed25519 signature
  (the Baseline's *Code Signing* section leaves the method to the Agent). The `ring` provider the
  build already carries ([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)) verifies
  Ed25519.
- **An upstream release is an archive.** `opentelemetry-collector-releases` publishes `.tar.gz`,
  `.deb`, `.rpm` and `.msi`, never a bare binary, with a `checksums.txt` of SHA-256 values and
  sigstore keyless signatures. The GLPI Agent's portable Windows build is a `.zip`
  ([ADR-0028](0028-glpi-agent-and-telegraf.md)). An in-house agent may have to stay confidential
  wherever it is stored. The protocol carries one file per package and leaves multi-file packages
  to "any file format that allows storing multiple files in a single file".
- **Many agents are more than one file.** Fluent Bit ships an executable plus the shared objects
  and plugins it loads, and a static build is not available upstream. A package that unpacks into
  a tree gives the archive a say in where bytes land — a security boundary this decision creates.
- **The rollback lifecycle must end.** Discarding a first install that will not start empties the
  program directory and the Server re-offers it: a download loop. A predecessor that also fails to
  start must not be respawned forever. And "survived the grace" is a first signal, not a final one,
  so the predecessor is worth keeping for a while.

## Decision

We will have each Supervisor take one top-level package offered for its Agent, stream it to its
own directory, verify its SHA-256 and — when a key is configured — its Ed25519 signature, open it
on the host whether it is a bare program or a `.tar.gz`, `.7z` or `.zip` holding one file or a
whole tree, swap it in by rename, health-gate it on the apply grace, roll back only to a real
predecessor, stop restarting after three failed starts, and keep the superseded version for a
configurable window.

1. **The Supervisor is the updater.** Every Supervisor-backed Agent declares `AcceptsPackages`
   and `ReportsPackageStatuses` (every Managed Process is one this Client installed,
   [ADR-0022](0022-a-supervisors-directory-program-and-set.md)). A verified artifact travels to
   the Supervisor as the Port command `ProcessCommand::ApplyPackage { staged, version, hash }`,
   answered by `ProcessEvent::PackageApplied`, beside `ApplyConfig`/`ConfigApplied`. No further
   process is spawned for it.

2. **One top-level package per Agent; anything else is refused and reported.** An Agent takes the
   one top-level package of an offer. An offer carrying only addons, or two top-level packages, is
   refused with a reason in `PackageStatuses.error_message` and nothing is downloaded — the
   Baseline lets any Server offer addons, and a Supervisor's only use for a package is to *be* the
   program, so the filter guards against a non-conforming peer writing an addon over the binary.
   An offer whose per-package hash equals the installed one is acknowledged by echoing the
   aggregate hash; a repeat of the hash already in flight is ignored.

3. **Status follows the Baseline lifecycle, and every outcome ends the offer.** While the bytes
   arrive the status is `Downloading` with `PackageDownloadDetails` (percent from
   `Content-Length`, `0` when unknown, and bytes per second); otherwise a taken offer is
   `Installing`, with `agent_has_version` still the old one; the Supervisor's answer makes it `Installed` or `InstallFailed` with the reason. A failed
   download or verification is `InstallFailed` too. Success *and* failure echo the offered
   `all_packages_hash`, so the Server stops re-offering the same bytes: a refusal is a report, not
   a loop. The installed package (name, version, hash) is persisted in the Agent's state, so a
   restarted Client reports what it runs and is not offered it again.

4. **The download is streamed, bounded and staged inside the Supervisor's own directory.**
   - `download_url` is used as given when it is absolute `http(s)://`; a path is resolved against
     the host of the Client's OpAMP endpoint, `ws`/`wss` mapped to `http`/`https`.
   - The offered package name must be 1–64 characters of letters, digits, `.`, `_`, `+`, `-`,
     checked before anything is resolved or written: the staged file is named after it.
   - The artifact is streamed to `<supervisor_dir>/packages/<name>.staged` and hashed on the way;
     peak memory is one chunk. The staging directory is owner-only (`0700` on Unix), so no other
     local user can swap the file between verification and install. A failure leaves no partial
     file.
   - `max_artifact_size_bytes` (top level of `supervisor.toml`, default `1073741824`, `0` fails
     startup) bounds the download: a `Content-Length` above it is refused before a byte is written,
     and a chunked body is cut the moment it crosses it.
   - Connect timeout 30 s and read timeout 60 s, never a total timeout — a large artifact over a
     slow link legitimately takes minutes.
   - Redirects are followed up to 5. The offer's `headers` are sent on the `GET`, and re-sent along
     a redirect only while scheme, host and port stay the ones they were given for; an invalid
     header fails the download naming its key only. Header values never reach a log or a debug
     print, and the logged source URL drops its query, fragment and credentials.
   - The download uses the Client's TLS trust but never its client certificate: an identity
     belongs to the Server, not to whoever hosts an artifact.

5. **Verification decides, and the signature policy is the operator's.** The SHA-256 of the
   streamed bytes must equal `content_hash`, always. The signature then follows
   `[packages] verification_key` (hex-encoded Ed25519 public key, decoded at startup; a malformed
   key fails startup):

   | `verification_key` | artifact signed | artifact unsigned |
   |---|---|---|
   | set | verified; invalid → refused | refused |
   | unset | refused (nothing to check it with) | accepted on its content hash |

   Unsigned operation is a legitimate posture, so it is allowed — and stated: a Client with no key
   whose Agents take packages logs a warning at startup. The signature covers the artifact exactly
   as published, archive and all. Where a Deployment keeps the signature is
   [ADR-0030](0030-packages-and-deployments.md). The same download and verification serve the
   Client's own update ([ADR-0021](0021-the-client-updates-itself.md)).

6. **The Agent opens the artifact; nothing between its author and the host repacks it.** The hash
   an Agent verifies is therefore the SHA-256 the artifact was published with — integrity runs in
   one line from the release page to the running program. What an artifact is, is decided by its
   leading bytes, never by a name: `1f 8b` is a `.tar.gz`; `37 7a bc af 27 1c` is a `.7z`;
   `PK\x03\x04` or `PK\x05\x06` is a `.zip`; anything else is the program itself. These three
   containers are the whole set the Client opens, for every kind and for its own update alike.
   A `.deb`, `.rpm` or `.msi` is not opened.

7. **A `.7z` may be encrypted, and the key lives only on the Agent.** `[packages] archive_key`
   opens an encrypted `.7z` (AES-256); the Server never learns it, so a confidential artifact is
   readable only on the host that runs it. It is one secret for the fleet — a single archive serves
   every Agent — and must never be the `[auth]` credential, which the Server rotates on its own
   ([ADR-0018](0018-connection-settings-and-server-capabilities.md)) and would leave every archive
   unopenable. It is masked in the configuration the Client reports. An encrypted archive without
   the key, or with the wrong one, fails naming the key. A `.zip` is never encrypted: an encrypted
   zip member refuses the archive and names `.7z` as the way.

8. **A single-file package installs one member, to a place the Client chose.** Without
   `program_path`, the artifact is the program or an archive holding it: the member whose *file
   name* equals the configured program's is extracted, wherever the archive keeps it, and nothing
   else. The archive never chooses a path, so a member named `../../etc/cron.d/x` lands where the
   Client put it like any other. A missing member fails, naming up to eight members the archive
   does hold. Output is bounded at 2 GiB, and the decompression needed to *reach* the member is
   bounded by the same budget from the declared sizes before anything inflates, so a bomb ahead
   of the target is refused rather than skipped through.

9. **A `[[supervisor]]` block's `program_path` makes the package a directory tree.** It is a
   relative path *inside* the package, e.g. `bin/fluent-bit`; absent, the package is one file. It
   is validated at startup — non-empty, relative, no `.` or `..` — and it says *where inside*,
   never *whether*: the program's bare file name stays the consent
   ([ADR-0022](0022-a-supervisors-directory-program-and-set.md)). Being written in the
   configuration, the spawn path `program/tree/<program_path>` is known before any package exists.
   - `program_path` matches a member by its **trailing path components**, so `bin/fluent-bit`
     finds `fluent-bit-3.1.0/bin/fluent-bit` and stays right at the next release. No match fails
     naming what the archive holds; several matches fail naming them, answered by writing more of
     the path.
   - The directory prefix above the match is stripped, and every member below it is extracted
     keeping its relative path. Members outside that prefix are not unpacked; they are counted in
     the install line and named at `debug`.
   - A bare program delivered to a tree Supervisor is placed at `program_path`.

10. **A tree's paths are sanitized and its size is bounded, or nothing is written.** Every member
    is checked before the first byte lands. An absolute path, a root or drive prefix, a `..`
    component, a symbolic or hard link, or a 7z anti-item refuses the **whole** archive — a
    partially unpacked agent is worse than none. Extraction is bounded at 2 GiB across all members
    and at 10 000 members, from declared sizes before decompression and again on what is written.

11. **Modes come from a `.tar.gz` on Unix; the program is always made executable.** A tar's member
    modes are applied. A `.7z` or `.zip` carries Windows attributes, so no mode is taken from it.
    The program — the single file or the member at `program_path` — is set `0755` regardless, so
    whether it can run never depends on how the archive was built; an agent with further
    executables beside its program ships as `.tar.gz`.

12. **The swap is a rename, and the apply grace gates it.** On disk, per Supervisor:

    | Package | Live | Predecessor | Staged |
    |---|---|---|---|
    | single file | `program/<file>` | `program/<file>.rollback` | `program/<file>.staged` |
    | tree | `program/tree/` | `program/tree.rollback/` | `program/.staging/` |

    The artifact is unpacked to the staged name first, beside what runs; a raw artifact is moved
    there rather than copied when it can be. A kind that declares a preflight proves the staged
    program runs before anything is stopped ([ADR-0029](0029-icinga-2.md)). Then the Managed
    Process is stopped, the live name renamed to the predecessor, the staged name renamed to the
    live one — so the live name is always the old package or the new one, never a mixture — and
    the process is spawned and must survive `apply_grace_secs`
    ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)). Surviving is `Installed`, after which the
    program is asked for its version again. A spawn failing with `ETXTBSY` right after the swap is
    retried briefly rather than rolled back. When there is nothing to run yet (no Configuration),
    the package is installed and reported `Installed`; the configuration that arrives starts it.

13. **A failed apply rolls back only to a predecessor.** If the process does not start or exits
    within the grace and a predecessor exists, the predecessor is renamed back over the live name
    and respawned. If there is none — a first install — nothing is rolled back: the verified
    program stays in place and is reported `InstallFailed`. A failure never empties `program/`, so
    the "installed ≠ offered, re-offer, re-download" loop cannot start.

14. **A program that keeps failing to start is held, not looped.** A start counts as failed when
    the apply fails its grace, or the process exits before it has run `max(apply_grace, 10 s)`.
    After **three** failed
    starts in a row the Supervisor stops restarting, reports the Agent unhealthy (`not restarting:
    the program keeps failing to start`) and waits. A new Configuration, a new package or a
    restart command is a fresh chance and resets the count; a run that outlasted the floor resets
    it too. This holds for a failed package, a rolled-back predecessor that will not start either,
    and a Configuration alike. Three is the self-update's give-up
    ([ADR-0021](0021-the-client-updates-itself.md)), so the two update paths behave alike.

15. **The superseded version is kept for a window, then deleted.** After a successful apply the
    predecessor stays, and a marker beside it (`<predecessor>.until`, Unix seconds) records
    `now + retain_previous_secs`. A sweep at startup and every 10 minutes deletes a predecessor
    past its deadline; a marker that will not parse counts as expired, and a predecessor without a
    marker is left alone. `[updates] retain_previous_secs` sets the window (default `86400`, one
    day); a `[[supervisor]]` block's `retain_previous_secs` overrides it for that Supervisor;
    negative values fail startup; `0` deletes the predecessor on success. A Supervisor keeps at
    most one predecessor: the next update replaces it and its marker. The marker lives in the
    Supervisor's directory, so the deadline survives a Client restart.

**Out of scope:** where artifacts are stored and served, upload and source entries, and the
Package's identity ([ADR-0020](0020-the-package-store.md)); signature placement and aiming
([ADR-0030](0030-packages-and-deployments.md)); when an Agent is offered a package
([ADR-0027](0027-rollout-and-what-reaches-an-agent.md)); verifying upstream sigstore signatures;
installing addon packages; resuming an interrupted download with range requests; distributing
the archive key or the verification key; keeping more than one predecessor.

## Alternatives considered

- **A separate Updater process for Managed-Process packages**, symmetric with the Client's own
  update. The Supervisor already is a distinct process that owns stop, swap, spawn and health
  gate; another process would buy nothing.
- **Content hash only, signatures later.** Authenticity is what verifying a binary means, and
  retrofitting it onto an install path is the costly change; Ed25519 through the `ring` already
  present costs little.
- **Unpack on the Server and store a bare binary.** One extraction instead of hundreds, no
  unpacking code on hosts. Rejected: the Server would re-hash its own output, so every Agent would
  verify a number the Server invented instead of the one upstream published, and an encrypted
  artifact would have to be decrypted on the Server — key and plaintext on the very machine the
  encryption keeps them from.
- **Support only `.7z`** (one code path, carries a password). Upstream publishes no `.7z`, so no
  release could be used without repacking by hand. **Only `.tar.gz` with some outer encryption**
  — a format someone would have to invent, where AES-256 `.7z` is understood by every tool.
- **The archive key in `server.toml`, distributed over OpAMP, or carried as a Configuration.** Each
  puts the key where the Server — whose download route anyone who can reach it may call — can open
  every artifact; as a Configuration it would also be echoed back as effective configuration and
  shown in the fleet view.
- **Addon packages for an agent's supplementary files.** Nothing groups a set of packages, so there
  is no consistent version, no ordering and no atomicity: an Agent could sit with a new executable
  and old shared objects.
- **A `strip_components` key, or stripping a lone top directory automatically.** The first is one
  more number to get right by inspecting an artifact; the second flattens an archive whose top
  level is meaningful and changes behaviour with the archive's contents. Suffix matching needs
  nothing written and fails loudly when ambiguous.
- **Find the program by searching the unpacked tree.** It removes "what will this host run" from
  the configuration: before the first package there is nothing to search.
- **A self-extracting single file.** Re-extracts on every update, hides what is installed, makes
  the reported version the wrapper's, and turns rollback into hope.
- **Unpack over the live `program/` in place.** A failed unpack halfway leaves a tree that is
  neither version and destroys the predecessor the health gate rolls back to.
- **A version-named directory and a `current` pointer** for trees. A directory rename is atomic on
  every platform the Client runs on; a pointer is a symlink on Unix and a junction on Windows —
  machinery that would run on every package. Two fixed names are the single-file swap one level
  up; the version is read from the Agent's report, not from disk.
- **Keep every superseded version.** An unbounded version store on hosts chosen for their disk
  budget; a rollback needs one predecessor.
- **Give up after one failed start.** A transient failure — a briefly held port — would strand a
  rollout; three matches the self-update.
- **Retention on the downloaded artifact instead of the installed predecessor.** A rollback restores
  the installed program, not the download; tying retention to the staged file would delete what a
  fallback needs.

## Sources / Prior art

- [OpAMP specification — Packages, Downloadable Packages, Code Signing](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#packages)
  — the hash-gated sync, the status lifecycle, the Download Server that "may be on the same host as
  the OpAMP Server or a different host", one downloadable file per package with multiple files
  stored "in any file format that allows storing multiple files in a single file", and package
  content being "Agent type-specific and … outside the concerns of the OpAMP protocol".
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) `PackagesSyncer` — the reference
  download-verify-report component.
- [Collector Supervisor specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — signs the package hash server-side and verifies before applying.
- [`opentelemetry-collector-releases` v0.157.0](https://github.com/open-telemetry/opentelemetry-collector-releases/releases/tag/v0.157.0)
  — archives only, never a bare binary; `checksums.txt` with a SHA-256 per asset; sigstore keyless
  `.sig`/`.pem` companions.
- [`sevenz-rust2`](https://crates.io/crates/sevenz-rust2) — pure-Rust 7z with an `aes256` feature;
  its default features pull `bzip2`, so it is taken with `default-features = false`.
- [`zip`](https://crates.io/crates/zip) — taken with `default-features = false` and `deflate` only,
  so reading stays on the `flate2`/`miniz_oxide` chain.
- [Elastic Agent standalone install](https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent)
  — the `.tar.gz` tree distribution is the one Fleet can upgrade.
- [Datadog Fleet Automation upgrades](https://docs.datadoghq.com/agent/fleet_automation/upgrade_agents/)
  — two installs side by side "in case a rollback is needed".
- [fluent/fluent-bit#2558](https://github.com/fluent/fluent-bit/pull/2558) — `FLB_STATIC_BINARY`, an
  unmerged draft: why a static Fluent Bit is not the answer.

## Consequences

- Positive: an upstream release, archive and all, is deliverable as published; the SHA-256 from
  its `checksums.txt` is checked on every host, and a confidential agent stays encrypted
  everywhere but the host that runs it.
- Positive: an agent that is an executable plus its libraries is managed like any other — the
  health gate, rollback, retention and version report come with it.
- Positive: neither failure loop can start. A first package that will not run stays installed and
  `InstallFailed`; a pair of broken versions is reported and held; for a day after a successful
  update the previous version is still on disk.
- Negative / trade-offs: every host parses untrusted archives. Path sanitizing, link refusal and
  the size and member bounds are the tested boundary, because a tree archive does choose where
  its bytes land.
- Negative / trade-offs: disk — two copies of a program or tree per Supervisor during the
  retention window, and the artifact ceiling and unpack bound sized for agents of hundreds of
  megabytes.
- Negative / trade-offs: the archive key is a fleet-wide secret in `supervisor.toml` on every
  host, protected only by the file's permissions; rotating it means every host and every encrypted
  archive. Offered download headers are likewise a fleet-wide secret in flight.
- Negative / trade-offs: a `.7z` or `.zip` tree carries no helper executables' modes, and a sloppy
  `.tar.gz` produces an agent that does not start — a cause in the archive, not on the host.
- Negative / trade-offs: two on-disk layouts, chosen by whether one optional key is set.
- Follow-ups: verifying upstream sigstore provenance instead of a pasted checksum; sourcing the
  archive key from an OS keystore or a tighter file; distributing keys without letting the Server
  read artifacts; a directory mode for the packing tool; range-request resumption for large
  artifacts.

## Enforcement

- Download and verification: `content_hash_mismatch_is_refused`,
  `signature_policy_is_enforced`, `a_traversing_package_name_is_refused`,
  `download_refuses_to_stage_a_traversing_name`, `the_cap_triggers_only_past_the_limit`,
  `the_download_source_drops_whatever_authorises_it`,
  `a_download_never_debug_prints_its_header_values` in
  [`packages.rs`](../../crates/fleet-agent/src/packages.rs);
  `a_body_too_large_by_its_content_length_is_refused`,
  `a_chunked_body_is_stopped_once_it_crosses_the_ceiling`,
  `a_download_follows_a_redirect_to_the_mirror`, `a_download_carries_the_headers_the_offer_named`,
  `an_offered_header_does_not_follow_a_redirect_to_another_origin`,
  `an_unusable_offered_header_fails_the_download_by_name`,
  `the_staging_directory_is_kept_owner_only` in
  [`tests/packages_download.rs`](../../crates/fleet-agent/tests/packages_download.rs);
  `the_artifact_size_limit_defaults_is_configurable_and_rejects_zero` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs); end to end,
  `a_signed_package_is_downloaded_verified_swapped_and_reported_installed` in
  [`tests/packages_e2e.rs`](../../crates/fleet-agent/tests/packages_e2e.rs).
- Offer and status: `an_addon_package_is_refused_instead_of_overwriting_the_binary`,
  `a_package_offer_for_the_named_package_is_acknowledged_installing_and_handed_over`,
  `a_package_offer_hands_its_download_headers_to_the_transport`,
  `a_failed_package_reports_installed_failed_and_keeps_the_old_version` in
  [`supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs);
  `every_supervisor_declares_package_acceptance` in
  [`supervisor/mod.rs`](../../crates/fleet-agent/src/supervisor/mod.rs);
  `a_slow_download_is_reported_as_downloading_with_progress` in
  [`transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs).
- Containers, single file and tree: in [`archive.rs`](../../crates/fleet-agent/src/archive.rs) the
  detection tests (`detects_gzip_by_its_leading_bytes_and_anything_else_as_raw`,
  `detects_a_7z_by_its_signature`, `detects_a_zip_by_its_signature_and_an_empty_one_too`),
  `an_encrypted_7z_opens_with_the_key_and_not_without_it`,
  `an_escaping_member_path_still_lands_only_where_we_put_it`,
  `a_bomb_ahead_of_the_target_is_refused_before_it_is_skipped`,
  `the_same_program_path_finds_the_program_under_any_wrapper`,
  `members_outside_the_programs_own_directory_are_left_out_and_counted`,
  `a_member_that_climbs_out_refuses_the_archive_before_writing_anything`,
  `an_absolute_member_refuses_the_archive`, `a_link_member_refuses_the_archive`,
  `a_hard_link_member_refuses_the_archive`,
  `a_7z_member_that_is_a_link_or_an_anti_item_refuses_the_archive`,
  `an_archive_of_too_many_members_is_refused`, `a_tree_that_outgrows_the_total_budget_is_refused`,
  `no_match_and_an_ambiguous_match_are_both_refused_by_name`,
  `a_tree_keeps_the_modes_the_archive_carried`, `a_hostile_zip_member_refuses_the_archive`;
  `a_program_path_must_stay_inside_the_package` and
  `a_tree_spawns_from_the_path_written_inside_the_package` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs).
- Swap, rollback, hold and retention: in
  [`tests/supervisor_process.rs`](../../crates/fleet-agent/tests/supervisor_process.rs)
  `apply_package_swaps_the_binary_and_acknowledges_installed`,
  `a_package_delivered_as_a_tar_gz_is_unpacked_and_installed`,
  `an_install_with_nothing_to_run_yet_keeps_the_binary_and_succeeds`,
  `a_package_that_will_not_stay_up_is_rolled_back_and_fails`,
  `a_first_install_that_will_not_start_is_kept_not_discarded`,
  `a_program_that_keeps_crashing_is_held_not_looped`,
  `a_successful_update_keeps_the_previous_version_for_the_window`,
  `a_tree_package_lands_whole_and_replaces_the_one_before_it`,
  `a_tree_that_will_not_stay_up_is_rolled_back_whole`,
  `a_tree_missing_the_configured_program_is_refused_and_changes_nothing`; in
  [`supervisor/process.rs`](../../crates/fleet-agent/src/supervisor/process.rs)
  `a_retained_backup_is_swept_only_after_its_deadline`,
  `a_sweep_leaves_an_unmarked_backup_alone`, `dropping_a_backup_clears_its_marker`;
  `retention_defaults_globally_and_is_overridable_per_supervisor` in
  [`config.rs`](../../crates/fleet-agent/src/config.rs).
