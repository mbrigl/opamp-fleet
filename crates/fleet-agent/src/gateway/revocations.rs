//! The one refusal a Gateway makes beyond its handshake: a downstream certificate its Server has
//! revoked (ADR-0055 clause 14).
//!
//! The Server never sees a downstream certificate — the Gateway terminates that handshake and
//! presents its own upstream — so the Server hands its Gateways the revoked certificates of the
//! fleet's client CA instead, chains already resolved (ADR-0056 clause 12). The Gateway fetches the
//! list over its own upstream origin, with its own certificate and credential, keeps it in memory,
//! and refuses what it names. While it holds no list young enough to trust, it admits nobody: a
//! Gateway whose Server is away forwards nothing anyway, and a list kept for ever would admit a
//! certificate revoked in the meantime.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;
use sha2::Digest as _;
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::shutdown::Shutdown;

/// How often the list is fetched.
pub const REVOCATION_REFRESH: Duration = Duration::from_secs(30);

/// How old the list may grow before the Gateway admits nobody.
pub const REVOCATION_MAX_AGE: Duration = Duration::from_secs(300);

/// How long a starting Gateway waits for its first list before it serves without one.
pub const REVOCATION_FIRST_WAIT: Duration = Duration::from_secs(5);

/// Where the Server serves the list, on its Agent plane.
const PATH: &str = "/v1/gateway/revocations";

/// What the list says about one downstream certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Admit,
    Revoked,
    /// No list fetched yet, or none within the maximum age.
    Stale,
}

impl Verdict {
    /// Why a session is ended, for the close frame.
    #[must_use]
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Verdict::Admit => None,
            Verdict::Revoked => Some("revoked"),
            Verdict::Stale => Some("revocation list stale"),
        }
    }
}

#[derive(Default)]
struct Held {
    fetched: Option<Instant>,
    /// Each revoked certificate by the SHA-256 of its issuer's DER name and its serial, as the
    /// Server's register names it.
    revoked: HashSet<(String, String)>,
    etag: Option<String>,
}

/// The list as this Gateway holds it, shared by every downstream connection.
#[derive(Clone)]
pub struct RevocationList {
    held: Arc<RwLock<Held>>,
    changes: Arc<watch::Sender<u64>>,
    max_age: Duration,
}

impl RevocationList {
    #[must_use]
    pub fn new(max_age: Duration) -> Self {
        RevocationList {
            held: Arc::new(RwLock::new(Held::default())),
            changes: Arc::new(watch::Sender::new(0)),
            max_age,
        }
    }

    /// What the list says about the certificate `der`, a downstream peer's.
    #[must_use]
    pub fn verdict(&self, der: &[u8]) -> Verdict {
        let held = self.held.read().expect("revocation list lock");
        if !held
            .fetched
            .is_some_and(|fetched| fetched.elapsed() <= self.max_age)
        {
            return Verdict::Stale;
        }
        match certificate_id(der) {
            Some(id) if !held.revoked.contains(&id) => Verdict::Admit,
            // One the handshake verified always parses; one that does not is not admitted.
            _ => Verdict::Revoked,
        }
    }

    /// Fires whenever a session should judge itself again: after every fetch, successful or not,
    /// so that a list going stale ends the sessions too.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn replace(&self, revoked: HashSet<(String, String)>, etag: Option<String>) {
        let mut held = self.held.write().expect("revocation list lock");
        held.revoked = revoked;
        held.etag = etag;
        held.fetched = Some(Instant::now());
    }

    fn confirm(&self) {
        self.held.write().expect("revocation list lock").fetched = Some(Instant::now());
    }

    fn etag(&self) -> Option<String> {
        self.held.read().expect("revocation list lock").etag.clone()
    }

    fn announce(&self) {
        self.changes.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// A certificate as the Server's register names it: the SHA-256 of its issuer's DER-encoded name,
/// and its serial in lowercase hex without leading zeros.
fn certificate_id(der: &[u8]) -> Option<(String, String)> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let issuer = hex::encode(sha2::Sha256::digest(cert.issuer().as_raw()));
    let serial = hex::encode(cert.raw_serial());
    let serial = serial.trim_start_matches('0');
    Some((
        issuer,
        if serial.is_empty() { "0" } else { serial }.to_string(),
    ))
}

