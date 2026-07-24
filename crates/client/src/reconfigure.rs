use crate::engine::Engine;
use crate::service::runtime::Shutdown;
    // A removed Supervisor is uninstalled (ADR-0015) — its adapter answers before the purge
    // below — while a changed one is only stopped and restarts under its name.
    let mut entries: Vec<(&String, &opamp::proto::AgentConfigObject)> = map.iter().collect();
    use opamp::proto::{AgentConfigMap, AgentConfigObject};
        AgentRemoteConfig {
            config: Some(AgentConfigMap {
                            AgentConfigObject {
            [[supervisor]]
            type = "command"
