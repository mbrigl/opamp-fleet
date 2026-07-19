//! The OpAMP server: it accepts agent connections on `/v1/opamp`, answers each agent with the
//! collector configuration it should be running, and pushes that configuration to the whole fleet
//! when it changes.
//!
//! The control loop is a single comparison. Each `AgentToServer` reports the hash of the
//! configuration the agent last received; the server compares it with the hash of the configuration
//! it currently distributes, and includes the remote config in its reply only when they differ. That
//! difference — nothing more — is what tells an agent to reconfigure (ADR-0006).
//!
//! The initial server is plain-`ws`, unauthenticated, and WebSocket-only; TLS + shared-token auth and
//! the OpAMP plain-HTTP transport are deferred to their own ADRs (ADR-0006).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::config::ConfigSource;
use crate::fleet::Fleet;
use crate::frame::{self, MAX_MESSAGE_SIZE};
use crate::proto::{
    AgentCapabilities, AgentRemoteConfig, AgentToServer, CommandType, ConnectionSettingsOffers,
    Header, Headers, OpAmpConnectionSettings, RemoteConfigStatuses, ServerCapabilities,
    ServerToAgent, ServerToAgentCommand, ServerToAgentFlags, TelemetryConnectionSettings,
};

/// The URL path OpAMP agents connect to. The supervisor's `server.endpoint` must end in it
/// (`ws://dev:4320/v1/opamp`).
pub const LISTEN_PATH: &str = "/v1/opamp";

/// The capabilities this server advertises: it accepts status reports, offers remote configuration and
/// connection settings (own-telemetry / heartbeat offers, ADR-0011), and accepts the effective config
/// agents report back. The specification requires `AcceptsStatus` to be set, and it must appear in the
/// first `ServerToAgent`. Package offers are deferred (ADR-0006).
const SERVER_CAPABILITIES: u64 = ServerCapabilities::AcceptsStatus as u64
    | ServerCapabilities::OffersRemoteConfig as u64
    | ServerCapabilities::AcceptsEffectiveConfig as u64
    | ServerCapabilities::OffersConnectionSettings as u64;

/// A message fanned out to the connections: a new configuration for the whole fleet, or a **restart
/// command** targeted at one agent by its instance UID (ADR-0011). Every connection receives it; the
/// restart is applied only by the connection whose agent UID matches.
#[derive(Clone)]
pub enum FleetPush {
    Config(Arc<AgentRemoteConfig>),
    Restart(Vec<u8>),
}

/// The control offers the Server makes to agents *beyond* config distribution (ADR-0011), from the
/// Server's own configuration: a heartbeat interval and an own-telemetry destination.
#[derive(Clone, Default)]
pub struct ServerOffers {
    /// The heartbeat interval to ask agents to use, in seconds; `0` offers none.
    pub heartbeat_interval_seconds: u64,
    /// The destination the Server offers for agents' own telemetry, or `None` to offer none.
    pub own_telemetry: Option<TelemetryOffer>,
}

/// An OTLP/HTTP destination the Server offers for an agent's own telemetry (ADR-0011).
#[derive(Clone)]
pub struct TelemetryOffer {
    pub endpoint: String,
    pub headers: Vec<(String, String)>,
}

impl ServerOffers {
    fn is_empty(&self) -> bool {
        self.heartbeat_interval_seconds == 0 && self.own_telemetry.is_none()
    }
}

/// Everything a connection handler needs, shared behind an `Arc`.
pub struct AppState {
    pub config: Arc<ConfigSource>,
    pub fleet: Arc<Fleet>,
    /// Fleet-wide pushes fan out to every connection through this channel; each handler holds a
    /// subscription. The payload is a new configuration to distribute or a targeted restart (ADR-0011).
    pub pushes: broadcast::Sender<FleetPush>,
    /// The control offers the Server makes to agents beyond config distribution (ADR-0011).
    pub offers: ServerOffers,
    /// Hands out a unique id to each accepted connection.
    next_conn_id: AtomicU64,
}

impl AppState {
    pub fn new(
        config: Arc<ConfigSource>,
        fleet: Arc<Fleet>,
        pushes: broadcast::Sender<FleetPush>,
        offers: ServerOffers,
    ) -> Self {
        Self {
            config,
            fleet,
            pushes,
            offers,
            next_conn_id: AtomicU64::new(0),
        }
    }
}

/// The OpAMP endpoint router: the WebSocket route at [`LISTEN_PATH`].
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route(LISTEN_PATH, get(upgrade))
        // Bound the request body to the same size cap as a WebSocket frame.
        .layer(DefaultBodyLimit::max(MAX_MESSAGE_SIZE))
        .with_state(state)
}

/// Accepts the WebSocket upgrade and hands the socket to the per-connection loop. The message-size
/// cap is enforced at the transport so an oversized frame is rejected before it reaches us.
async fn upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    ws.max_message_size(MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| serve_connection(socket, state))
}

