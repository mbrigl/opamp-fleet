//! The upstream Connection Pool (ADR-0037): *m* WebSocket connections carrying *n* Agents.
//!
//! Two rules do the work. The pool **grows lazily** to its configured cap — a Gateway in front of
//! three Agents holds three connections, not ten — and an Agent is **stuck to its connection** by
//! `instance_uid` for as long as that connection lives. Nothing in the protocol requires the
//! stickiness; it keeps one Agent's `sequence_num` stream and its `ReportFullState` exchanges on a
//! single socket, which is what makes a fleet debuggable.
//!
//! The pool is WebSocket-only, and the configuration refuses anything else at startup: a polling
//! upstream could not carry the Server's pushes to the Agents behind the Gateway.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};

use crate::config::ClientConfig;
use crate::gateway::registry::Registry;

/// One upstream connection: a sender into its writer task, and who rides it.
struct Upstream {
    outbound: mpsc::Sender<Vec<u8>>,
    /// The Agents assigned to this connection — the count least-connections balances on, and the
    /// set to re-home when it drops.
    carries: Vec<InstanceUid>,
    /// Cleared by the reader task when the socket is gone, so the next send re-homes instead of
    /// writing into a channel nobody drains.
    alive: Arc<AtomicBool>,
}

/// The pool, shared between the downstream connections that feed it.
pub struct Pool {
    inner: Mutex<Inner>,
    config: Arc<ClientConfig>,
    registry: Arc<Registry>,
    limit: usize,
}

struct Inner {
    connections: Vec<Upstream>,
    /// Which connection an Agent is stuck to, by index into `connections`.
    assigned: HashMap<InstanceUid, usize>,
}

impl Pool {
    pub fn new(config: Arc<ClientConfig>, registry: Arc<Registry>) -> Self {
        let limit = config.max_message_size_bytes;
        Pool {
            inner: Mutex::new(Inner {
                connections: Vec::new(),
                assigned: HashMap::new(),
            }),
            config,
            registry,
            limit,
        }
    }

    /// Forwards one report upstream on its Agent's connection, opening or re-homing as needed.
    ///
    /// The message is forwarded **unchanged** (ADR-0003): this encodes exactly what arrived, and
    /// the `Authorization` the downstream peer presented rides the upstream handshake.
    pub async fn forward(
        &self,
        uid: InstanceUid,
        report: &AgentToServer,
        authorization: Option<&str>,
    ) -> Result<(), String> {
        let frame = opamp::frame::encode_within(report, self.limit)
            .map_err(|e| format!("cannot forward a report of {uid}: {e}"))?;

        // Two attempts: the assigned connection may have died between the last send and this one,
        // and re-homing is exactly what rule 6 of ADR-0037 asks for.
        for attempt in 0..2 {
            let outbound = self.connection_for(uid, authorization).await?;
            match outbound.send(frame.clone()).await {
                Ok(()) => return Ok(()),
                Err(_) if attempt == 0 => {
                    debug!(agent = %uid, "the upstream connection is gone; re-homing");
                    self.forget_connection_of(uid);
                }
                Err(e) => return Err(format!("cannot forward a report of {uid}: {e}")),
            }
        }
        Err(format!("cannot forward a report of {uid}"))
    }

    /// The Agent's connection: the one it is stuck to while that lives, else the least-loaded one,
    /// else a new one while the cap allows.
    async fn connection_for(
        &self,
        uid: InstanceUid,
        authorization: Option<&str>,
    ) -> Result<mpsc::Sender<Vec<u8>>, String> {
        {
            let inner = self.inner.lock().expect("pool lock");
            if let Some(&index) = inner.assigned.get(&uid) {
                if let Some(upstream) = inner.connections.get(index) {
                    if upstream.alive.load(Ordering::Relaxed) {
                        return Ok(upstream.outbound.clone());
                    }
                }
            }
            // Grow only when every existing connection already carries something, and only to the
            // cap: the pool costs what it uses (ADR-0037 rule 5).
            let idle = inner
                .connections
                .iter()
                .enumerate()
                .filter(|(_, c)| c.alive.load(Ordering::Relaxed))
                .min_by_key(|(_, c)| c.carries.len());
            let reuse = match idle {
                Some((index, connection))
                    if connection.carries.is_empty()
                        || inner.connections.len() >= self.limit_connections() =>
                {
                    Some(index)
                }
                _ => None,
            };
            if let Some(index) = reuse {
                let mut inner = inner;
                inner.assigned.insert(uid, index);
                inner.connections[index].carries.push(uid);
                return Ok(inner.connections[index].outbound.clone());
            }
        }
        self.open(uid, authorization).await
    }

