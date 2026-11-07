//! The notifications the instances on one database share
//! (ADR-0041
//! clause 9): one channel per schema, a `NOTIFY` that names a kind of change, the identifier it
//! concerns and the instance that sent it, and nothing else, sent in the transaction that commits
//! the change, and a connection of each instance's own that listens. A channel is the database's,
//! not the schema's, so its name carries the schema: two fleets in two schemas of one database
//! never hear each other.
//!
//! Every connection of an instance carries the instance's origin as the session setting
//! [`ORIGIN_SETTING`], so a notification names its sender whichever store sends it, and the
//! listener hands on none that its own instance sent: that instance holds what it committed
//! already. A call whose answer is lost is the exception: the instance cannot tell whether it
//! committed, so it is handed the call's notifications at once, and hears its own notification of
//! each once more ([`Unsure`]).
//!
//! A binding a report makes, and a Host row made or whose valid until moves forward, by a report,
//! an issuance or a prune, are announced right after their commit instead, in a transaction of
//! their own that does not wait for its flush ([`will_follow`], [`follow_commit`]): PostgreSQL
//! commits one transaction that sent a `NOTIFY` at a time, database-wide, across its flush, so
//! announcing each new Agent's binding in its commit would let a connecting fleet in no faster than
//! one flush apiece (the several-instances draft, clause 9). Such a notice may be lost; every lease
//! renewal reads the ledger again for it (clause 10).

use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::Cached as GenericClient;
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, NoTls, Socket};

use super::{sql, Database, Tls};

/// The channel every instance on a schema listens on, as SQL: its name and the schema's.
const CHANNEL: &str = "'opamp_fleet:' || coalesce(current_schema(), '')";

/// The session setting every connection of an instance carries its origin in.
pub(crate) const ORIGIN_SETTING: &str = "opamp_fleet.origin";

/// The kind a revocation added or lifted is announced as.
pub const REVOCATION: &str = "revocation";

/// The kind a package floor set or removed is announced as.
pub const FLOOR: &str = "floor";

/// The kind a register entry written or removed is announced as.
pub const CERTIFICATE: &str = "certificate";

/// The kind a Host written or removed, or an `instance_uid` bound or released, is announced as.
pub const HOST: &str = "host";

/// The kind an enrolment request queued, decided or withdrawn is announced as.
pub const ENROLMENT: &str = "enrolment";

/// The kind the enrolment window opened, moved, closed or ended is announced as.
pub const WINDOW: &str = "window";

/// The kind a Configuration saved, collected or deleted is announced as, by its name.
pub const CONFIGURATION: &str = "configuration";

/// The kind a Deployment written or deleted, or a member of it added or removed, is announced as,
/// by its name.
pub const DEPLOYMENT: &str = "deployment";

/// The kind a Package created or deleted, or an entry or a signature of it written or removed,
/// is announced as, by its `<agent type>@<version>`.
pub const PACKAGE: &str = "package";

/// The kind an Agent's assignment changed by a rollout is announced as, by its `instance_uid`.
pub const ASSIGNMENT: &str = "assignment";

/// The kind an Agent's labels set is announced as, by its `instance_uid`.
pub const LABELS: &str = "labels";

/// The kind a restart requested for an Agent is announced as, by its `instance_uid`.
pub const RESTART: &str = "restart";

/// The kind an Agent forgotten is announced as, by its `instance_uid`.
pub const FORGOTTEN: &str = "forgotten";

/// The kind an Agent taken from another instance with a live lease is announced as, by its
/// `instance_uid`: the instance it was taken from reads its holder again and stops carrying it
/// (ADR-0041
/// clause 8).
pub const TAKEN: &str = "taken";

/// How long a listener waits before it connects again after its connection ended.
const RECONNECT: Duration = Duration::from_secs(1);

