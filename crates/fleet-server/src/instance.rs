//! A Server instance on PostgreSQL
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clauses 1 to 3, 9 and 10): the lease it holds while it serves, the planes it opens only while it
//! holds one, and the changes of other instances it takes up.
//!
//! An instance takes its lease before it opens either listener and renews it while it serves. It
//! measures the lease from the moment it sent the last renewal the database confirmed, so it deems
//! it lapsed no later than the database and every other instance do. Once it cannot prove one, it
//! ends every session it carries and closes both listeners; it never serves on a lease it cannot
//! prove, because it could not learn of a revocation. When the database answers again it takes a
//! new lease under a new instance id and binds the same addresses again. On the filesystem store
//! there is one Server, no lease, and the planes are served until the Server stops.

use std::collections::HashSet;
use std::future::Future;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use opamp::server::listen::Handle;
use opamp::uid::InstanceUid;
use rustls::ServerConfig;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::config::LeaseTimes;
use crate::fleet::AppState;
use crate::postgres::lease::Taken;
use crate::postgres::notify::{
    Heard, ASSIGNMENT, CERTIFICATE, CONFIGURATION, DEPLOYMENT, ENROLMENT, FLOOR, HOST, REVOCATION,
    WINDOW,
};
use crate::postgres::Database;

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
        let instance_id = crate::postgres::lease::new_instance_id()?;
        Self::take_as(database, times, host, instance_id)
    }

    /// [`take`](Self::take) under `instance_id`, the one the instance made at startup and its log
    /// names already.
    ///
    /// # Errors
    /// As [`take`](Self::take).
    pub fn take_as(
        database: Arc<Database>,
        times: LeaseTimes,
        host: &str,
        instance_id: String,
    ) -> Result<Self, String> {
        let taken = take_in_time(&database, times, host, instance_id, None)?;
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

/// A lease taken under `instance_id` within its own lapse: one the database answered for later
/// than that is lapsed already, as far as this instance can tell, and is not served on. `own` is
/// the lease this instance lost, if it lost one: a take that removes it logs it as this instance's
/// own lapsed lease, and every other lease it removes as one of an instance that is gone.
pub(crate) fn take_in_time(
    database: &Database,
    times: LeaseTimes,
    host: &str,
    instance_id: String,
    own: Option<&str>,
) -> Result<Taken, String> {
    let taken = database.take_lease_within(instance_id, host, times.lapse, times.lapse)?;
    for removed in &taken.taken_over {
        if own == Some(removed.as_str()) {
            info!(
                instance_id = %taken.instance_id,
                lapsed = %removed,
                "removed this instance's own lapsed lease"
            );
        } else {
            info!(
                instance_id = %taken.instance_id,
                taken_over = %removed,
                "took over the lease of an instance that is gone"
            );
        }
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
            let (db, host, lost) = (database.clone(), host.clone(), instance_id.clone());
            match tokio::task::spawn_blocking(move || {
                let fresh = crate::postgres::lease::new_instance_id()?;
                take_in_time(&db, times, &host, fresh, Some(&lost))
            })
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

/// Takes up the changes other instances commit
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clauses 9 and 10): for each one announced, what its kind concerns is read again. A revocation, a
/// Host or a register entry has the ledger read again, and the sessions check themselves against it
/// (ADR-draft-admission-by-client-certificate-and-revocation clause 32); a floor has the floors read
/// again; an enrolment request or the window has the window and its queue read again, and the
/// connections waiting on them answered; a Configuration or a Deployment has its store read again
/// (clause 12); an Agent's assignment has its record read again, and its connection offered what
/// it was released. Every time the listening connection is made, since what
/// was committed before it was not heard, and for a kind this instance does not know, everything is
/// read again. What this instance announced itself is never heard: it holds what it committed.
/// What is heard while a read runs is taken up by one read after it, and a read that fails is tried
/// again until one holds.
pub fn follow(database: &Database, state: Arc<AppState>) -> Following {
    let (heard, hearing) = tokio::sync::mpsc::unbounded_channel();
    database.listen(move |what| {
        let _ = heard.send(what);
    });
    let following = Following::default();
    let counted = following.clone();
    let read = move |due: Due| {
        if due.ledger {
            if let Some(revocations) = state.revocations() {
                revocations.reload_ledger()?;
            }
            counted.ledger.fetch_add(1, Ordering::SeqCst);
        }
        if due.floors {
            if let Some(floors) = state.floors() {
                floors.reload()?;
            }
        }
        if due.enrolment {
            if let Some(enrolment) = state.enrolment() {
                enrolment.reload()?;
            }
        }
        // The Configurations before the assignments, so an Agent released a revision another
        // instance saved is offered it from what this one holds.
        state.reload_catalogue(due.configurations, due.deployments)?;
        match &due.agents {
            Agents::Named(uids) if uids.is_empty() => {}
            Agents::Named(uids) => state.take_up_assignments(Some(uids))?,
            Agents::All => state.take_up_assignments(None)?,
        }
        Ok(())
    };
    tokio::spawn(take_up(hearing, read, RELOAD_RETRY_FIRST, RELOAD_RETRY_MAX));
    following
}

/// What [`follow`] has read again so far, for a test to tell what it took up.
#[derive(Clone, Default)]
pub struct Following {
    ledger: Arc<AtomicU64>,
}

impl Following {
    /// How many times the ledger was read again for what was heard.
    #[must_use]
    pub fn ledger_reads(&self) -> u64 {
        self.ledger.load(Ordering::SeqCst)
    }
}

/// The Agents whose records are read again.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Agents {
    Named(HashSet<InstanceUid>),
    All,
}

impl Default for Agents {
    fn default() -> Self {
        Agents::Named(HashSet::new())
    }
}

/// What is read again for what was heard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Due {
    ledger: bool,
    floors: bool,
    enrolment: bool,
    configurations: bool,
    deployments: bool,
    agents: Agents,
}

