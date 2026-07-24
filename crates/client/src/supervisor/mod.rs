//! The supervision domain (ADR-0017): builds the Agents the [`Engine`](crate::engine) carries.
//!

pub mod agent;
pub mod collector;
pub mod command;
pub mod endpoint;
pub mod ports;
pub mod process;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;


use crate::storage::Storage;

use agent::AgentState;

/// The compiled-in plugin registry (ADR-0017). A new process kind is a new module and one line
/// here — the supervision core stays untouched (goal 8).
fn registry() -> Vec<Box<dyn Plugin>> {
    vec![
        Box::new(collector::CollectorPlugin),
        Box::new(command::CommandPlugin),
    ]
}

/// The kinds this Client was compiled with, as attributes of its own Agent (ADR-0017 clause 18).
///
/// Wrapping created a fact the fleet did not have to know before: a `type` is something a Client
/// either carries or does not, and a Server rolling a `glpi` set at a Client too old to have that
/// plugin used to learn it from a `FAILED` afterwards rather than by not aiming there.
///
/// **One key per kind**, not one list, because of how matching works here: a Selector is equality
/// over string values (`configs.rs::matches`), so a list could only be matched by spelling the
/// whole list — and the question one wants to ask is about *one member* of it. `AvailableComponents`
/// is the Baseline's own home for this and is deliberately not used yet: it is marked *Development*
/// in the schema this project pins, and Selectors resolve over the description, so reporting kinds
/// there would tell the fleet something it could not act on.
///
/// An operator's own attribute of the same name is left alone — configured values are the host's
/// statement about itself, and this function only fills in what nothing else said.
fn kind_attributes(mut attributes: BTreeMap<String, String>) -> BTreeMap<String, String> {
    for plugin in registry() {
        attributes
            .entry(format!("supervisor.kind.{}", plugin.kind()))
            .or_insert_with(|| "true".to_string());
    }
    attributes
}

