//! A Server instance on PostgreSQL
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clauses 1 to 3 and 9): the lease it holds while it serves, the planes it opens only while it
//! holds one, and the changes of other instances it takes up.
//!
//! An instance takes its lease before it opens either listener and renews it while it serves. It
//! measures the lease from the moment it sent the last renewal the database confirmed, so it deems
//! it lapsed no later than the database and every other instance do. Once it cannot prove one, it
//! ends every session it carries and closes both listeners; it never serves on a lease it cannot
//! prove, because it could not learn of a revocation. When the database answers again it takes a
//! new lease under a new instance id and binds the same addresses again. On the filesystem store
//! there is one Server, no lease, and the planes are served until the Server stops.

use std::future::Future;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use opamp::server::listen::Handle;
use rustls::ServerConfig;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::config::LeaseTimes;
use crate::postgres::lease::Taken;
use crate::postgres::notify::{Heard, CERTIFICATE, HOST, REVOCATION};
use crate::postgres::Database;
use crate::revocation::Revocations;

/// How long an instance waits before it tries again to take a lease, or to bind a listener, after
/// it could not.
const RETRY: Duration = Duration::from_secs(1);

/// The lease an instance holds, as the planes see it: its instance id while it can prove one,
/// `None` while it cannot.
pub type Held = watch::Receiver<Option<String>>;

/// The lease of a running instance, renewed in the background until it is released.
///
/// The renewal runs on a thread and a runtime of its own, so a Server whose own threads wait on a
/// database that does not answer still notices the deadline on time.
pub struct Lease {
    database: Arc<Database>,
    held: watch::Receiver<Option<String>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Lease {
    /// Takes a lease on `database` for `host` and renews it every `times.renew` in the background.
    ///
    /// # Errors
    /// Returns an error when the first lease cannot be taken in time: the database does not
    /// answer, or this instance may not share the database with the instances on it.
    pub fn take(database: Arc<Database>, times: LeaseTimes, host: &str) -> Result<Self, String> {
        let taken = take_in_time(&database, times, host)?;
        info!(instance_id = %taken.instance_id, "took a lease");
        let (held, watched) = watch::channel(Some(taken.instance_id.clone()));
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let renewing = database.clone();
        let host = host.to_string();
        std::thread::Builder::new()
            .name("lease".to_string())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(async move {
                        tokio::select! {
                            () = renew(renewing, times, host, &held, taken) => {}
                            _ = stopped => {}
                        }
                    }),
                    Err(e) => error!(error = %e, "cannot start the lease's runtime"),
                }
            })
            .map_err(|e| format!("cannot start the lease's thread: {e}"))?;
        Ok(Lease {
            database,
            held: watched,
            stop: Some(stop),
        })
    }

    /// The lease as the planes see it. It ends, and the planes with it, once the instance can
    /// hold none until it is started again.
    #[must_use]
    pub fn held(&self) -> Held {
        self.held.clone()
    }

    /// Stops renewing and releases the lease, at an orderly shutdown.
    pub async fn release(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let Some(instance_id) = self.held.borrow().clone() else {
            return;
        };
        let database = self.database.clone();
        let id = instance_id.clone();
        match tokio::task::spawn_blocking(move || database.release_lease(&id)).await {
            Ok(Ok(())) => info!(instance_id = %instance_id, "released the lease"),
            Ok(Err(e)) => warn!(
                instance_id = %instance_id,
                error = %e,
                "could not release the lease; it lapses by itself"
            ),
            Err(e) => warn!(instance_id = %instance_id, error = %e, "could not release the lease"),
        }
    }
}

/// A lease taken within its own lapse: one the database answered for later than that is lapsed
/// already, as far as this instance can tell, and is not served on.
fn take_in_time(database: &Database, times: LeaseTimes, host: &str) -> Result<Taken, String> {
    let taken = database.take_lease_within(host, times.lapse, times.lapse)?;
    for gone in &taken.taken_over {
        info!(
            instance_id = %taken.instance_id,
            taken_over = %gone,
            "took over the lease of an instance that is gone"
        );
    }
    if Instant::now() >= taken.sent + times.lapse {
        return Err(format!(
            "the lease {} was answered for after it had lapsed",
            taken.instance_id
        ));
    }
    Ok(taken)
}

/// What a wait inside [`renew`] ended with.
enum Waited<T> {
    Done(T),
    Lapsed,
    LockLost,
}