impl Due {
    /// Everything: what a missed notification, or one of a kind not known, calls for.
    const EVERYTHING: Due = Due {
        ledger: true,
        floors: true,
        enrolment: true,
        configurations: true,
        deployments: true,
        agents: Agents::All,
    };

    /// What `what` calls for.
    fn of(what: &Heard) -> Due {
        let Heard::Notified(notice) = what else {
            return Due::EVERYTHING;
        };
        match notice.kind.as_str() {
            REVOCATION | HOST | CERTIFICATE => Due {
                ledger: true,
                ..Due::default()
            },
            FLOOR => Due {
                floors: true,
                ..Due::default()
            },
            ENROLMENT | WINDOW => Due {
                enrolment: true,
                ..Due::default()
            },
            CONFIGURATION => Due {
                configurations: true,
                ..Due::default()
            },
            DEPLOYMENT => Due {
                deployments: true,
                ..Due::default()
            },
            ASSIGNMENT => Due {
                agents: match InstanceUid::parse(&notice.id) {
                    Some(uid) => Agents::Named(HashSet::from([uid])),
                    None => Agents::All,
                },
                ..Due::default()
            },
            _ => Due::EVERYTHING,
        }
    }

    fn and(self, other: Due) -> Due {
        Due {
            ledger: self.ledger || other.ledger,
            floors: self.floors || other.floors,
            enrolment: self.enrolment || other.enrolment,
            configurations: self.configurations || other.configurations,
            deployments: self.deployments || other.deployments,
            agents: match (self.agents, other.agents) {
                (Agents::Named(mut uids), Agents::Named(more)) => {
                    uids.extend(more);
                    Agents::Named(uids)
                }
                _ => Agents::All,
            },
        }
    }
}

