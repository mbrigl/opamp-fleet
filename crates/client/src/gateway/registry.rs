//! Where a `ServerToAgent` goes on its way down (ADR-0037 rule 10).
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
use std::sync::Mutex;

use opamp::proto::ServerToAgent;
use opamp::uid::InstanceUid;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

/// Who is waiting for an Agent's replies.
enum Downstream {
    /// A WebSocket peer: everything for this Agent goes down this channel until it disconnects.
    Socket(mpsc::Sender<ServerToAgent>),
    /// A plain-HTTP peer mid-exchange: one reply, then gone.
    Exchange(oneshot::Sender<ServerToAgent>),
}

#[derive(Default)]
pub struct Registry {
    routes: Mutex<HashMap<InstanceUid, Downstream>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    /// A WebSocket peer claims an Agent. Claiming again replaces the route: an Agent that
    /// reconnects through a second socket is reachable on the new one, which is the same
    /// last-writer-wins the Server applies to its own connection ownership.
    pub fn attach(&self, uid: InstanceUid, sink: mpsc::Sender<ServerToAgent>) {
        self.routes
            .lock()
            .expect("registry lock")
            .insert(uid, Downstream::Socket(sink));
    }

    /// A plain-HTTP peer waits for exactly one reply for this Agent.
    pub fn expect_once(&self, uid: InstanceUid, reply: oneshot::Sender<ServerToAgent>) {
        self.routes
            .lock()
            .expect("registry lock")
            .insert(uid, Downstream::Exchange(reply));
    }

    /// Releases every Agent a departing WebSocket peer carried.
    ///
    /// Nothing is sent upstream about it: a downstream Client that vanished said no goodbye, and
    /// this Gateway does not say one for it (ADR-0037 rule 7).
    pub fn detach_all(&self, uids: &[InstanceUid]) {
        let mut routes = self.routes.lock().expect("registry lock");
        for uid in uids {
            routes.remove(uid);
        }
    }

    /// Hands one reply to whoever is waiting for that Agent.
    pub async fn deliver(&self, uid: InstanceUid, reply: ServerToAgent) {
        // Taken out under the lock, awaited outside it: a slow downstream peer must not hold the
        // routing table while every other Agent's replies queue behind it.
        let route = {
            let mut routes = self.routes.lock().expect("registry lock");
            match routes.get(&uid) {
                Some(Downstream::Exchange(_)) => routes.remove(&uid),
                Some(Downstream::Socket(sink)) => Some(Downstream::Socket(sink.clone())),
                None => None,
            }
        };
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
