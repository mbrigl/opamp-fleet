//! The notifications the instances on one database share
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
//! clause 9): one channel per schema, a `NOTIFY` that names a kind of change and nothing else,
//! sent in the transaction that commits the change, and a connection of each instance's own that
//! listens. A channel is the database's, not the schema's, so its name carries the schema: two
//! fleets in two schemas of one database never hear each other.

use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::{AsyncMessage, GenericClient, NoTls, Socket};

use super::{sql, Database, Tls};

/// The channel every instance on a schema listens on, as SQL: its name and the schema's.
const CHANNEL: &str = "'opamp_fleet:' || coalesce(current_schema(), '')";

/// The kind a revocation added or lifted is announced as.
pub const REVOCATION: &str = "revocation";

/// The kind a package floor set or removed is announced as.
pub const FLOOR: &str = "floor";

/// The kind a register entry written or removed is announced as.
pub const CERTIFICATE: &str = "certificate";

/// The kind a Host written or removed, or an `instance_uid` bound or released, is announced as.
pub const HOST: &str = "host";

/// How long a listener waits before it connects again after its connection ended.
const RECONNECT: Duration = Duration::from_secs(1);

/// What a listener hears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heard {
    /// A change of this kind was committed, by this instance or another.
    Notified(String),
    /// The listening connection was made, the first time or again after it ended: whatever was
    /// committed in between was not heard.
    Listening,
}

/// Announces a change of `kind` to every instance, in the transaction `client` is in, so it is
/// heard once the change is committed and never when it is not.
///
/// # Errors
/// Returns the database's error.
pub(crate) async fn notify(client: &impl GenericClient, kind: &str) -> Result<(), String> {
    client
        .execute(&format!("SELECT pg_notify({CHANNEL}, $1)"), &[&kind])
        .await
        .map_err(sql)?;
    Ok(())
}

impl Database {
    /// Listens on a connection of its own for as long as the database is open, and hands `on`
    /// every kind of change announced, and [`Heard::Listening`] each time the connection is made.
    /// A connection that ends is made again after a second.
    pub fn listen(&self, on: impl Fn(Heard) + Send + Sync + 'static) {
        let (pg, tls, name) = (self.pg.clone(), self.tls.clone(), self.name.clone());
        let closing = self.closing.clone();
        self.bridge.spawn(async move {
            loop {
                if closing.load(Ordering::SeqCst) {
                    return;
                }
                let ended = match &tls {
                    Tls::Rustls(tls) => listen_once(&pg, tls.clone(), &on).await,
                    Tls::Plain => listen_once(&pg, NoTls, &on).await,
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
async fn listen_once<T>(pg: &tokio_postgres::Config, tls: T, on: &impl Fn(Heard)) -> String
where
    T: MakeTlsConnect<Socket>,
    T::Stream: Send + 'static,
{
    let (client, mut connection) = match pg.connect(tls).await {
        Ok(connected) => connected,
        Err(e) => return sql(e),
    };
    let (heard, mut hearing) = tokio::sync::mpsc::unbounded_channel();
    let driven = tokio::spawn(async move {
        let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
        while let Some(message) = messages.next().await {
            match message {
                Ok(AsyncMessage::Notification(notification)) => {
                    let _ = heard.send(notification.payload().to_string());
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
    while let Some(kind) = hearing.recv().await {
        on(Heard::Notified(kind));
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

    /// A change announced in a transaction is heard by every listening instance once it is
    /// committed, and not when it is rolled back, nor when it is announced in another schema of
    /// the same database; each listener hears that it listens first.
    /// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
    #[test]
    fn a_committed_change_is_heard_by_every_instance() {
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
            notify(&**client, "elsewhere").await
        })
        .expect("notify in another schema");
        for commit in [false, true] {
            a.run(move |pool| async move {
                let mut client = client(&pool).await?;
                let transaction = client.transaction().await.map_err(sql)?;
                notify(&*transaction, if commit { "kept" } else { "dropped" }).await?;
                if commit {
                    transaction.commit().await.map_err(sql)?;
                }
                Ok(())
            })
            .expect("notify");
        }
        assert_eq!(
            hearing.recv_timeout(Duration::from_secs(10)),
            Ok(Heard::Notified("kept".to_string()))
        );
    }
}
