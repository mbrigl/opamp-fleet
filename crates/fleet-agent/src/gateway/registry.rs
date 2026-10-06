//! Where a `ServerToAgent` goes on its way down (ADR-0034 clause 13).
//!
//! The Gateway routes by `instance_uid` and nothing else, in both directions. Upward that needs no
//! lookup — a report leaves on its own Agent's connection. Downward it does: the pool's reader
//! tasks see replies for every Agent the Gateway carries, and each has to reach the downstream
//! connection that Agent is actually on.
//!
//! Two shapes of downstream connection, because there are two transports. A WebSocket peer holds a
//! channel open for as long as it is connected; a plain-HTTP peer is a single exchange waiting for
//! exactly one reply. Both are registered here, and a reply for an `instance_uid` nobody claims is
//! dropped with a log line rather than broadcast.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use opamp::proto::ServerToAgent;
use opamp::uid::InstanceUid;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::gateway::cache::PackageCache;

/// Who is waiting for an Agent's replies, and the host its certificate names.
struct Route {
    downstream: Downstream,
    /// The host the downstream connection's certificate names (`urn:opamp-fleet:host:<id>`), the
    /// one an offer relayed over it is recorded for (ADR-0033 clause 11).
    host: Option<String>,
}

/// Who is waiting for an Agent's replies.
enum Downstream {
    /// A WebSocket peer: everything for this Agent goes down this channel until it disconnects.
    Socket(mpsc::Sender<ServerToAgent>),
    /// A plain-HTTP peer mid-exchange: one reply, then gone.
    Exchange(oneshot::Sender<ServerToAgent>),
}

pub struct Registry {
    routes: Mutex<HashMap<InstanceUid, Route>>,
    /// What sees every offer on its way down (ADR-0033 clause 8).
    cache: Arc<PackageCache>,
}

impl Registry {
    pub fn new(cache: Arc<PackageCache>) -> Self {
        Registry {
            routes: Mutex::new(HashMap::new()),
            cache,
        }
    }

    /// A WebSocket peer claims an Agent. Claiming again replaces the route: an Agent that
    /// reconnects through a second socket is reachable on the new one, which is the same
    /// last-writer-wins the Server applies to its own connection ownership.
    pub fn attach(
        &self,
        uid: InstanceUid,
        sink: mpsc::Sender<ServerToAgent>,
        host: Option<String>,
    ) {
        // A report binds its `instance_uid` to its host, the first one only (ADR-0033 clause 11);
        // an `instance_uid` with a route here is never the binding that makes room.
        let mut routes = self.routes.lock().expect("registry lock");
        self.cache
            .bind(uid, host.as_deref(), |uid| routes.contains_key(uid));
        routes.insert(
            uid,
            Route {
                downstream: Downstream::Socket(sink),
                host,
            },
        );
    }

    /// A plain-HTTP peer waits for exactly one reply for this Agent.
    pub fn expect_once(
        &self,
        uid: InstanceUid,
        reply: oneshot::Sender<ServerToAgent>,
        host: Option<String>,
    ) {
        let mut routes = self.routes.lock().expect("registry lock");
        self.cache
            .bind(uid, host.as_deref(), |uid| routes.contains_key(uid));
        routes.insert(
            uid,
            Route {
                downstream: Downstream::Exchange(reply),
                host,
            },
        );
    }

    /// Releases every Agent a departing WebSocket peer carried.
    ///
    /// Nothing is sent upstream about it: a downstream Client that vanished said no goodbye, and
    /// this Gateway does not say one for it (ADR-0034 clause 10).
    pub fn detach_all(&self, uids: impl IntoIterator<Item = InstanceUid>) {
        let mut routes = self.routes.lock().expect("registry lock");
        for uid in uids {
            routes.remove(&uid);
        }
    }

    /// Hands one reply to whoever is waiting for that Agent, once the package cache has seen what
    /// it offers — so an artifact the Agent asks for next is already recorded as offered to its host
    /// (ADR-0033 clauses 8, 11).
    pub async fn deliver(&self, uid: InstanceUid, reply: ServerToAgent) {
        // Taken out under the lock, awaited outside it: a slow downstream peer must not hold the
        // routing table while every other Agent's replies queue behind it.
        let route = {
            let mut routes = self.routes.lock().expect("registry lock");
            let host = routes.get(&uid).and_then(|route| route.host.clone());
            let downstream = match routes.get(&uid).map(|route| &route.downstream) {
                Some(Downstream::Exchange(_)) => routes.remove(&uid).map(|route| route.downstream),
                Some(Downstream::Socket(sink)) => Some(Downstream::Socket(sink.clone())),
                None => None,
            };
            downstream.map(|downstream| (downstream, host))
        };
        let route = route.map(|(downstream, host)| {
            self.cache.observe(uid, host, &reply);
            downstream
        });
        match route {
            Some(Downstream::Socket(sink)) => {
                let _ = sink.send(reply).await;
            }
            Some(Downstream::Exchange(reply_to)) => {
                let _ = reply_to.send(reply);
            }
            None => debug!(agent = %uid, "dropping a Server message for an unknown Agent"),
        }
    }
}