/// Build the Engine from the configuration, starting one adapter task per Supervisor.
///
/// # Errors
/// Returns an error when an Agent's state cannot be restored, a `[[supervisor]]` block names an
/// unknown plugin, or a plugin rejects its settings — startup fails loudly, nothing runs half.
pub fn build_engine(config: &ClientConfig, shutdown: &Shutdown) -> Result<Engine, String> {
    let (event_tx, events) = mpsc::channel(64);
            .with_attributes(kind_attributes(config.agent_attributes(None)))
    }
                    opamp::proto::AgentConfigObject {

    // The three the core resolves, checked here so a Server-delivered set carrying one is refused
    // before a running process is touched (ADR-0017), exactly as a bad plugin setting is.
    effective_service_name(block, plugin, &program.path)?;
    check_endpoint_port(block, plugin)?;
    effective_timing(config, block, plugin)?;
/// Pinning the Supervisor Endpoint's port is a decision only where something connects to it
/// (ADR-0017).
///
/// The Endpoint itself is bound for every Supervisor and stays that way (ADR-0014) — what is
/// refused is *naming* its port for a kind whose Managed Process speaks no OpAMP, where the value
/// would read as configuration and do nothing. `0` is not refused: it is the default written out,
/// and refusing a no-op teaches nobody anything.
fn check_endpoint_port(block: &SupervisorBlock, plugin: &dyn Plugin) -> Result<(), String> {
    if block.endpoint_port != 0 && !plugin.defaults().endpoint_port {
        return Err(format!(
            "supervisor {:?}: `endpoint_port` says nothing for type {:?} — the Supervisor Endpoint \
             is bound for every Supervisor, but only a Managed Process that speaks OpAMP connects \
             to one, and this kind's does not; remove the line",
            block.name,
            plugin.kind()
        ));
    }
    Ok(())
}

    let install = match effective_program_path(block, plugin)? {
            program_path,
    let service_name = effective_service_name(block, plugin, &program.path)?;
    check_endpoint_port(block, plugin)?;
    let timing = effective_timing(config, block, plugin)?;
        stop_timeout: timing.stop_timeout,
        apply_grace: timing.apply_grace,
        retain_previous: timing.retain_previous,
    let named = settings.remove(key);
    let program_name = match (named, plugin.defaults().program) {
        // A wrapped kind knows its program, so writing it is naming a value this Client computes
        // (ADR-0017 clause 13) — refused with what supplies it now, never quietly overridden.
        (Some(_), Some(derived)) => {
            return Err(format!(
                "supervisor {:?}: `{key}` is no longer a supervisor key for type {:?} — the kind \
                 installs and names its own program ({derived}); remove the line",
                block.name,
                plugin.kind()
            ))
        }
        (Some(raw), None) => raw
            .as_str()
            .ok_or_else(|| {
                format!(
                    "supervisor {:?}: `{key}` must be a path, not {}",
                    block.name,
                    raw.type_str()
                )
            })?
            .to_string(),
        (None, Some(derived)) => derived.to_string(),
        (None, None) => return Err(format!("supervisor {:?}: needs a `{key}`", block.name)),
    };
        std::path::Path::new(&program_name),
        effective_program_path(block, plugin)?.as_deref(),
}
/// Where the program sits inside a package tree (ADR-0028): the block's answer, or the one the kind
/// knows (ADR-0017). A kind that knows it refuses a block that states it, for the reason
/// [`take_program`] refuses a program name.
///
/// # Errors
/// Returns an error when a block states a `program_path` its kind already supplies.
fn effective_program_path(
) -> Result<Option<PathBuf>, String> {
    match (&block.program_path, plugin.defaults().program_path) {
        (Some(_), Some(derived)) => Err(format!(
            "supervisor {:?}: `program_path` is no longer a supervisor key for type {:?} — the \
             kind knows where its program sits in the tree it delivers ({derived}); remove the line",
            block.name,
            plugin.kind()
        )),
        (Some(stated), None) => Ok(Some(stated.clone())),
        (None, derived) => Ok(derived.map(PathBuf::from)),
    }
}

/// The Agent type this Supervisor presents until — and unless — its Managed Process reports one of
/// its own (ADR-0015): the block's, the kind's, else the program's file name.
///
/// The file-name fallback is what the operator already wrote in this very block; it is read from
/// configuration and never parsed out of a program's output, where a name has no grammar to
/// recognise it by. A kind that states its type refuses a block that restates it (ADR-0017).
/// What this Supervisor's three timings are, and whether its block was allowed to say anything
/// about them (ADR-0017).
///
/// Three layers, outermost first: the fleet's policy in `[supervisors]` and `[updates]`, a wrapped
/// kind's correction of it, and — only where no kind exists to hold the value — the block. A block
/// of a wrapped kind naming one is refused with what supplies it now, on the same terms as every
/// other retired key, so an offered Supervisor set is refused before a process is touched.
fn effective_timing(
    plugin: &dyn Plugin,
) -> Result<Timing, String> {
    let fleet = Timing {
        stop_timeout: Duration::from_secs(config.supervisor_defaults.stop_timeout_secs),
        apply_grace: Duration::from_secs(config.supervisor_defaults.apply_grace_secs),
        retain_previous: Duration::from_secs(config.updates.retain_previous_secs),
    };
    let stated = [
        ("stop_timeout_secs", block.stop_timeout_secs),
        ("apply_grace_secs", block.apply_grace_secs),
        ("retain_previous_secs", block.retain_previous_secs),
    ];
    let Some(kind) = plugin.defaults().timing else {
        // An unwrapped kind: nothing here knows the agent, so the block still answers.
        return Ok(Timing {
            stop_timeout: block
                .stop_timeout_secs
                .map_or(fleet.stop_timeout, Duration::from_secs),
            apply_grace: block
                .apply_grace_secs
                .map_or(fleet.apply_grace, Duration::from_secs),
            retain_previous: block
                .retain_previous_secs
                .map_or(fleet.retain_previous, Duration::from_secs),
        });
    };
    for (key, value) in stated {
        if value.is_some() {
            return Err(format!(
                "supervisor {:?}: `{key}` is no longer a supervisor key for type {:?} — how long \
                 an agent needs is a property of that agent, which the kind states, over the \
                 fleet's own `[supervisors]`/`[updates]` policy; remove the line",
                block.name,
                plugin.kind()
            ));
        }
    }
    Ok(Timing {
        stop_timeout: kind.stop_timeout.unwrap_or(fleet.stop_timeout),
        apply_grace: kind.apply_grace.unwrap_or(fleet.apply_grace),
        retain_previous: kind.retain_previous.unwrap_or(fleet.retain_previous),
    })
}

/// The three resolved timings of one Supervisor.
struct Timing {
    stop_timeout: Duration,
    apply_grace: Duration,
    retain_previous: Duration,
}

fn effective_service_name(
    program: &std::path::Path,
) -> Result<String, String> {
    match (&block.service_name, plugin.defaults().service_name) {
        (Some(_), Some(derived)) => Err(format!(
            "supervisor {:?}: `service_name` is no longer a supervisor key for type {:?} — the \
             kind states the Agent type it presents ({derived}); remove the line",
            block.name,
            plugin.kind()
        )),
        (Some(stated), None) => Ok(stated.clone()),
        (None, Some(derived)) => Ok(derived.to_string()),
        (None, None) => Ok(program
            .into_owned()),
    }
}

mod tests {
    use super::*;
    use crate::service::runtime::shutdown_channel;
        let block = &config.supervisors[0];
        let plugins = registry();
        let plugin = find_plugin(&plugins, block).expect("the kind is known");
        assert_eq!(
            effective_service_name(block, plugin, &program.path).expect("a type"),
    /// A Client says which kinds it carries, one key per kind (ADR-0017 clause 18), so a Selector
    /// can aim a Supervisor set at the Clients that can actually run it — rather than the Server
    /// learning from a `FAILED` that it aimed at a Client too old to have the plugin.
    #[test]
    fn a_client_reports_the_kinds_it_was_compiled_with() {
        let reported = kind_attributes(BTreeMap::new());
        for plugin in registry() {
            assert_eq!(
                reported
                    .get(&format!("supervisor.kind.{}", plugin.kind()))
                    .map(String::as_str),
                Some("true"),
                "{} is compiled in and unreported",
                plugin.kind()
            );
        }
        // An operator's own value under the same key is left alone: a configured attribute is the
        // host's statement about itself, and this only fills in what nothing else said.
        let stated = kind_attributes(
                .into_iter()
                .collect(),
        );
        assert_eq!(
            Some("no")
        );
    }

        let block = &config.supervisors[0];
        let timing = effective_timing(&config, block, find_plugin(&plugins, block).expect("kind"))
            .expect("resolved");
    /// And where a kind states no correction, the fleet's policy reaches the Supervisor unchanged
    /// — including for an unwrapped kind, whose block may still override it because no kind exists
    /// there to hold the value.
    #[test]
    fn the_fleets_timing_reaches_a_supervisor_that_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plugins = registry();
        let mut config: ClientConfig =
        config.supervisor_defaults.stop_timeout_secs = 45;
        config.supervisor_defaults.apply_grace_secs = 7;
        let block = &config.supervisors[0];
        let timing = effective_timing(&config, block, find_plugin(&plugins, block).expect("kind"))
            .expect("resolved");
        assert_eq!(timing.stop_timeout, Duration::from_secs(45));
        assert_eq!(timing.apply_grace, Duration::from_secs(7));

        let stated: ClientConfig = toml::from_str(&format!(
             [supervisors]\nstop_timeout_secs = 45\n\
             [[supervisor]]\ntype = \"command\"\nname = \"agent\"\ncommand = \"x\"\n\
             stop_timeout_secs = 90\n",
        let block = &stated.supervisors[0];
        let timing = effective_timing(&stated, block, find_plugin(&plugins, block).expect("kind"))
            .expect("resolved");
        assert_eq!(
            timing.stop_timeout,
            Duration::from_secs(90),
            "an unwrapped kind's block still answers"
        );
    }

    /// The unwrapped kinds are untouched: `command` knows nothing, so its block still says
    /// everything — and a Collector may still pin the port something actually connects to.
    #[test]
    fn an_unwrapped_kind_still_says_everything_itself() {
        let block = &config.supervisors[0];
        let plugins = registry();
        let plugin = find_plugin(&plugins, block).expect("the kind is known");
        let (_, program) = take_program(&config, block, plugin).expect("the block names it");
        assert!(program.path.ends_with("managed-agent"));
        assert_eq!(
            effective_service_name(block, plugin, &program.path).expect("a type"),
            "managed-agent",
            "the program's file name is what the operator already wrote"
        );

        let collector: ClientConfig = toml::from_str(&format!(
             [[supervisor]]\ntype = \"collector\"\nname = \"otelcol\"\n\
             binary = \"otelcol\"\nendpoint_port = 4321\n",
        assert!(
            check_endpoint_port(
                &collector.supervisors[0],
                find_plugin(&plugins, &collector.supervisors[0]).expect("kind")
            )
            .is_ok(),
            "the opampextension connects to it, so pinning it is a decision"
        );
    }