#[derive(Deserialize)]
struct ListBody {
    certificates: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    issuer: String,
    serial: String,
}

/// One attempt to bring `list` up to date, and every session told to judge itself again. What the
/// Gateway awaits before it serves, so it does not refuse its first peers for want of a list.
pub async fn update(list: &RevocationList, config: &ClientConfig) {
    match fetch(list, config).await {
        Ok(Some(count)) => info!(revoked = count, "revocation list fetched"),
        Ok(None) => {}
        Err(e) => warn!(error = %e, "cannot fetch the revocation list"),
    }
    list.announce();
}

/// Keeps `list` fresh until `shutdown` fires, one [`update`] every `every`.
pub async fn refresh(
    list: RevocationList,
    config: Arc<ClientConfig>,
    mut shutdown: Shutdown,
    every: Duration,
) {
    let mut tick = tokio::time::interval_at(Instant::now() + every, every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {}
            () = shutdown.requested() => return,
        }
        update(&list, &config).await;
    }
}

/// One fetch: `Ok(Some(n))` for a new list of `n` certificates, `Ok(None)` when it is unchanged.
async fn fetch(list: &RevocationList, config: &ClientConfig) -> Result<Option<usize>, String> {
    let url = crate::packages::resolve_url(PATH, &config.endpoint)?;
    let builder = reqwest::Client::builder()
        .use_rustls_tls()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30));
    let client = crate::tls::client_tls(config)?
        .apply(builder)?
        .build()
        .map_err(|e| format!("cannot build the client: {e}"))?;
    let mut request = client.get(&url);
    if let Some(authorization) = config.authorization_value()? {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }
    if let Some(etag) = list.etag() {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    let response = request.send().await.map_err(|e| format!("{url}: {e}"))?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        list.confirm();
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body: ListBody = response
        .json()
        .await
        .map_err(|e| format!("{url}: an unreadable list: {e}"))?;
    let revoked: HashSet<(String, String)> = body
        .certificates
        .into_iter()
        .map(|entry| (entry.issuer, entry.serial))
        .collect();
    let count = revoked.len();
    list.replace(revoked, etag);
    Ok(Some(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate() -> Vec<u8> {
        let key = rcgen::KeyPair::generate().expect("key");
        rcgen::CertificateParams::new(vec!["edge".into()])
            .expect("params")
            .self_signed(&key)
            .expect("cert")
            .der()
            .to_vec()
    }

    /// A Gateway with no list yet, or one past its age, admits nobody; with a fresh list it admits
    /// what the list does not name and refuses what it does.
    /// Verifies: ADR-0055
    #[tokio::test(start_paused = true)]
    async fn the_verdict_follows_the_list_and_its_age() {
        let cert = certificate();
        let list = RevocationList::new(Duration::from_secs(300));
        assert_eq!(list.verdict(&cert), Verdict::Stale, "no list yet");

        list.replace(HashSet::new(), None);
        assert_eq!(list.verdict(&cert), Verdict::Admit);

        list.replace(HashSet::from([certificate_id(&cert).expect("id")]), None);
        assert_eq!(list.verdict(&cert), Verdict::Revoked);

        list.replace(HashSet::new(), None);
        tokio::time::advance(Duration::from_secs(301)).await;
        assert_eq!(list.verdict(&cert), Verdict::Stale, "too old");
        list.confirm();
        assert_eq!(
            list.verdict(&cert),
            Verdict::Admit,
            "an unchanged answer renews it"
        );
    }
}