/// Answers what is heard with `read`, until nothing more can be heard: everything heard by the
/// time a read starts is answered by that one read, and a read that fails is tried again after
/// `first`, each wait doubled up to `longest`, together with what was heard meanwhile.
async fn take_up<R>(
    mut hearing: tokio::sync::mpsc::UnboundedReceiver<Heard>,
    read: R,
    first: Duration,
    longest: Duration,
) where
    R: Fn(Due) -> Result<(), String> + Clone + Send + 'static,
{
    while let Some(what) = hearing.recv().await {
        let mut due = Due::of(&what);
        // Everything heard by now is answered by the one read.
        while let Ok(more) = hearing.try_recv() {
            due = due.and(Due::of(&more));
        }
        let mut wait = first;
        loop {
            let (read, reading) = (read.clone(), due.clone());
            let done = tokio::task::spawn_blocking(move || read(reading))
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
            let Err(e) = done else { break };
            warn!(error = %e, "could not read again what other instances committed");
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(longest);
            // What was heard while waiting is answered by the read tried again.
            while let Ok(more) = hearing.try_recv() {
                due = due.and(Due::of(&more));
            }
        }
    }
}

/// The first wait before a failed read is tried again; each next one doubles.
const RELOAD_RETRY_FIRST: Duration = Duration::from_millis(250);

/// The longest wait before a failed read is tried again.
const RELOAD_RETRY_MAX: Duration = Duration::from_secs(10);

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

/// The instance id the log names
/// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
/// clause 1): the one the instance makes at startup, once its store is known to be PostgreSQL and
/// before it takes its first lease under it; then the one of each new lease. Between a lost lease
/// and the next it names the lost one, so every line names an instance. A line logged before the
/// configuration is read cannot know the store and names none; on the filesystem store it is
/// never set, and the log is as it was.
///
/// It follows the lease through a task, so an ordinary line logged at the moment the lease changes
/// can still name the id before it; the lines of the lease's own take, loss, release and takeover
/// name their ids as fields of their own and are exact.
#[derive(Clone, Default)]
pub struct LoggedId(Arc<std::sync::RwLock<Option<String>>>);

impl LoggedId {
    /// The instance id the log names now.
    #[must_use]
    pub fn get(&self) -> Option<String> {
        self.0.read().ok().and_then(|id| id.clone())
    }

    /// Names `id` from now on: the instance id made at startup.
    pub fn name(&self, id: String) {
        if let Ok(mut held) = self.0.write() {
            *held = Some(id);
        }
    }

    /// Writes `instance_id=<id> ` into `writer` while an id is named, without copying it.
    fn write_prefix(&self, writer: &mut impl std::fmt::Write) -> std::fmt::Result {
        match self.0.read() {
            Ok(id) => match id.as_deref() {
                Some(id) => write!(writer, "instance_id={id} "),
                None => Ok(()),
            },
            Err(_) => Ok(()),
        }
    }

    /// Names the instance id of each lease `held` sends, from now on and at each change. While the
    /// lease is lost, and once it can be held no more, the last one stays named.
    pub fn follow(&self, mut held: Held) {
        if let Some(id) = held.borrow_and_update().clone() {
            self.name(id);
        }
        let logged = self.clone();
        tokio::spawn(async move {
            while held.changed().await.is_ok() {
                if let Some(id) = held.borrow_and_update().clone() {
                    logged.name(id);
                }
            }
        });
    }
}

/// A log format that begins each line with `instance_id=<id>` while [`LoggedId`] names one, and
/// is `inner` otherwise. A line that names an `instance_id` field of its own, as a lease's take,
/// loss and takeover do, is left as `inner` writes it, so it names each id once.
pub struct NamesInstance<F> {
    inner: F,
    id: LoggedId,
}

impl<F> NamesInstance<F> {
    /// `inner`, its lines naming the instance id `id` holds.
    pub fn new(inner: F, id: LoggedId) -> Self {
        NamesInstance { inner, id }
    }
}