/// One notification as it travels: its kind, the identifier it concerns, and the origin of the
/// instance that sent it.
#[derive(Serialize, Deserialize)]
struct Payload {
    kind: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    from: Option<String>,
    /// The call of its sender that sent it, by which that sender tells a call whose answer was
    /// lost from its others ([`Unsure`]); a version that does not know it reads past it.
    #[serde(default)]
    call: Option<String>,
}

/// A change another instance committed: its kind and the identifier it concerns, empty where the
/// kind names none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub kind: String,
    pub id: String,
}

impl Notice {
    /// A notice of `kind` concerning `id`.
    #[must_use]
    pub fn new(kind: &str, id: &str) -> Self {
        Notice {
            kind: kind.to_string(),
            id: id.to_string(),
        }
    }
}

/// What a listener hears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// Another instance committed a change of this kind. A payload that does not read is heard as
    /// a notice of a kind no instance knows.
    Notified(Notice),
    /// The listening connection was made, the first time or again after it ended: whatever was
    /// committed in between was not heard.
    Listening,
}

tokio::task_local! {
    /// The call being run ([`sending`]).
    static CALL: Arc<Call>;
}

/// The calls of this process so far, which mark each call apart.
static CALLS: AtomicU64 = AtomicU64::new(0);

/// One call to the database: the mark its notifications carry, those it has sent so far, and those
/// that follow its commit.
pub(crate) struct Call {
    mark: String,
    sent: Mutex<Vec<Notice>>,
    following: Mutex<Vec<Notice>>,
    afterwards: Arc<Afterwards>,
}

impl Call {
    /// A call with a mark of its own, whose notices after its commit go through `afterwards`.
    pub(crate) fn new(afterwards: Arc<Afterwards>) -> Arc<Call> {
        Arc::new(Call {
            mark: CALLS.fetch_add(1, Ordering::Relaxed).to_string(),
            sent: Mutex::new(Vec::new()),
            following: Mutex::new(Vec::new()),
            afterwards,
        })
    }