/// Waits for `work` until `deadline`, and not a moment past the exclusive lock's loss.
async fn until_deadline<T>(
    database: &Database,
    deadline: Instant,
    work: impl std::future::Future<Output = T>,
) -> Waited<T> {
    let lock_lost = async {
        while !database.lock_lost() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    tokio::select! {
        done = work => Waited::Done(done),
        () = tokio::time::sleep_until(deadline.into()) => Waited::Lapsed,
        () = lock_lost => Waited::LockLost,
    }
}

/// Renews the lease `taken` until it cannot prove it; then takes a new one under a new instance
/// id, and renews that. Ends, and with it what `held` sends, once the exclusive lock of an
/// instance that must be alone is lost: such an instance takes it again only when it is started
/// again
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 2).
async fn renew(
    database: Arc<Database>,
    times: LeaseTimes,
    host: String,
    held: &watch::Sender<Option<String>>,
    taken: Taken,
) {
    let (mut instance_id, mut sent) = (taken.instance_id, taken.sent);
    loop {
        // Held: renewed every `renew`, and lost the moment `lapse` has passed since the last
        // renewal the database confirmed was sent. Each attempt is bounded by `renew`, so one
        // connection that hangs costs one attempt, not the lease.
        loop {
            let deadline = sent + times.lapse;
            let next = (sent + times.renew).min(deadline);
            let attempt = loop {
                match until_deadline(&database, deadline, tokio::time::sleep_until(next.into()))
                    .await
                {
                    Waited::Done(()) => {}
                    Waited::Lapsed => break None,
                    Waited::LockLost => return lock_lost(held, &instance_id),
                }
                let bound = times
                    .renew
                    .min(deadline.saturating_duration_since(Instant::now()));
                let (db, id) = (database.clone(), instance_id.clone());
                let renewing =
                    tokio::task::spawn_blocking(move || db.renew_lease(&id, times.lapse, bound));
                match until_deadline(&database, deadline, renewing).await {
                    Waited::Done(Ok(Ok(Some(renewal)))) => break Some(renewal),
                    Waited::Done(Ok(Ok(None))) | Waited::Lapsed => break None,
                    Waited::LockLost => return lock_lost(held, &instance_id),
                    Waited::Done(Ok(Err(e))) => {
                        warn!(instance_id = %instance_id, error = %e, "could not renew the lease");
                    }
                    Waited::Done(Err(e)) => {
                        warn!(instance_id = %instance_id, error = %e, "could not renew the lease");
                    }
                }
                // Tried again a second later, until the lease lapses: the last confirmed renewal
                // still decides when that is.
                match until_deadline(&database, deadline, tokio::time::sleep(RETRY)).await {
                    Waited::Done(()) => {}
                    Waited::Lapsed => break None,
                    Waited::LockLost => return lock_lost(held, &instance_id),
                }
            };
            match attempt {
                Some(renewal) => sent = renewal,
                None => break,
            }
        }
        error!(
            instance_id = %instance_id,
            "lost the lease: ending every session and closing both listeners until a new lease is \
             held"
        );
        held.send_replace(None);
        // Lost: a new lease under a new instance id, once the database answers in time.
        loop {
            if database.lock_lost() {
                return lock_lost(held, &instance_id);
            }
            let (db, host) = (database.clone(), host.clone());
            match tokio::task::spawn_blocking(move || take_in_time(&db, times, &host))
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            {
                Ok(taken) => {
                    info!(
                        instance_id = %taken.instance_id,
                        lost = %instance_id,
                        "took a new lease"
                    );
                    instance_id = taken.instance_id;
                    sent = taken.sent;
                    held.send_replace(Some(instance_id.clone()));
                    break;
                }
                Err(e) => {
                    warn!(lost = %instance_id, error = %e, "could not take a new lease");
                    tokio::time::sleep(RETRY.max(times.renew / 10)).await;
                }
            }
        }
    }
}

/// The end of an instance whose exclusive lock is gone: no lease from now on, and the planes end.
fn lock_lost(held: &watch::Sender<Option<String>>, instance_id: &str) {
    error!(
        instance_id = %instance_id,
        "lost the exclusive lock: ending every session, closing both listeners, and serving no more \
         until the Server is started again"
    );
    held.send_replace(None);
}

/// Takes up the revocations other instances commit
/// (ADR-draft-admission-by-client-certificate-and-revocation clause 32): each one announced, and
/// every time the listening connection is made, since what was committed before it was not heard,
/// the ledger is read again and the sessions check themselves against it. A Host or a register
/// entry another instance writes is taken up the same way
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 19). What is heard while a read runs is taken up by one read after it, and a read that
/// fails is tried again until one holds.
pub fn follow_revocations(database: &Database, revocations: Arc<Revocations>) {
    let (heard, hearing) = tokio::sync::mpsc::unbounded_channel();
    database.listen(move |what| {
        let _ = heard.send(what);
    });
    let read = move || revocations.reload_ledger();
    tokio::spawn(take_up(hearing, read, RELOAD_RETRY_FIRST, RELOAD_RETRY_MAX));
}