    fn limit_connections(&self) -> usize {
        self.config
            .gateway
            .as_ref()
            .map(|g| g.upstream_connections)
            .unwrap_or(1)
    }

    /// Opens one upstream connection and starts its reader and writer tasks.
    async fn open(
        &self,
        uid: InstanceUid,
        authorization: Option<&str>,
    ) -> Result<mpsc::Sender<Vec<u8>>, String> {
        let endpoint = self.config.endpoint.clone();
        let mut request = endpoint
            .as_str()
            .into_client_request()
            .map_err(|e| format!("invalid endpoint {endpoint}: {e}"))?;
        // The downstream peer's credential, forwarded untouched — a Gateway makes no
        // authentication decisions (ADR-0003, ADR-0035).
        if let Some(value) = authorization {
            request.headers_mut().insert(
                AUTHORIZATION,
                value
                    .parse()
                    .map_err(|e| format!("a forwarded credential is not a valid header: {e}"))?,
            );
        }
        let connector = crate::tls::rustls_client_config(&self.config)?
            .map(tokio_tungstenite::Connector::Rustls);
        let ws_config = Some(
            WebSocketConfig::default()
                .max_message_size(Some(self.limit))
                .max_frame_size(Some(self.limit)),
        );
        let (socket, _) =
            tokio_tungstenite::connect_async_tls_with_config(request, ws_config, false, connector)
                .await
                .map_err(|e| format!("cannot reach {endpoint}: {e}"))?;

        let (mut sink, mut stream) = socket.split();
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
        let alive = Arc::new(AtomicBool::new(true));

        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if sink.send(Message::Binary(frame.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let registry = self.registry.clone();
        let limit = self.limit;
        let reader_alive = alive.clone();
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                let payload = match message {
                    Ok(Message::Binary(payload)) => payload,
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                };
                match opamp::frame::decode::<ServerToAgent>(&payload, limit) {
                    // Routing is by `instance_uid` alone (ADR-0037 rule 10): a message for an Agent
                    // this Gateway has never carried is dropped, never broadcast.
                    Ok(reply) => match InstanceUid::from_wire(&reply.instance_uid) {
                        Some(uid) => registry.deliver(uid, reply).await,
                        None => warn!("dropping a Server message with a malformed instance_uid"),
                    },
                    Err(e) => warn!(error = %e, "dropping an unreadable Server message"),
                }
            }
            reader_alive.store(false, Ordering::Relaxed);
            debug!("an upstream connection closed");
        });

        let mut inner = self.inner.lock().expect("pool lock");
        inner.connections.push(Upstream {
            outbound: tx.clone(),
            carries: vec![uid],
            alive,
        });
        let index = inner.connections.len() - 1;
        inner.assigned.insert(uid, index);
        info!(
            connections = inner.connections.len(),
            endpoint = %self.config.endpoint,
            "opened an upstream connection"
        );
        Ok(tx)
    }

    /// Drops an Agent's assignment so the next report re-homes it (ADR-0037 rule 6). Nothing is
    /// said upstream on its behalf: it never disconnected.
    fn forget_connection_of(&self, uid: InstanceUid) {
        let mut inner = self.inner.lock().expect("pool lock");
        if let Some(index) = inner.assigned.remove(&uid) {
            if let Some(connection) = inner.connections.get_mut(index) {
                connection.carries.retain(|carried| *carried != uid);
            }
        }
    }

    /// How many upstream connections are open — what a test asserts the lazy growth on.
    pub fn open_connections(&self) -> usize {
        self.inner
            .lock()
            .expect("pool lock")
            .connections
            .iter()
            .filter(|c| c.alive.load(Ordering::Relaxed))
            .count()
    }
}