    /// The notifications it has sent, taken.
    pub(crate) fn take(&self) -> Vec<Notice> {
        std::mem::take(&mut *self.sent.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The notices that were to follow its commit, taken.
    pub(crate) fn take_following(&self) -> Vec<Notice> {
        std::mem::take(
            &mut *self
                .following
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// The mark its notifications carry.
    pub(crate) fn mark(&self) -> &str {
        &self.mark
    }
}

/// `work` as `call`: its every [`notify`] carries the call's mark and is counted into it, even
/// where `work` is dropped unfinished, which is what a call whose answer is lost may have
/// committed.
pub(crate) fn sending<F: std::future::Future>(
    call: Arc<Call>,
    work: F,
) -> impl std::future::Future<Output = F::Output> {
    CALL.scope(call, work)
}

/// How many of its own notifications an instance waits to hear at most; the oldest goes first. An
/// entry stays where its call was rolled back, refused, or committed before its answer was lost
/// and was heard before it was awaited; none of these is heard again, and no other call's
/// notification matches it, so dropping the oldest loses nothing a commit still to land needs
/// unless this many calls lost their answers since.
const AWAITED_AT_MOST: usize = 1024;

/// The notifications of this instance's calls whose answer was lost: it does not know whether they
/// committed. Each is handed to the listener's `on` at once, as if heard, and its own notification
/// of the same kind and identifier is handed on once more when it is heard: a commit that landed
/// before the call failed is read by the first, one that lands after it by the second.
#[derive(Default)]
pub(crate) struct Unsure {
    state: Mutex<UnsureState>,
}

#[derive(Default)]
struct UnsureState {
    /// What [`Database::listen`] hands on to; none while nothing listens.
    on: Option<Arc<dyn Fn(Heard) + Send + Sync>>,
    /// The notifications of this instance's own that are handed on when heard, each once, with the
    /// mark of the call that sent them.
    awaited: VecDeque<(Notice, String)>,
}

impl Unsure {
    /// The notifications of the call marked `call`, whose answer was lost: awaited, then handed on
    /// at once.
    pub(crate) fn lost(&self, sent: Vec<Notice>, call: &str) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut lost: Vec<Notice> = Vec::new();
        for notice in sent {
            if !lost.contains(&notice) {
                lost.push(notice);
            }
        }
        for notice in &lost {
            if state.awaited.len() == AWAITED_AT_MOST {
                state.awaited.pop_front();
            }
            state.awaited.push_back((notice.clone(), call.to_string()));
        }
        let on = state.on.clone();
        drop(state);
        if let Some(on) = on {
            for notice in lost {
                on(Heard::Notified(notice));
            }
        }
    }

    /// Whether this instance's own `notice`, sent by the call marked `call`, is handed on: once,
    /// where that call lost its answer.
    fn awaits(&self, notice: &Notice, call: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match state
            .awaited
            .iter()
            .position(|(awaited, by)| awaited == notice && by == call)
        {
            Some(at) => {
                state.awaited.remove(at);
                true
            }
            None => false,
        }
    }

    /// The notices that were to follow the commit of a call whose answer was lost: handed on at
    /// once, each once, and not awaited, as none may be sent (clause 10). A commit of them that
    /// lands later is read at the next report that meets it, and at the next lease renewal.
    pub(crate) fn reread(&self, following: Vec<Notice>) {
        let mut handed: Vec<Notice> = Vec::new();
        for notice in following {
            if !handed.contains(&notice) {
                handed.push(notice);
            }
        }
        let on = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on
            .clone();
        if let Some(on) = on {
            for notice in handed {
                on(Heard::Notified(notice));
            }
        }
    }

    fn listen(&self, on: Arc<dyn Fn(Heard) + Send + Sync>) {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).on = Some(on);
    }
}

/// How many times a notice after its commit is sent at most.
const FOLLOW_ATTEMPTS: u32 = 4;

/// The wait before a notice after its commit that failed is sent again; each next one doubles.
const FOLLOW_RETRY_FIRST: Duration = Duration::from_millis(250);

/// How long one sending of the notices after a commit may take.
const FOLLOW_BOUND: Duration = Duration::from_secs(5);

/// What the notices after a commit of one database's calls go through: nothing but in a test,
/// which drops them, as an instance that ends between the commit and the `NOTIFY` does, or has
/// the next sendings fail.
#[derive(Default)]
pub(crate) struct Afterwards {
    #[cfg(test)]
    dropped: AtomicBool,
    #[cfg(test)]
    failing: AtomicUsize,
}

#[cfg(test)]
impl Afterwards {
    /// Whether every notice after a commit is dropped unsent from now on.
    pub(crate) fn drop_all(&self, dropped: bool) {
        self.dropped.store(dropped, Ordering::SeqCst);
    }

    /// Has the next `times` sendings of notices after a commit fail.
    pub(crate) fn fail_next(&self, times: usize) {
        self.failing.store(times, Ordering::SeqCst);
    }
}

/// Counts a notice of `kind` concerning `id` to follow the commit of the call it runs in, which
/// [`follow_commit`] sends once the call committed. Counted before the commit, so a call whose
/// answer is lost has its instance read again what it names ([`Unsure::reread`]).
pub(crate) fn will_follow(kind: &str, id: &str) {
    let _ = CALL.try_with(|call| {
        call.following
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Notice::new(kind, id));
    });
}

/// Announces what [`will_follow`] counted in this call to every other instance, now that the call
/// committed it: on `client`, the connection that committed it, in a transaction of its own that
/// does not wait for its WAL flush, so it holds PostgreSQL's turn for announcing commits only for a
/// moment (the several-instances draft, clause 9). It is sent apart from the call, which answers
/// at once: a sending that fails is logged and tried again on another connection of `pool`, and
/// after the last try the other instances take the change up at their next lease renewal.
pub(crate) fn follow_commit(client: deadpool_postgres::Object, pool: &deadpool_postgres::Pool) {
    let Ok((notices, mark, afterwards)) = CALL.try_with(|call| {
        (
            call.following
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            call.mark.clone(),
            call.afterwards.clone(),
        )
    }) else {
        return;
    };
    if notices.is_empty() {
        return;
    }
    tokio::spawn(send_following(
        client,
        pool.clone(),
        notices,
        mark,
        afterwards,
    ));
}

/// Sends `notices` as [`follow_commit`] says.
async fn send_following(
    client: deadpool_postgres::Object,
    pool: deadpool_postgres::Pool,
    notices: Vec<Notice>,
    mark: String,
    afterwards: Arc<Afterwards>,
) {
    #[cfg(test)]
    if afterwards.dropped.load(Ordering::SeqCst) {
        return;
    }
    #[cfg(not(test))]
    let _ = &afterwards;
    let mut client = Some(client);
    let mut wait = FOLLOW_RETRY_FIRST;
    for attempt in 1..=FOLLOW_ATTEMPTS {
        let sending = async {
            let mut client = match client.take() {
                Some(client) => client,
                None => super::client(&pool).await?,
            };
            #[cfg(test)]
            if afterwards
                .failing
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                return Err("a sending made to fail".to_string());
            }
            let transaction = client.transaction().await.map_err(sql)?;
            transaction
                .batch_execute("SET LOCAL synchronous_commit TO OFF")
                .await
                .map_err(sql)?;
            for notice in &notices {
                announce(&transaction, &notice.kind, &notice.id, Some(&mark)).await?;
            }
            transaction.commit().await.map_err(sql)
        };
        let error = match tokio::time::timeout(FOLLOW_BOUND, sending).await {
            Ok(Ok(())) => return,
            Ok(Err(e)) => e,
            Err(_) => format!("no answer within {} ms", FOLLOW_BOUND.as_millis()),
        };
        if attempt == FOLLOW_ATTEMPTS {
            tracing::warn!(
                error = %error,
                "could not announce a change after its commit; the other instances take it up at \
                 their next lease renewal"
            );
            return;
        }
        tracing::warn!(
            error = %error,
            attempt,
            "could not announce a change after its commit; sending it again"
        );
        tokio::time::sleep(wait).await;
        wait *= 2;
    }
}