/// Answers what is heard with `read`, until nothing more can be heard: everything heard by the
/// time a read starts is answered by that one read, and a read that fails is tried again after
/// `first`, each wait doubled up to `longest`.
async fn take_up<R>(
    mut hearing: tokio::sync::mpsc::UnboundedReceiver<Heard>,
    read: R,
    first: Duration,
    longest: Duration,
) where
    R: Fn() -> Result<(), String> + Clone + Send + 'static,
{
    while let Some(what) = hearing.recv().await {
        let mut due = calls_for_a_read(&what);
        // Everything heard by now is answered by the one read.
        while let Ok(more) = hearing.try_recv() {
            due |= calls_for_a_read(&more);
        }
        if !due {
            continue;
        }
        let mut wait = first;
        loop {
            let read = read.clone();
            let done = tokio::task::spawn_blocking(read)
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let Err(e) = done else { break };
            warn!(error = %e, "could not read the ledger other instances committed");
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(longest);
            // What was heard while waiting is answered by the read tried again.
            while hearing.try_recv().is_ok() {}
        }
    }
}

/// The first wait before a failed read of the ledger is tried again; each next one doubles.
const RELOAD_RETRY_FIRST: Duration = Duration::from_millis(250);

/// The longest wait before a failed read of the ledger is tried again.
const RELOAD_RETRY_MAX: Duration = Duration::from_secs(10);

/// Whether what was heard calls for the ledger to be read again: a revocation, a Host or a
/// register entry another instance wrote, and the connection made again.
fn calls_for_a_read(what: &Heard) -> bool {
    match what {
        Heard::Notified(kind) => kind == REVOCATION || kind == HOST || kind == CERTIFICATE,
        Heard::Listening => true,
    }
}

/// The two planes as the binary builds them (ADR-0012): what each serves, its TLS and how many
/// connections it holds at most.
pub struct Planes {
    /// The OpAMP endpoint and the package downloads.
    pub agents: Router,
    /// The REST API, its docs, and the UI.
    pub operators: Router,
    /// The Agent plane's TLS, a client certificate required in the handshake.
    pub agent_tls: Arc<ServerConfig>,
    /// The Operator plane's TLS.
    pub operator_tls: Arc<ServerConfig>,
    /// The connections the Agent plane holds at most.
    pub agent_max_connections: usize,
    /// The new connections the Agent plane takes a second.
    pub agent_max_new_connections_per_sec: u32,
    /// The connections the Operator plane holds at most.
    pub operator_max_connections: usize,
    /// What a stop spreads the closing of the WebSocket sessions over.
    pub drain: Duration,
}

/// Serves both planes from `listeners` until `stop` resolves, then drains them over
/// [`Planes::drain`].
///
/// With a lease, the planes are open only while it is held: once it is lost, every connection is
/// cut and both listeners are closed; once a new one is held, the same addresses are bound again.
///
/// # Errors
/// Returns an error when a plane cannot be served.
pub async fn serve(
    planes: Planes,
    listeners: (TcpListener, TcpListener),
    mut held: Option<Held>,
    stop: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let addresses = (
        listeners.0.local_addr().map_err(|e| e.to_string())?,
        listeners.1.local_addr().map_err(|e| e.to_string())?,
    );
    let (stopping, stopped) = watch::channel(false);
    tokio::spawn(async move {
        stop.await;
        stopping.send_replace(true);
    });
    let mut listeners = Some(listeners);
    loop {
        if let Some(held) = held.as_mut() {
            let mut stopped = stopped.clone();
            loop {
                if *stopped.borrow() {
                    return Ok(());
                }
                if held.borrow().is_some() {
                    break;
                }
                tokio::select! {
                    changed = held.changed() => if changed.is_err() {
                        return Err(ended_for_good());
                    },
                    _ = stopped.changed() => {}
                }
            }
        }
        let (agent, operator) = match listeners.take() {
            Some(bound) => bound,
            None => match bind_again(addresses, stopped.clone()).await {
                Some(bound) => bound,
                None => return Ok(()),
            },
        };
        let handle = Handle::new();
        // The lease the planes are opened under: once it is not the one held, lost or followed by
        // another, every connection is cut, since none of them may outlive the lease it was
        // admitted under.
        let opened_under = held.as_ref().and_then(|held| held.borrow().clone());
        let drain = planes.drain;
        let watching = tokio::spawn({
            let (handle, mut held, mut stopped) = (handle.clone(), held.clone(), stopped.clone());
            async move {
                loop {
                    if *stopped.borrow() {
                        info!(drain = ?drain, "shutting down");
                        crate::listen::shut_down(&handle, drain);
                        return;
                    }
                    if held
                        .as_ref()
                        .is_some_and(|held| *held.borrow() != opened_under)
                    {
                        handle.shutdown();
                        return;
                    }
                    match held.as_mut() {
                        Some(held) => tokio::select! {
                            changed = held.changed() => if changed.is_err() {
                                handle.shutdown();
                                return;
                            },
                            _ = stopped.changed() => {}
                        },
                        None => {
                            let _ = stopped.changed().await;
                        }
                    }
                }
            }
        });
        let (agents, operators) = tokio::join!(
            crate::listen::plane(
                agent,
                Some(planes.agent_tls.clone()),
                planes.agent_max_connections,
                handle.clone()
            )
            .with_max_new_connections_per_sec(planes.agent_max_new_connections_per_sec)
            .serve(planes.agents.clone()),
            crate::listen::plane(
                operator,
                Some(planes.operator_tls.clone()),
                planes.operator_max_connections,
                handle
            )
            .serve(planes.operators.clone()),
        );
        watching.abort();
        agents.map_err(|e| format!("serving the Agent plane: {e}"))?;
        operators.map_err(|e| format!("serving the Operator plane: {e}"))?;
        if *stopped.borrow() {
            return Ok(());
        }
    }
}