/// Serves one agent for the life of its connection: it folds each incoming report into the fleet and
/// replies, and forwards fleet-wide configuration pushes to this agent.
async fn serve_connection(mut socket: WebSocket, state: Arc<AppState>) {
    let id = state.next_conn_id.fetch_add(1, Ordering::Relaxed);
    state.fleet.connect(id);
    let mut pushes = state.pushes.subscribe();

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        if !handle_report(&mut socket, &state, id, &data).await {
                            break;
                        }
                    }
                    // Text is not a valid OpAMP frame; ignore it rather than tear down the connection.
                    Some(Ok(Message::Text(_))) => warn!(conn = id, "ignoring unexpected text frame"),
                    // Ping/Pong are handled by the transport; nothing for us to do.
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        debug!(conn = id, error = %e, "connection error");
                        break;
                    }
                }
            }
            push = pushes.recv() => {
                match push {
                    Ok(FleetPush::Config(cfg)) => {
                        if !forward_push(&mut socket, &state, id, &cfg).await {
                            break;
                        }
                    }
                    // A restart is addressed to one agent by UID; only the matching connection sends it.
                    Ok(FleetPush::Restart(uid)) => {
                        if state.fleet.uid_of(id).as_deref() == Some(uid.as_slice()) {
                            if !send_restart(&mut socket, &uid).await {
                                break;
                            }
                            info!(agent = %short(&uid), "sent restart command to agent");
                        }
                    }
                    // The connection fell behind the push channel. Nothing to send now: the next
                    // report from this agent carries its config hash, and the comparison re-sends the
                    // current configuration if it is still out of date. A missed restart is not retried
                    // (a restart is a one-shot operator action, not fleet state to reconcile).
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(conn = id, missed = n, "connection lagged fleet pushes");
                    }
                    // The sender was dropped: the server is shutting down.
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    if let Some(uid) = state.fleet.disconnect(id) {
        info!(agent = %uid, "agent disconnected");
    }
}

/// Decodes one WebSocket report and sends the reply. Returns `false` if the connection should be torn
/// down. A malformed frame is the agent's problem, not a reason to drop a connection that may recover.
async fn handle_report(socket: &mut WebSocket, state: &AppState, id: u64, data: &[u8]) -> bool {
    let msg: AgentToServer = match frame::decode(data) {
        Ok(msg) => msg,
        Err(e) => {
            warn!(conn = id, error = %e, "cannot decode agent message");
            return true;
        }
    };
    let resp = build_reply(state, id, &msg);
    send(socket, &resp).await
}

/// Folds one report into the fleet and builds the reply — the heart of the control loop. It answers
/// with a configuration exactly when the agent's reported hash differs from what the server
/// distributes.
fn build_reply(state: &AppState, id: u64, msg: &AgentToServer) -> ServerToAgent {
    let folded = state.fleet.fold(id, msg);
    log_report(id, msg);

    let mut resp = ServerToAgent {
        instance_uid: msg.instance_uid.clone(),
        capabilities: SERVER_CAPABILITIES,
        ..Default::default()
    };

    if folded.report_full_state {
        resp.flags |= ServerToAgentFlags::ReportFullState as u64;
        info!(agent = %short(&msg.instance_uid), "sequence gap detected, requesting full state");
    }

    if let Some(cfg) = state.config.current() {
        if folded.config_hash != cfg.config_hash {
            info!(agent = %short(&msg.instance_uid), hash = %short(&cfg.config_hash), "sending configuration to agent");
            resp.remote_config = Some(cfg);
        }
    }

    // Offer the configured connection settings — a heartbeat interval, an own-telemetry destination — to
    // an agent that declares it accepts them (ADR-0011). The agent de-duplicates, so re-offering the same
    // settings on later reports is harmless.
    resp.connection_settings = connection_settings(&state.offers, msg.capabilities);

    resp
}

/// Builds the connection-settings offer for one agent from the Server's configured offers, including
/// only what the agent's declared capabilities say it accepts (ADR-0011). `None` when there is nothing
/// to offer.
fn connection_settings(
    offers: &ServerOffers,
    agent_capabilities: u64,
) -> Option<ConnectionSettingsOffers> {
    if offers.is_empty() {
        return None;
    }
    let declares = |cap: AgentCapabilities| agent_capabilities & cap as u64 != 0;
    let mut cs = ConnectionSettingsOffers::default();
    let mut any = false;

    // The heartbeat interval only means anything to an agent that reports heartbeats.
    if offers.heartbeat_interval_seconds > 0 && declares(AgentCapabilities::ReportsHeartbeat) {
        cs.opamp = Some(OpAmpConnectionSettings {
            heartbeat_interval_seconds: offers.heartbeat_interval_seconds,
            ..Default::default()
        });
        any = true;
    }

    // Own-telemetry is offered per signal, only for the signals the agent reports it will ship.
    if let Some(offer) = &offers.own_telemetry {
        if declares(AgentCapabilities::ReportsOwnMetrics) {
            cs.own_metrics = Some(telemetry_settings(offer));
            any = true;
        }
        if declares(AgentCapabilities::ReportsOwnLogs) {
            cs.own_logs = Some(telemetry_settings(offer));
            any = true;
        }
        if declares(AgentCapabilities::ReportsOwnTraces) {
            cs.own_traces = Some(telemetry_settings(offer));
            any = true;
        }
    }

    any.then_some(cs)
}

