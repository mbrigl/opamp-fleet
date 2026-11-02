//! The notifications the instances on one database share
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clause 9): one channel per schema, a `NOTIFY` that names a kind of change, the identifier it
//! concerns and the instance that sent it, and nothing else, sent in the transaction that commits
//! the change, and a connection of each instance's own that listens. A channel is the database's,
//! not the schema's, so its name carries the schema: two fleets in two schemas of one database
//! never hear each other.
//!
//! Every connection of an instance carries the instance's origin as the session setting
//! [`ORIGIN_SETTING`], so a notification names its sender whichever store sends it, and the
//! listener hands on none that its own instance sent: that instance holds what it committed
//! already.

use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, GenericClient, NoTls, Socket};

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

/// Announces a change of `kind` concerning `id` to every other instance, in the transaction
/// `client` is in, so it is heard once the change is committed and never when it is not. It names
/// the origin the connection carries.
///
/// # Errors
/// Returns the database's error.
pub(crate) async fn notify(
    client: &impl GenericClient,
    kind: &str,
    id: &str,
) -> Result<(), String> {
    client
        .execute(
            &format!(
                "SELECT pg_notify({CHANNEL}, json_build_object('kind', $1::text, 'id', $2::text, \
                 'from', current_setting('{ORIGIN_SETTING}', true))::text)"
            ),
            &[&kind, &id],
        )
        .await
        .map_err(sql)?;
    Ok(())
}

/// What `payload` says, unless `origin` sent it: a payload that does not read is a notice of a
/// kind nobody knows, which counts as every notification.
fn heard(payload: &str, origin: &str) -> Option<Notice> {
    match serde_json::from_str::<Payload>(payload) {
        Ok(payload) if payload.from.as_deref() == Some(origin) => None,
        Ok(payload) => Some(Notice {
            kind: payload.kind,
            id: payload.id,
        }),
        Err(_) => Some(Notice::new("", "")),
    }
}

impl Database {
    /// Listens on a connection of its own for as long as the database is open, and hands `on`
    /// every change another instance announced, and [`Heard::Listening`] each time the connection
    /// is made. What this instance announced itself is not handed on. A connection that ends is
    /// made again after a second.
    pub fn listen(&self, on: impl Fn(Heard) + Send + Sync + 'static) {
        let (pg, tls, name) = (self.pg.clone(), self.tls.clone(), self.name.clone());
        let (closing, origin) = (self.closing.clone(), self.origin.clone());
        self.bridge.spawn(async move {
            loop {
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                let ended = match &tls {
                    Tls::Rustls(tls) => listen_once(&pg, tls.clone(), &origin, &on).await,
                    Tls::Plain => listen_once(&pg, NoTls, &origin, &on).await,
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
    on: &impl Fn(Heard),
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
        if let Some(notice) = heard(&payload, origin) {
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
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_notice_from_this_instance_is_passed_over_and_an_unreadable_one_is_unknown() {
        let sent = r#"{"kind":"floor","id":"otelcol","from":"a"}"#;
        assert_eq!(heard(sent, "a"), None);
        assert_eq!(heard(sent, "b"), Some(Notice::new("floor", "otelcol")));
        assert_eq!(
            heard(r#"{"kind":"floor","id":"otelcol","from":null}"#, "a"),
            Some(Notice::new("floor", "otelcol"))
        );
        assert_eq!(heard("floor", "a"), Some(Notice::new("", "")));
    }
}