/// Announces a change of `kind` concerning `id` to every other instance, in the transaction
/// `client` is in, so it is heard once the change is committed and never when it is not. It names
/// the origin the connection carries and the mark of the call it runs in, which counts it as sent
/// before it is sent ([`sending`]).
///
/// # Errors
/// Returns the database's error.
pub(crate) async fn notify(
    client: &impl GenericClient,
    kind: &str,
    id: &str,
) -> Result<(), String> {
    let call = CALL
        .try_with(|call| {
            call.sent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Notice::new(kind, id));
            call.mark.clone()
        })
        .ok();
    announce(client, kind, id, call.as_deref()).await
}

/// Sends one `NOTIFY` of `kind` concerning `id` in the transaction `client` is in, naming the
/// origin the connection carries and the mark `call`.
async fn announce(
    client: &impl GenericClient,
    kind: &str,
    id: &str,
    call: Option<&str>,
) -> Result<(), String> {
    client
        .execute(
            &format!(
                "SELECT pg_notify({CHANNEL}, json_build_object('kind', $1::text, 'id', $2::text, \
                 'from', current_setting('{ORIGIN_SETTING}', true), 'call', $3::text)::text)"
            ),
            &[&kind, &id, &call],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// What `payload` says, unless `origin` sent it and `unsure` awaits it no more: a payload that does
/// not read is a notice of a kind nobody knows, which counts as every notification.
fn heard(payload: &str, origin: &str, unsure: &Unsure) -> Option<Notice> {
    match serde_json::from_str::<Payload>(payload) {
        Ok(payload) => {
            let notice = Notice {
                kind: payload.kind,
                id: payload.id,
            };
            // Only this instance's own notification is matched: another's call marks count apart.
            if payload.from.as_deref() == Some(origin)
                && !payload
                    .call
                    .as_deref()
                    .is_some_and(|call| unsure.awaits(&notice, call))
            {
                return None;
            }
            Some(notice)
        }
        Err(_) => Some(Notice::new("", "")),
    }
}

impl Database {
    /// Listens on a connection of its own for as long as the database is open, and hands `on`
    /// every change another instance announced, and [`Heard::Listening`] each time the connection
    /// is made. What this instance announced itself is not handed on, but where a call of its own
    /// lost its answer: then the call's notifications are handed on at once, and each once more
    /// when it is heard ([`Unsure`]). A connection that ends is made again after a second.
    pub fn listen(&self, on: impl Fn(Heard) + Send + Sync + 'static) {
        let on: Arc<dyn Fn(Heard) + Send + Sync> = Arc::new(on);
        self.unsure.listen(on.clone());
        let (pg, tls, name) = (self.pg.clone(), self.tls.clone(), self.name.clone());
        let (closing, origin) = (self.closing.clone(), self.origin.clone());
        let unsure = self.unsure.clone();
        self.bridge.spawn(async move {
            loop {
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                let ended = match &tls {
                    Tls::Rustls(tls) => listen_once(&pg, tls.clone(), &origin, &unsure, &*on).await,
                    Tls::Plain => listen_once(&pg, NoTls, &origin, &unsure, &*on).await,
                };
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                tracing::warn!(
                    database = %name,
                    error = %ended,
                    "the connection that listens for other instances' changes ended; connecting \
                     again"
                );
                tokio::time::sleep(RECONNECT).await;
            }
        });
    }
}

/// One listening connection, until it ends: why it ended.
async fn listen_once<T>(
    pg: &tokio_postgres::Config,
    tls: T,
    origin: &str,
    unsure: &Unsure,
    on: &(dyn Fn(Heard) + Send + Sync),
) -> String
where
    T: MakeTlsConnect<Socket>,
    T::Stream: Send + 'static,
{
    let (client, mut connection) = match pg.connect(tls).await {
        Ok(connected) => connected,
        Err(e) => return sql(e),
    };
    let (heard_tx, mut hearing) = tokio::sync::mpsc::unbounded_channel();
    let driven = tokio::spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(AsyncMessage::Notification(notification)) => {
                    let _ = heard_tx.send(notification.payload().to_string());
                }
                Ok(_) => {}
                Err(e) => return sql(e),
            }
        }
        "it was closed".to_string()
    });
    let channel: String = match client.query_one(&format!("SELECT {CHANNEL}"), &[]).await {
        Ok(row) => row.get(0),
        Err(e) => {
            driven.abort();
            return sql(e);
        }
    };
    let listen = format!("LISTEN \"{}\"", channel.replace('"', "\"\""));
    if let Err(e) = client.batch_execute(&listen).await {
        driven.abort();
        return sql(e);
    }
    on(Heard::Listening);
    while let Some(payload) = hearing.recv().await {
        if let Some(notice) = heard(&payload, origin, unsure) {
            on(Heard::Notified(notice));
        }
    }
    driven
        .await
        .unwrap_or_else(|e| format!("its task ended: {e}"))
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestDatabase;
    use super::super::{client, Exclusive};
    use super::*;

    /// A change announced in a transaction is heard by every other listening instance once it is
    /// committed, with the identifier it names, and not when it is rolled back, nor by the instance
    /// that announced it, nor when it is announced in another schema of the same database; each
    /// listener hears that it listens first.
    /// Verifies: ADR-0041
    #[test]
    fn a_committed_change_is_heard_by_every_other_instance() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let open = || {
            Database::open_in(&test.config(), Exclusive::Shared, Some(&test.schema)).expect("open")
        };
        let (a, b) = (open(), open());
        let (heard, hearing) = std::sync::mpsc::channel();
        b.listen(move |what| {
            let _ = heard.send(what);
        });
        assert_eq!(
            hearing.recv_timeout(Duration::from_secs(10)),
            Ok(Heard::Listening)
        );
        let elsewhere = TestDatabase::named().expect("the same database");
        Database::open_in(
            &elsewhere.config(),
            Exclusive::Shared,
            Some(&elsewhere.schema),
        )
        .expect("another schema")
        .run(|pool| async move {
            let client = client(&pool).await?;
            notify(&**client, "elsewhere", "").await
        })
        .expect("notify in another schema");
        let announce = |on: &Database, kind: &'static str, commit: bool| {
            on.run(move |pool| async move {
                let mut client = client(&pool).await?;
                let transaction = client.transaction().await.map_err(sql)?;
                notify(&*transaction, kind, "the-id").await?;
                if commit {
                    transaction.commit().await.map_err(sql)?;
                }
                Ok(())
            })
            .expect("notify");
        };
        announce(&a, "dropped", false);
        announce(&b, "own", true);
        announce(&a, "kept", true);
        assert_eq!(
            hearing.recv_timeout(Duration::from_secs(10)),
            Ok(Heard::Notified(Notice::new("kept", "the-id")))
        );
    }

    /// A payload names its sender; one this instance sent is not handed on, and one that does not
    /// read is a notice of a kind nobody knows.
    /// Verifies: ADR-0041
    #[test]
    fn a_notice_from_this_instance_is_passed_over_and_an_unreadable_one_is_unknown() {
        let sent = r#"{"kind":"floor","id":"otelcol","from":"a"}"#;
        let none = Unsure::default();
        assert_eq!(heard(sent, "a", &none), None);
        assert_eq!(
            heard(sent, "b", &none),
            Some(Notice::new("floor", "otelcol"))
        );
        assert_eq!(
            heard(r#"{"kind":"floor","id":"otelcol","from":null}"#, "a", &none),
            Some(Notice::new("floor", "otelcol"))
        );
        assert_eq!(heard("floor", "a", &none), Some(Notice::new("", "")));
    }

    /// The notifications of a call whose answer was lost are handed on at once, each once, and this
    /// instance's own notification of that call is handed on once more when it is heard, and only
    /// once: a commit that landed before the call failed and one that lands after it are both read.
    /// The same notification of an earlier call of its own, or of another instance's call that
    /// bears the same mark, is not taken for it.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    #[test]
    fn the_notifications_of_a_call_whose_answer_was_lost_are_handed_on_and_heard_once_more() {
        let unsure = Unsure::default();
        let handed = Arc::new(Mutex::new(Vec::new()));
        unsure.listen({
            let handed = handed.clone();
            Arc::new(move |what| handed.lock().expect("handed").push(what))
        });
        let own = r#"{"kind":"labels","id":"x","from":"a","call":"2"}"#;
        assert_eq!(heard(own, "a", &unsure), None, "a call that answered");
        unsure.lost(
            vec![Notice::new("labels", "x"), Notice::new("labels", "x")],
            "2",
        );
        assert_eq!(
            *handed.lock().expect("handed"),
            [Heard::Notified(Notice::new("labels", "x"))],
            "handed on at once, once"
        );
        let other = r#"{"kind":"floor","id":"x","from":"a","call":"2"}"#;
        assert_eq!(heard(other, "a", &unsure), None, "only what the call sent");
        let earlier = r#"{"kind":"labels","id":"x","from":"a","call":"1"}"#;
        assert_eq!(heard(earlier, "a", &unsure), None, "not an earlier call's");
        let unmarked = r#"{"kind":"labels","id":"x","from":"a"}"#;
        assert_eq!(heard(unmarked, "a", &unsure), None, "nor one no call marks");
        let elsewhere = r#"{"kind":"labels","id":"x","from":"b","call":"2"}"#;
        assert_eq!(
            heard(elsewhere, "a", &unsure),
            Some(Notice::new("labels", "x")),
            "another instance's is heard as ever"
        );
        assert_eq!(
            heard(own, "a", &unsure),
            Some(Notice::new("labels", "x")),
            "and does not use up this one's: heard once more"
        );
        assert_eq!(heard(own, "a", &unsure), None, "and once only");
        for _ in 0..=AWAITED_AT_MOST {
            unsure.lost(vec![Notice::new("floor", "y")], "3");
        }
        let awaited = unsure.state.lock().expect("state").awaited.len();
        assert_eq!(awaited, AWAITED_AT_MOST, "bounded");
    }

    /// The notices that were to follow the commit of a call whose answer was lost are handed on at
    /// once, each once, and not awaited: this instance's own notice of the same kind and
    /// identifier, should one come, is passed over (clause 10).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    #[test]
    fn the_notices_to_follow_a_commit_whose_answer_was_lost_are_read_again_and_not_awaited() {
        let unsure = Unsure::default();
        let handed = Arc::new(Mutex::new(Vec::new()));
        unsure.listen({
            let handed = handed.clone();
            Arc::new(move |what| handed.lock().expect("handed").push(what))
        });
        unsure.reread(vec![Notice::new(HOST, "h"), Notice::new(HOST, "h")]);
        assert_eq!(
            *handed.lock().expect("handed"),
            [Heard::Notified(Notice::new(HOST, "h"))],
            "handed on at once, once"
        );
        assert!(
            unsure.state.lock().expect("state").awaited.is_empty(),
            "nothing awaited"
        );
        let own = r#"{"kind":"host","id":"h","from":"a","call":"5"}"#;
        assert_eq!(heard(own, "a", &unsure), None, "its own notice passed over");
    }

    /// A call that fails after it committed a change whose notice was to follow the commit has its
    /// instance read what the notice names at once, as on hearing it (clause 10).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    #[test]
    fn a_call_that_fails_after_its_commit_has_the_notice_to_follow_it_read_at_once() {
        let Some(test) = TestDatabase::named() else {
            return;
        };
        let database =
            Database::open_in(&test.config(), Exclusive::Shared, Some(&test.schema)).expect("open");
        let (heard, hearing) = std::sync::mpsc::channel();
        database.listen(move |what| {
            let _ = heard.send(what);
        });
        assert_eq!(
            hearing.recv_timeout(Duration::from_secs(10)),
            Ok(Heard::Listening)
        );
        let failed = database.run(|pool| async move {
            let mut client = client(&pool).await?;
            let transaction = client.transaction().await.map_err(sql)?;
            will_follow(HOST, "h");
            transaction.commit().await.map_err(sql)?;
            Err::<(), String>("the answer was lost".to_string())
        });
        assert!(failed.is_err());
        assert_eq!(
            hearing.recv_timeout(Duration::from_secs(1)),
            Ok(Heard::Notified(Notice::new(HOST, "h")))
        );
        assert!(
            database
                .unsure
                .state
                .lock()
                .expect("state")
                .awaited
                .is_empty(),
            "nothing awaited"
        );
    }

    /// A notification that names the call that sent it is read by a Server of 0.8.0 as one without:
    /// its payload, as 0.8.0 declares it, reads past a field it does not know (clause 16).
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
    #[test]
    fn a_notification_that_names_its_call_is_read_by_the_release_before() {
        /// The payload as version/0.8.0 declares it in `postgres/notify.rs`.
        #[derive(Deserialize)]
        struct Payload080 {
            kind: String,
            #[serde(default)]
            id: String,
            #[serde(default)]
            from: Option<String>,
        }
        let read: Payload080 =
            serde_json::from_str(r#"{"kind":"labels","id":"x","from":"a","call":"7"}"#)
                .expect("read by 0.8.0");
        assert_eq!(
            (read.kind.as_str(), read.id.as_str(), read.from.as_deref()),
            ("labels", "x", Some("a"))
        );
    }
}