/// One signal's `TelemetryConnectionSettings` from a configured own-telemetry offer (ADR-0011).
fn telemetry_settings(offer: &TelemetryOffer) -> TelemetryConnectionSettings {
    TelemetryConnectionSettings {
        destination_endpoint: offer.endpoint.clone(),
        headers: (!offer.headers.is_empty()).then(|| Headers {
            headers: offer
                .headers
                .iter()
                .map(|(key, value)| Header {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
        }),
        ..Default::default()
    }
}

/// Sends a restart command to one agent (ADR-0011). Per the specification a command message carries only
/// the instance UID and the command itself, so no other fields are set. Returns `false` if the socket is
/// gone.
async fn send_restart(socket: &mut WebSocket, uid: &[u8]) -> bool {
    let msg = ServerToAgent {
        instance_uid: uid.to_vec(),
        command: Some(ServerToAgentCommand {
            r#type: CommandType::Restart as i32,
        }),
        ..Default::default()
    };
    send(socket, &msg).await
}

/// Forwards a fleet-wide configuration push to this connection's agent. Returns `false` if the
/// connection should be torn down.
async fn forward_push(
    socket: &mut WebSocket,
    state: &AppState,
    id: u64,
    cfg: &Arc<AgentRemoteConfig>,
) -> bool {
    // Address the push to the agent behind this connection. Before its first report we do not know
    // its instance UID; skip the push, since its first report will reconcile the hash anyway.
    let Some(uid) = state.fleet.uid_of(id) else {
        return true;
    };
    let resp = ServerToAgent {
        instance_uid: uid,
        capabilities: SERVER_CAPABILITIES,
        remote_config: Some((**cfg).clone()),
        ..Default::default()
    };
    send(socket, &resp).await
}

/// Sends one framed `ServerToAgent`. Returns `false` if the socket is gone.
async fn send(socket: &mut WebSocket, msg: &ServerToAgent) -> bool {
    socket
        .send(Message::Binary(frame::encode(msg).into()))
        .await
        .is_ok()
}

/// Surfaces what an agent reports about itself. This is the only view a developer has of the
/// collector's state, because the collector runs in the sidecar and its logs are not visible from
/// inside the Dev Container (ADR-0003).
fn log_report(id: u64, msg: &AgentToServer) {
    let agent = short(&msg.instance_uid);
    if let Some(h) = &msg.health {
        info!(conn = id, agent = %agent, healthy = h.healthy, status = %h.status, "agent health");
    }
    if let Some(st) = &msg.remote_config_status {
        info!(
            conn = id,
            agent = %agent,
            status = st.status,
            hash = %short(&st.last_remote_config_hash),
            "agent config status"
        );
        if st.status == RemoteConfigStatuses::Failed as i32 {
            error!(agent = %agent, error = %st.error_message, "agent rejected the configuration");
        }
    }
}

/// Renders a hash or instance UID for humans; the full value is noise in a log line.
fn short(bytes: &[u8]) -> String {
    const N: usize = 6;
    hex::encode(&bytes[..bytes.len().min(N)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offers() -> ServerOffers {
        ServerOffers {
            heartbeat_interval_seconds: 30,
            own_telemetry: Some(TelemetryOffer {
                endpoint: "https://otlp.example/v1".to_string(),
                headers: vec![("Authorization".to_string(), "Bearer x".to_string())],
            }),
        }
    }

    #[test]
    fn no_connection_settings_without_offers() {
        assert!(connection_settings(&ServerOffers::default(), u64::MAX).is_none());
    }

    #[test]
    fn offers_only_what_the_agent_declares() {
        // Reports heartbeats and own metrics only: gets the heartbeat and own_metrics, not logs/traces.
        let caps = AgentCapabilities::ReportsHeartbeat as u64
            | AgentCapabilities::ReportsOwnMetrics as u64;
        let cs = connection_settings(&offers(), caps).expect("something is offered");
        assert_eq!(cs.opamp.unwrap().heartbeat_interval_seconds, 30);
        let metrics = cs.own_metrics.expect("metrics offered");
        assert_eq!(metrics.destination_endpoint, "https://otlp.example/v1");
        assert_eq!(metrics.headers.unwrap().headers[0].key, "Authorization");
        assert!(cs.own_logs.is_none());
        assert!(cs.own_traces.is_none());
    }

    #[test]
    fn no_heartbeat_offer_for_a_non_heartbeat_agent() {
        // Own-telemetry-only agent: no opamp heartbeat offered (it does not report heartbeats).
        let cs = connection_settings(&offers(), AgentCapabilities::ReportsOwnTraces as u64)
            .expect("traces offered");
        assert!(cs.opamp.is_none());
        assert!(cs.own_traces.is_some());
    }

    #[test]
    fn nothing_offered_when_the_agent_declares_nothing_relevant() {
        // Declares only ReportsStatus: heartbeat and own-telemetry are both gated out.
        assert!(connection_settings(&offers(), AgentCapabilities::ReportsStatus as u64).is_none());
    }
}