/// Why the planes end for good: the instance holds no lease and will take none until it is started
/// again.
fn ended_for_good() -> String {
    "the instance lost its exclusive lock on the database and serves no more; start it again once \
     no other instance holds the database"
        .to_string()
}

/// Binds both addresses again, trying once a second until both are bound; `None` once stopped.
async fn bind_again(
    (agent, operator): (SocketAddr, SocketAddr),
    stopped: watch::Receiver<bool>,
) -> Option<(TcpListener, TcpListener)> {
    loop {
        if *stopped.borrow() {
            return None;
        }
        match (TcpListener::bind(agent), TcpListener::bind(operator)) {
            (Ok(agent), Ok(operator)) => return Some((agent, operator)),
            (Err(e), _) | (_, Err(e)) => {
                warn!(error = %e, "could not bind the planes again; trying once a second");
                let mut stopped = stopped.clone();
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => {}
                    _ = stopped.changed() => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::{take_up, Heard};

    /// A reader that fails its first `failing` reads and counts every read.
    fn reader(failing: usize) -> (Arc<AtomicUsize>, impl Fn() -> Result<(), String> + Clone) {
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let read = move || {
            let read = counted.fetch_add(1, Ordering::SeqCst);
            if read < failing {
                Err("the database is away".to_string())
            } else {
                Ok(())
            }
        };
        (reads, read)
    }

    /// What is heard before a read starts is answered by one read, and what calls for none is
    /// passed over (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 19).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[tokio::test]
    async fn what_is_heard_at_once_is_answered_by_one_read() {
        let (heard, hearing) = tokio::sync::mpsc::unbounded_channel();
        for kind in ["host", "certificate", "revocation"] {
            heard.send(Heard::Notified(kind.to_string())).expect("send");
        }
        heard.send(Heard::Listening).expect("send");
        heard
            .send(Heard::Notified("floor".to_string()))
            .expect("send");
        drop(heard);
        let (reads, read) = reader(0);
        take_up(
            hearing,
            read,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
        .await;
        assert_eq!(reads.load(Ordering::SeqCst), 1);

        let (heard, hearing) = tokio::sync::mpsc::unbounded_channel();
        heard
            .send(Heard::Notified("floor".to_string()))
            .expect("send");
        drop(heard);
        let (reads, read) = reader(0);
        take_up(
            hearing,
            read,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
        .await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            0,
            "a floor calls for no read of the ledger"
        );
    }

    /// A read that fails is tried again, each wait longer, until one holds.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[tokio::test]
    async fn a_read_that_fails_is_tried_again_until_one_holds() {
        let (heard, hearing) = tokio::sync::mpsc::unbounded_channel();
        heard
            .send(Heard::Notified("host".to_string()))
            .expect("send");
        drop(heard);
        let (reads, read) = reader(3);
        let started = tokio::time::Instant::now();
        take_up(
            hearing,
            read,
            Duration::from_millis(10),
            Duration::from_millis(25),
        )
        .await;
        assert_eq!(
            reads.load(Ordering::SeqCst),
            4,
            "three failures, then the read that holds"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(10 + 20 + 25),
            "each wait doubles, up to the longest"
        );
    }
}