impl<S, N, F> tracing_subscriber::fmt::FormatEvent<S, N> for NamesInstance<F>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'w> tracing_subscriber::fmt::FormatFields<'w> + 'static,
    F: tracing_subscriber::fmt::FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        if event.metadata().fields().field("instance_id").is_none() {
            self.id.write_prefix(&mut writer)?;
        }
        self.inner.format_event(ctx, writer, event)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use std::collections::HashSet;

    use opamp::uid::InstanceUid;

    use super::{take_up, Agents, Due, Heard, LoggedId, NamesInstance};
    use crate::postgres::notify::Notice;

    /// What the log writes, collected.
    #[derive(Clone, Default)]
    struct Collected(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Collected {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Collected {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().expect("log").clone())
                .expect("utf-8")
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    /// The Server's log format over `id`, writing into what it returns.
    fn log(id: &LoggedId) -> (Collected, impl tracing::Subscriber + Send + Sync) {
        let collected = Collected::default();
        let writer = collected.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .event_format(NamesInstance::new(
                tracing_subscriber::fmt::format().without_time(),
                id.clone(),
            ))
            .finish();
        (collected, subscriber)
    }

    /// Each line names the instance id of the lease the instance holds, and none while it holds
    /// none; a line that names an instance id of its own names it once, as the lease's take and
    /// takeover do
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 1).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn each_line_names_the_instance_id_of_the_lease_held() {
        let id = LoggedId::default();
        let (collected, subscriber) = log(&id);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("before the store is known");
            id.name("a1".to_string());
            tracing::info!(agents = 3, "serving");
            tracing::info!(instance_id = "b2", taken_over = "a1", "took over");
            tracing::warn!("between leases");
        });
        let lines = collected.lines();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(!lines[0].contains("instance_id"), "{}", lines[0]);
        assert!(lines[1].starts_with("instance_id=a1 "), "{}", lines[1]);
        assert!(lines[1].contains("serving agents=3"), "{}", lines[1]);
        assert_eq!(lines[2].matches("instance_id").count(), 1, "{}", lines[2]);
        assert!(
            lines[2].contains("instance_id=\"b2\" taken_over=\"a1\""),
            "{}",
            lines[2]
        );
        assert!(lines[3].starts_with("instance_id=a1 "), "{}", lines[3]);
    }

    /// The log follows the lease: the id it holds and each new one, and the last one while the
    /// lease is lost or can be held no more, so every line names an instance.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[tokio::test]
    async fn the_logged_id_follows_the_lease() {
        let (held, watched) = tokio::sync::watch::channel(Some("a1".to_string()));
        let id = LoggedId::default();
        id.follow(watched);
        assert_eq!(id.get().as_deref(), Some("a1"));
        let settled = |want: &'static str| {
            let id = id.clone();
            async move {
                for _ in 0..200 {
                    if id.get().as_deref() == Some(want) {
                        return true;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                false
            }
        };
        held.send_replace(None);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            id.get().as_deref(),
            Some("a1"),
            "a lost lease names the lost id"
        );
        held.send_replace(Some("b2".to_string()));
        assert!(settled("b2").await, "a new lease names its id");
        drop(held);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            id.get().as_deref(),
            Some("b2"),
            "a lease held no more names the last id"
        );
    }

    /// What each read was due, in the order of the reads.
    type Reads = Arc<Mutex<Vec<Due>>>;

    /// A reader that fails its first `failing` reads and records what each read was due.
    fn reader(failing: usize) -> (Reads, impl Fn(Due) -> Result<(), String> + Clone) {
        let reads = Arc::new(Mutex::new(Vec::new()));
        let recorded = reads.clone();
        let read = move |due| {
            let mut reads = recorded.lock().expect("reads");
            reads.push(due);
            if reads.len() <= failing {
                Err("the database is away".to_string())
            } else {
                Ok(())
            }
        };
        (reads, read)
    }

    /// What `heard` comes to, read by a reader that never fails.
    async fn reads_for(heard: Vec<Heard>) -> Vec<Due> {
        let (sender, hearing) = tokio::sync::mpsc::unbounded_channel();
        for what in heard {
            sender.send(what).expect("send");
        }
        drop(sender);
        let (reads, read) = reader(0);
        take_up(
            hearing,
            read,
            Duration::from_millis(1),
            Duration::from_millis(4),
        )
        .await;
        let reads = reads.lock().expect("reads").clone();
        reads
    }

    fn notified(kind: &str) -> Heard {
        Heard::Notified(Notice::new(kind, "id"))
    }

    /// What is heard before a read starts is answered by one read, and each kind has what it
    /// concerns read again and nothing else
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clauses 9 and 19).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[tokio::test]
    async fn what_is_heard_at_once_is_answered_by_one_read_of_what_it_concerns() {
        let ledger = Due {
            ledger: true,
            ..Due::default()
        };
        assert_eq!(
            reads_for(vec![
                notified("host"),
                notified("certificate"),
                notified("revocation")
            ])
            .await,
            vec![ledger]
        );
        assert_eq!(
            reads_for(vec![notified("floor")]).await,
            vec![Due {
                floors: true,
                ..Due::default()
            }],
            "a floor calls for no read of the ledger"
        );
        assert_eq!(
            reads_for(vec![notified("enrolment"), notified("window")]).await,
            vec![Due {
                enrolment: true,
                ..Due::default()
            }]
        );
        let (one, two) = (InstanceUid([1; 16]), InstanceUid([2; 16]));
        assert_eq!(
            reads_for(vec![
                Heard::Notified(Notice::new("configuration", "collector")),
                Heard::Notified(Notice::new("assignment", &one.to_string())),
                Heard::Notified(Notice::new("assignment", &two.to_string())),
                Heard::Notified(Notice::new("deployment", "stable")),
            ])
            .await,
            vec![Due {
                configurations: true,
                deployments: true,
                agents: Agents::Named(HashSet::from([one, two])),
                ..Due::default()
            }],
            "an assignment has the record of the Agent it names read again"
        );
        assert_eq!(
            reads_for(vec![
                Heard::Notified(Notice::new("assignment", &one.to_string())),
                Heard::Notified(Notice::new("assignment", "not an instance_uid")),
            ])
            .await,
            vec![Due {
                agents: Agents::All,
                ..Due::default()
            }],
            "an assignment that names no Agent has every record read again"
        );
        assert_eq!(
            reads_for(vec![notified("floor"), notified("revocation")]).await,
            vec![Due {
                ledger: true,
                floors: true,
                ..Due::default()
            }]
        );
    }

    /// A listening connection made again, and a notification of a kind the instance does not
    /// know, have everything read again
    /// (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    /// clause 10).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[tokio::test]
    async fn a_missed_or_unknown_notification_has_everything_read_again() {
        assert_eq!(
            reads_for(vec![Heard::Listening]).await,
            vec![Due::EVERYTHING]
        );
        assert_eq!(
            reads_for(vec![notified("from-a-newer-version")]).await,
            vec![Due::EVERYTHING]
        );
        assert_eq!(reads_for(vec![notified("")]).await, vec![Due::EVERYTHING]);
    }

    /// A read that fails is tried again, each wait longer, until one holds, together with what was
    /// heard meanwhile.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[tokio::test]
    async fn a_read_that_fails_is_tried_again_until_one_holds() {
        let (heard, hearing) = tokio::sync::mpsc::unbounded_channel();
        heard.send(notified("host")).expect("send");
        let (reads, read) = reader(3);
        let started = tokio::time::Instant::now();
        let taking = tokio::spawn(take_up(
            hearing,
            read,
            Duration::from_millis(10),
            Duration::from_millis(25),
        ));
        heard.send(notified("floor")).expect("send");
        drop(heard);
        taking.await.expect("take up");
        let reads = reads.lock().expect("reads").clone();
        assert_eq!(reads.len(), 4, "three failures, then the read that holds");
        assert_eq!(
            reads.last(),
            Some(&Due {
                ledger: true,
                floors: true,
                ..Due::default()
            }),
            "what was heard while the reads failed is read with them"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(10 + 20 + 25),
            "each wait doubles, up to the longest"
        );
    }
}
