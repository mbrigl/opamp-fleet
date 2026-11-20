//! The Agent plane's use cases (SRV-two-use-case-layers §1): every operation the Agent plane
//! performs — admission, a report, a certificate request, an enrolment, a download, and the
//! Gateways' read of the revocation list — as the methods of one type, [`AgentCases`]. `transport`
//! carries the protocol and calls these methods and nothing else that holds state
//! (SRV-two-use-case-layers §2); the core's values cross unchanged (SRV-two-use-case-layers §3).
//!
//! What a peer must prove is [`Admission`]; the root builds it and hands it to [`AgentCases`] with
//! the fleet's state.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opamp::proto::{AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::agent_rate::{Bucket, Subject};
use crate::audit::{Audit, Entry};
use crate::enrolment::{Enrolment, Requester, Submitted};
use crate::fleet::{bad_request, unavailable, AppState, ConnId, Transport};
use crate::packages::{PackageId, Platform};
use crate::revocation::{CertId, Presented, Revocations};
use crate::throttle::Throttle;
use crate::tls::{Issuers, Peer};

/// What a peer must prove to reach `/v1/opamp` (SEC-admission-by-client-certificate §1 and §6): a client certificate the
/// handshake verified, not revoked — the whole of admission. No `Authorization` header is read; one
/// that is sent is ignored, never refused, and no refusal carries a `WWW-Authenticate` challenge. A
/// certificate from the bootstrap CA admits an enrolling host, and only while an operator holds the
/// enrolment window open. Repeated failures from one peer address are throttled.
#[derive(Default)]
pub struct Admission {
    /// The connection must have carried a certificate. The certificate itself is already verified
    /// — rustls refuses one it cannot chain — so this is a presence check, never a second
    /// verification.
    require_client_certificate: bool,
    pub(crate) issuers: Issuers,
    enrolment: Option<Arc<Enrolment>>,
    throttle: Option<Arc<Throttle>>,
    revocations: Option<Arc<Revocations>>,
    audit: Option<Arc<dyn Audit>>,
    /// When a plain-HTTP peer presenting a certificate was last recorded as admitted.
    admitted: Mutex<HashMap<(IpAddr, Option<String>), Instant>>,
}

/// How often a plain-HTTP peer's admission is recorded: once per address and certificate per
/// hour, so a poll every few seconds does not crowd the record (SEC-append-only-audit-record §1).
const ADMITTED_EVERY: Duration = Duration::from_secs(3600);

/// The plain-HTTP peers remembered as recorded at most.
const ADMITTED_MAX: usize = 100_000;

/// WebSocket session ends when it is revoked, or when it expires.
#[derive(Clone, Debug, Default)]
pub struct Proofs {
    pub certificate: Option<(CertId, u64)>,
    /// The certificate itself (DER), which a bootstrap proof naming it must embed byte for byte
    /// (CLT-modes-and-gateway §25).
    pub der: Option<Arc<[u8]>>,
    /// The host the certificate was issued to, when it names one (SEC-bootstrap-certificates-and-enrolment §11).
    pub host: Option<String>,
    /// The certificate's subject values, which a Host made at its first report carries
    /// (SRV-fleet-record-agents-hosts §15).
    pub subject_values: crate::revocation::SubjectValues,
}

impl Proofs {
    fn revoked(&self, revocations: &Revocations) -> bool {
        self.certificate
            .as_ref()
            .is_some_and(|(id, _)| revocations.is_certificate_revoked(id))
    }

    /// Who a member's messages and downloads are counted for (CLT-modes-and-gateway §26): the host its
    /// certificate names, else the certificate, else — with no certificate at all — the peer.
    fn subject(&self, peer: Option<IpAddr>) -> Subject {
        match (&self.host, &self.certificate) {
            (Some(host), _) => Subject::Host(host.clone()),
            (None, Some((id, _))) => Subject::Certificate(id.clone()),
            (None, None) => Subject::peer(peer),
        }
    }

    /// The refusal of a message or a download past the limit (SEC-tls13-two-planes-bounded-input §29): the host, or the
    /// certificate's serial and the role of the CA that issued it.
    fn throttled_entry(
        &self,
        peer: Option<IpAddr>,
        revocations: Option<&Arc<Revocations>>,
        bucket: Bucket,
    ) -> Entry {
        let entry = Entry::new("agent_rate.throttled", "throttled")
            .peer(peer)
            .with("plane", "agent")
            .with("bucket", bucket.as_str());
        match (&self.host, &self.certificate) {
            (Some(host), _) => entry.with("host", host.clone()),
            (None, Some((id, _))) => entry.with("serial", id.serial.clone()).with(
                "authority",
                revocations
                    .and_then(|revocations| revocations.authority_of(&id.issuer))
                    .map(|authority| authority.role.clone()),
            ),
            (None, None) => entry,
        }
    }
}

/// Whether `subject` is a host marked as a Gateway, read at each message (CLT-modes-and-gateway §26).
fn is_gateway(subject: &Subject, revocations: Option<&Arc<Revocations>>) -> bool {
    match (subject, revocations) {
        (Subject::Host(host), Some(revocations)) => revocations.is_gateway(host),
        _ => false,
    }
}

/// What a message past the limit is answered (SEC-tls13-two-planes-bounded-input §25).
const THROTTLED: &str = "too many messages arrive from this sender — retry later";

impl Admission {
    /// No proof required — what a test that is not about admission serves with. A Server never
    /// does: its configuration requires the client CA (SEC-admission-by-client-certificate).
    pub fn open() -> Self {
        Admission::default()
    }

    pub fn new(require_client_certificate: bool) -> Self {
        Admission {
            require_client_certificate,
            ..Admission::default()
        }
    }

    /// Tells a member's certificate from an enrolling host's, and admits the latter only while
    /// `enrolment`'s window is open.
    #[must_use]
    pub fn with_enrolment(mut self, issuers: Issuers, enrolment: Option<Arc<Enrolment>>) -> Self {
        self.issuers = issuers;
        self.enrolment = enrolment;
        self
    }

    /// Throttles repeated failures from one peer address (SEC-admission-by-client-certificate §19).
    #[must_use]
    pub fn with_throttle(mut self, throttle: Arc<Throttle>) -> Self {
        self.throttle = Some(throttle);
        self
    }

    /// Records every decision in `audit` (SEC-append-only-audit-record).
    #[must_use]
    pub fn with_audit(mut self, audit: Option<Arc<dyn Audit>>) -> Self {
        self.audit = audit;
        self
    }

    /// `client` or `bootstrap` for a certificate one of this Server's CAs issued.
    fn issuers_role(&self, issuer: &str) -> Option<String> {
        self.revocations
            .as_ref()
            .and_then(|revocations| revocations.authority_of(issuer))
            .map(|authority| authority.role.clone())
    }

    /// Whether this admission is recorded: every WebSocket session, and a plain-HTTP peer once per
    /// address — an IPv6 one by its /64 — and certificate per [`ADMITTED_EVERY`].
    fn worth_recording(&self, websocket: bool, peer: Option<IpAddr>, serial: Option<&str>) -> bool {
        let (false, Some(peer)) = (websocket, peer) else {
            return true;
        };
        let key = (crate::throttle::peer_key(peer), serial.map(str::to_string));
        self.admitted
            .lock()
            .expect("admitted lock")
            .get(&key)
            .is_none_or(|at| at.elapsed() >= ADMITTED_EVERY)
    }

    /// Notes that a plain-HTTP peer's admission was recorded — only once it was, so a peer
    /// refused for want of a record is recorded when it is admitted.
    fn recorded(&self, peer: Option<IpAddr>, serial: Option<&str>) {
        let Some(peer) = peer else {
            return;
        };
        let mut admitted = self.admitted.lock().expect("admitted lock");
        if admitted.len() >= ADMITTED_MAX {
            admitted.retain(|_, at| at.elapsed() < ADMITTED_EVERY);
            if admitted.len() >= ADMITTED_MAX {
                // Forgetting only means recording again; it never means admitting unrecorded.
                admitted.clear();
            }
        }
        admitted.insert(
            (crate::throttle::peer_key(peer), serial.map(str::to_string)),
            Instant::now(),
        );
    }

    /// Refuses a revoked certificate (SEC-admission-by-client-certificate §31).
    #[must_use]
    pub fn with_revocations(mut self, revocations: Option<Arc<Revocations>>) -> Self {
        self.revocations = revocations;
        self
    }

    fn required(&self) -> bool {
        self.require_client_certificate || self.throttle.is_some() || self.revocations.is_some()
    }
}

/// Milliseconds since the Unix epoch, on the host's clock.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Whether a certificate whose `not_after` is `not_after_ms` has expired: from that instant on it
/// admits nothing, as it ends a WebSocket session then (SEC-admission-by-client-certificate §33).
fn expired(not_after_ms: u64) -> bool {
    not_after_ms <= now_ms()
}

/// The check of [`checked`] a presented certificate failed.
#[derive(Debug)]
enum Failed {
    Unreadable,
    Expired,
    Revoked { serial: String },
}

/// What `/v1/opamp` and the package download alike hold a presented certificate (DER) to, beyond
/// the handshake (SEC-admission-by-client-certificate §6, §18, §31 and §33): its facts read, it has not ended, and it is not
/// revoked. Its facts, read once, or `None` where no certificate was presented.
fn checked(
    certificate: Option<&[u8]>,
    revocations: Option<&Revocations>,
) -> Result<Option<crate::revocation::Facts>, Failed> {
    let Some(certificate) = certificate else {
        return Ok(None);
    };
    // The handshake verified it, so it parses; one whose facts do not read, such as a serial past
    // 20 octets, is not admitted.
    let facts = crate::ca::facts(certificate).map_err(|_| Failed::Unreadable)?;
    // The handshake checked the certificate's validity once per connection; a kept-alive one
    // outlives it, so each request is checked again.
    if expired(facts.not_after_ms) {
        return Err(Failed::Expired);
    }
    if revocations.is_some_and(|revocations| revocations.is_certificate_revoked(&facts.id)) {
        return Err(Failed::Revoked {
            serial: facts.id.serial,
        });
    }
    Ok(Some(facts))
}

/// The Agent plane's operations over the fleet's state (SRV-two-use-case-layers §1). The root
/// builds one with the plane's [`Admission`] and hands it to `transport`, which reaches state
/// through it alone.
pub struct AgentCases {
    state: Arc<AppState>,
    admission: Admission,
}

/// A message or a push while this instance holds no lease it can prove
/// (SRV-instances-share-one-postgresql §3).
#[derive(Debug)]
pub(crate) struct NoLease;

/// Why a peer is not admitted to `/v1/opamp` (SEC-admission-by-client-certificate §1, §6 and §19).
#[derive(Debug)]
pub(crate) enum Refused {
    /// The peer address is in back-off for this many seconds.
    Throttled(u64),
    /// No client certificate where one is required.
    NoCertificate,
    /// A certificate that does not admit: unreadable, expired, or revoked — which is not said.
    NotOfTheFleet,
    /// A bootstrap certificate outside an enrolment window.
    NoEnrolmentWindow,
    /// The audit record is unavailable, and nothing is admitted without its record.
    Busy,
}

/// Why a download is not admitted (SEC-admission-by-client-certificate §18 and
/// SEC-tls13-two-planes-bounded-input §26).
#[derive(Debug)]
pub(crate) enum DownloadRefused {
    /// The peer address is in back-off for this many seconds.
    Throttled(u64),
    /// No certificate of the fleet's client CA.
    NotOfTheFleet,
    /// Past the sender's rate.
    OverRate,
    /// The audit record is unavailable.
    Busy,
}

/// Why an artifact is not served.
#[derive(Debug)]
pub(crate) enum ArtifactRefused {
    /// One answer for every artifact this requester may not fetch, whether or not the store holds
    /// it (PKG-signed-packages-and-delivery §37).
    NotFound,
    /// The stored artifact cannot be opened.
    Unreadable,
}

impl AgentCases {
    #[must_use]
    pub fn new(state: Arc<AppState>, admission: Admission) -> Self {
        AgentCases { state, admission }
    }

    /// The largest message either transport takes or sends.
    pub(crate) fn max_message_size(&self) -> usize {
        self.state.max_message_size()
    }

    /// Whether `/v1/opamp` is guarded at all; only a test serves without.
    pub(crate) fn requires_admission(&self) -> bool {
        self.admission.required()
    }

    /// Whether the download route is guarded at all; only a test serves without.
    pub(crate) fn guards_downloads(&self) -> bool {
        self.admission.require_client_certificate
            || self.admission.throttle.is_some()
            || self.state.agent_rate().is_some()
    }

    /// Admits a peer to `/v1/opamp`, or says why not (SEC-admission-by-client-certificate). What
    /// this proves is *fleet membership* and the host, not which Agent is speaking:
    /// `instance_uid` stays self-asserted behind the certificate
    /// (SEC-bootstrap-certificates-and-enrolment §11 and SEC-admission-by-client-certificate §12).
    /// Admission is the trust boundary; the host is the only bound between admitted Agents.
    pub(crate) fn admit(
        &self,
        peer: Option<IpAddr>,
        websocket: bool,
        certificate: Option<&[u8]>,
    ) -> Result<(Peer, Proofs), Refused> {
        let admission = &self.admission;
        let record_refusal = |event: &str, outcome: &str, check: &str| {
            if let Some(audit) = &admission.audit {
                audit.refusal(
                    Entry::new(event, outcome)
                        .peer(peer)
                        .with("plane", "agent")
                        .with("check", check),
                );
            }
        };
        if let (Some(throttle), Some(peer)) = (&admission.throttle, peer) {
            if let Some(wait) = throttle.retry_after(peer) {
                record_refusal("admission.throttled", "throttled", "throttle");
                return Err(Refused::Throttled(wait));
            }
        }
        let refuse = |check: &str, refused: Refused| {
            if let (Some(throttle), Some(peer)) = (&admission.throttle, peer) {
                throttle.failed(peer);
            }
            record_refusal("admission.refused", "refused", check);
            refused
        };
        if admission.require_client_certificate && certificate.is_none() {
            debug!("refused: the OpAMP endpoint requires a client certificate");
            return Err(refuse("certificate", Refused::NoCertificate));
        }
        let presented = match checked(certificate, admission.revocations.as_deref()) {
            Ok(presented) => presented,
            Err(Failed::Unreadable) => return Err(refuse("certificate", Refused::NotOfTheFleet)),
            Err(Failed::Expired) => {
                debug!("refused: an expired certificate");
                return Err(refuse("expired", Refused::NotOfTheFleet));
            }
            // That the certificate was revoked is not said (SEC-admission-by-client-certificate §31).
            Err(Failed::Revoked { serial }) => {
                debug!("refused: a revoked certificate");
                if let Some(audit) = &admission.audit {
                    audit.refusal(
                        Entry::new("admission.revoked_proof", "refused")
                            .peer(peer)
                            .with("serial", Some(serial)),
                    );
                }
                return Err(refuse("revoked", Refused::NotOfTheFleet));
            }
        };
        let classified = certificate
            .map(|cert| admission.issuers.classify(cert))
            .unwrap_or(Peer::Member);
        if let Peer::Enrolling { .. } = &classified {
            // A bootstrap certificate opens nothing outside an enrolment window (SEC-bootstrap-certificates-and-enrolment §9).
            // The handshake proved the certificate, so this is no guess and counts as no failure:
            // hosts that wait for an operator must not throttle the members behind the same address.
            if !admission.enrolment.as_ref().is_some_and(|e| e.is_open()) {
                debug!("refused: a bootstrap certificate outside an enrolment window");
                record_refusal("admission.refused", "refused", "enrolment_window");
                return Err(Refused::NoEnrolmentWindow);
            }
        }
        if let Some(audit) = &admission.audit {
            let serial = presented.as_ref().map(|facts| facts.id.serial.as_str());
            if admission.worth_recording(websocket, peer, serial) {
                let entry = Entry::new("admission.admitted", "admitted")
                    .peer(peer)
                    .with("plane", "agent")
                    .with("transport", if websocket { "websocket" } else { "http" })
                    .with(
                        "as",
                        match &classified {
                            Peer::Member => "member",
                            Peer::Enrolling { .. } => "enrolling",
                        },
                    )
                    .with(
                        "authority",
                        presented
                            .as_ref()
                            .and_then(|facts| admission.issuers_role(&facts.id.issuer)),
                    )
                    .with("serial", serial.map(str::to_string));
                // Not admitted without its record (SEC-append-only-audit-record §5).
                if audit.record(entry).is_err() {
                    return Err(Refused::Busy);
                }
                if !websocket {
                    admission.recorded(peer, serial);
                }
            }
        }
        let proofs = Proofs {
            der: certificate.map(Arc::from),
            host: presented.as_ref().and_then(|facts| facts.host.clone()),
            subject_values: presented
                .as_ref()
                .map(|facts| facts.subject_values.clone())
                .unwrap_or_default(),
            certificate: presented.map(|facts| (facts.id, facts.not_after_ms)),
        };
        Ok((classified, proofs))
    }

    /// Admits a download (SEC-admission-by-client-certificate §18): the same handshake as
    /// `/v1/opamp`, and a certificate from the client CA — a bootstrap certificate is not enough —
    /// counted against the sender's rate (SEC-tls13-two-planes-bounded-input §24). What the
    /// certificate proves, read once, is handed to the route, which serves only what is offered to
    /// an Agent of the host it names (SRV-fleet-record-agents-hosts §16).
    pub(crate) fn admit_download(
        &self,
        peer: Option<IpAddr>,
        path: &str,
        certificate: Option<&[u8]>,
    ) -> Result<Option<Proofs>, DownloadRefused> {
        let guard = &self.admission;
        let record = |event: &str, outcome: &str| {
            guard.audit.as_ref().map(|audit| {
                audit.record(
                    Entry::new(event, outcome)
                        .peer(peer)
                        .with("plane", "agent")
                        .with("path", path.to_string()),
                )
            })
        };
        if let (Some(throttle), Some(peer)) = (&guard.throttle, peer) {
            if let Some(wait) = throttle.retry_after(peer) {
                let _ = record("download.throttled", "throttled");
                return Err(DownloadRefused::Throttled(wait));
            }
        }
        let checked = checked(certificate, guard.revocations.as_deref());
        let member = match (certificate, &checked) {
            (Some(cert), Ok(_)) => guard.issuers.classify(cert) == Peer::Member,
            (Some(_), Err(_)) => false,
            (None, _) => !guard.require_client_certificate,
        };
        let facts = checked.ok().flatten();
        if !member {
            if let (Some(throttle), Some(peer)) = (&guard.throttle, peer) {
                throttle.failed(peer);
            }
            let _ = record("download.refused", "refused");
            return Err(DownloadRefused::NotOfTheFleet);
        }
        let proofs = certificate.map(|cert| Proofs {
            der: Some(Arc::from(cert)),
            host: facts.as_ref().and_then(|facts| facts.host.clone()),
            subject_values: facts
                .as_ref()
                .map(|facts| facts.subject_values.clone())
                .unwrap_or_default(),
            certificate: facts.map(|facts| (facts.id, facts.not_after_ms)),
        });
        if let Some(agent_rate) = self.state.agent_rate() {
            let proofs = proofs.clone().unwrap_or_default();
            let subject = proofs.subject(peer);
            let gateway = is_gateway(&subject, guard.revocations.as_ref());
            // A download names no Agent, so through a Gateway it counts in the aggregate alone. Over
            // the limit is no failure: the throttle of SEC-admission-by-client-certificate §19 is not fed.
            if let Err(bucket) = agent_rate.admit(&subject, gateway, None) {
                if let Some(audit) = &guard.audit {
                    audit.refusal(
                        proofs
                            .throttled_entry(peer, guard.revocations.as_ref(), bucket)
                            .with("route", "download"),
                    );
                }
                return Err(DownloadRefused::OverRate);
            }
        }
        if record("download.admitted", "admitted") == Some(Err(crate::audit::Unavailable)) {
            return Err(DownloadRefused::Busy);
        }
        Ok(proofs)
    }

    /// An entry's artifact for a download: its size and its bytes. A member certificate fetches
    /// only what is offered to an Agent its host speaks for (PKG-signed-packages-and-delivery §2).
    /// The download guard admitted the certificate and handed over what it proves; a request
    /// without one exists only where no certificate is required, and is tested here no more than
    /// at admission.
    pub(crate) fn artifact(
        &self,
        proofs: Option<&Proofs>,
        peer: Option<IpAddr>,
        id: &PackageId,
        platform: &Platform,
    ) -> Result<(u64, Box<dyn std::io::Read + Send>), ArtifactRefused> {
        let state = &self.state;
        if let Some(proofs) = proofs {
            let host = proofs.host.as_deref();
            if !host.is_some_and(|host| state.offers_artifact(host, id, platform)) {
                state.audit_refusal(
                    crate::audit::Entry::new("download.refused", "refused")
                        .peer(peer)
                        .with("plane", "agent")
                        .with("check", "not offered")
                        .with("host", host.map(str::to_string))
                        .with(
                            "serial",
                            proofs.certificate.as_ref().map(|(id, _)| id.serial.clone()),
                        )
                        .with("agent_type", id.agent_type.clone())
                        .with("version", id.version.clone())
                        .with("platform", platform.tag()),
                );
                return Err(ArtifactRefused::NotFound);
            }
        }
        let Some((sha256, size)) = state
            .packages()
            .and_then(|store| store.artifact(id, platform))
        else {
            return Err(ArtifactRefused::NotFound);
        };
        let opened = state
            .packages()
            .map(|store| store.open_artifact(&sha256))
            .unwrap_or_else(|| Err("package delivery is not configured".to_string()));
        match opened {
            Ok(bytes) => Ok((size, bytes)),
            Err(e) => {
                warn!(package = %id, error = %e, "cannot open the stored artifact");
                Err(ArtifactRefused::Unreadable)
            }
        }
    }

    /// The revoked certificates of the client CA, chains resolved, for a host marked as a Gateway
    /// (SEC-admission-by-client-certificate §34), as its body and its `ETag` ([`gateway_list`]);
    /// `None`, audited, for a member whose host is not one. `host` is the host a member's
    /// certificate names.
    pub(crate) fn gateway_revocations(
        &self,
        host: Option<String>,
        peer: Option<IpAddr>,
    ) -> Option<(String, String)> {
        // Without a register nothing is revoked, so the empty list reveals nothing to anyone. With one,
        // the list is a member's whose host is marked as a Gateway, and nobody else's.
        match self.state.revocations() {
            None => Some(gateway_list(&[], None)),
            Some(revocations) => {
                if !host
                    .as_ref()
                    .is_some_and(|host| revocations.is_gateway(host))
                {
                    // A member asking for what only a Gateway is handed is a refusal like any other
                    // (SEC-append-only-audit-record §1).
                    self.state.audit_refusal(
                        Entry::new("gateway_list.refused", "refused")
                            .peer(peer)
                            .with("plane", "agent")
                            .with("host", host)
                            .with("check", "not a gateway"),
                    );
                    return None;
                }
                Some(gateway_list_of(revocations))
            }
        }
    }

    /// Every floor the Server stores, for every admitted member (PKG-versions-high-water-mark-floor §15): a JSON object
    /// that maps an Agent type to its version and its signature in hex; `None`, audited, for an
    /// enrolling host, which is no member yet. Each Client checks the signatures itself, so the
    /// Server serves what it stores whatever its own check found.
    pub(crate) fn package_floors(&self, enrolling: bool, peer: Option<IpAddr>) -> Option<String> {
        if enrolling {
            self.state.audit_refusal(
                Entry::new("package_floors.refused", "refused")
                    .peer(peer)
                    .with("plane", "agent")
                    .with("check", "enrolling"),
            );
            return None;
        }
        let floors: serde_json::Map<String, serde_json::Value> = self
            .state
            .floors()
            .map(|floors| floors.all())
            .unwrap_or_default()
            .into_iter()
            .map(|(agent_type, floor)| {
                (
                    agent_type,
                    serde_json::json!({
                        "version": floor.version,
                        "signature": hex::encode(floor.signature),
                    }),
                )
            })
            .collect();
        Some(serde_json::Value::Object(floors).to_string())
    }
}

/// The Gateways' list as its body and its `ETag`. The `ETag` is the SHA-256 of the body, and where
/// the store keeps the list's one version, as PostgreSQL does, `list-<version>-` before it
/// (SRV-state-stores-contract-pair §10). Every instance that holds the same list
/// at the same version answers the same `ETag`; the hash keeps one version from naming two bodies,
/// as it would when a Server of 0.8.0 beside this one registered a renewal, which moves no version
/// (SRV-state-stores-contract-pair §10). Either way it changes with the list and survives a restart.
#[must_use]
pub(crate) fn gateway_list(
    certificates: &[serde_json::Value],
    version: Option<u64>,
) -> (String, String) {
    use sha2::Digest as _;
    let body = serde_json::json!({ "certificates": certificates }).to_string();
    let hash = hex::encode(sha2::Sha256::digest(body.as_bytes()));
    let etag = match version {
        Some(version) => format!("\"list-{version}-{hash}\""),
        None => format!("\"{hash}\""),
    };
    (body, etag)
}

/// The Gateways' list of `revocations`, as [`gateway_list`] answers it.
#[must_use]
pub(crate) fn gateway_list_of(revocations: &Revocations) -> (String, String) {
    let (revoked, version) = revocations.revoked_certificates_at("client");
    let certificates: Vec<serde_json::Value> = revoked
        .into_iter()
        .map(|id| serde_json::json!({ "issuer": id.issuer, "serial": id.serial }))
        .collect();
    gateway_list(&certificates, version)
}

/// One connection as the fleet sees it.
pub(crate) struct Connection {
    transport: Transport,
    /// Set on an enrolment connection (SEC-bootstrap-certificates-and-enrolment §9): the bootstrap certificate it carried,
    /// where from, and the key its request asks to be certified, once it has sent one.
    enrolling: Option<Enrolling>,
    /// This connection's identity — what the duplicate detection tells connections apart by. Plain
    /// HTTP is stateless polling, so there is none to pass.
    conn: Option<ConnId>,
    /// The Agents this socket carried, any number of them told apart by `instance_uid` alone
    /// (CLT-modes-and-gateway), so all of them are marked unreachable when it goes.
    seen: Vec<InstanceUid>,
    /// What admitted the connection: re-checked before a CSR is signed, and the presented
    /// certificate is the predecessor of one it renews (SRV-state-stores-contract-pair §9 and SEC-admission-by-client-certificate §28).
    proofs: Proofs,
    /// Where the connection comes from.
    peer: Option<IpAddr>,
    /// Who its messages are counted for (CLT-modes-and-gateway §26).
    subject: Subject,
    /// Why the session was ended, once its [`Guard`] has ended it.
    ended: Arc<Mutex<Option<&'static str>>>,
}

/// What an enrolling connection carried, and what it has asked for.
struct Enrolling {
    requester: Requester,
    request: Arc<Mutex<Option<crate::enrolment::Request>>>,
    instance_uid: Arc<Mutex<Vec<u8>>>,
}

/// What a connection is sent without asking. A member hears of every change to the fleet's
/// desired state — the "within seconds" of the control loop. An enrolling host hears of its own
/// request's decision, and its connection ends when the window closes or the request is rejected.
enum Pushes {
    Desired(watch::Receiver<u64>),
    Enrolment {
        changes: watch::Receiver<u64>,
        enrolment: Arc<Enrolment>,
        request: Arc<Mutex<Option<crate::enrolment::Request>>>,
    },
}

/// A WebSocket session's outbound side, and what ends it: a revocation of what admitted it, or the
/// expiry of its certificate (SEC-admission-by-client-certificate §32 and §33).
pub(crate) struct Session {
    pushes: Pushes,
    guard: Option<Guard>,
    /// Why the session was ended, once something ended it: what the connection's own loop decided
    /// as well as what its [`Guard`] did.
    ended: Arc<Mutex<Option<&'static str>>>,
}

/// Why a session is ended whose every Agent was forgotten (SRV-fleet-record-agents-hosts §10).
pub(crate) const FORGOTTEN: &str = "forgotten";

struct Guard {
    revocations: Arc<Revocations>,
    audit: Option<Arc<dyn Audit>>,
    changes: watch::Receiver<u64>,
    proofs: Proofs,
    expires: Option<tokio::time::Instant>,
    ended: Arc<Mutex<Option<&'static str>>>,
}

impl Guard {
    fn new(
        revocations: Arc<Revocations>,
        audit: Option<Arc<dyn Audit>>,
        proofs: Proofs,
        ended: Arc<Mutex<Option<&'static str>>>,
    ) -> Self {
        let expires = proofs.certificate.as_ref().map(|(_, not_after_ms)| {
            tokio::time::Instant::now()
                + std::time::Duration::from_millis(not_after_ms.saturating_sub(now_ms()))
        });
        let mut changes = revocations.subscribe();
        // A revocation that landed between admission and here is checked at once.
        changes.mark_changed();
        Guard {
            changes,
            revocations,
            audit,
            proofs,
            expires,
            ended,
        }
    }

    /// Waits until the session must end, and says why.
    async fn tripped(&mut self) -> &'static str {
        let Guard {
            revocations,
            changes,
            proofs,
            expires,
            ..
        } = self;
        let expiry = async {
            match expires {
                Some(at) => tokio::time::sleep_until(*at).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(expiry);
        loop {
            tokio::select! {
                () = &mut expiry => return "certificate expired",
                changed = changes.changed() => {
                    if changed.is_err() {
                        // The list is gone with the Server; only the expiry is left to wait for.
                        (&mut expiry).await;
                        return "certificate expired";
                    }
                    if proofs.revoked(revocations) {
                        return "revoked";
                    }
                }
            }
        }
    }
}

impl Session {
    /// The next occasion to push, or `None` once the session ends: its pushes stop, or what
    /// admitted it was revoked or expired.
    pub(crate) async fn next_push(&mut self) -> Option<Push> {
        let Session {
            pushes,
            guard,
            ended,
        } = self;
        if ended.lock().expect("ended lock").is_some() {
            return None;
        }
        let Some(guard) = guard else {
            return pushes.next().await;
        };
        let ended = guard.ended.clone();
        let audit = guard.audit.clone();
        let serial = guard
            .proofs
            .certificate
            .as_ref()
            .map(|(id, _)| id.serial.clone());
        tokio::select! {
            item = pushes.next() => item,
            reason = guard.tripped() => {
                info!(reason, "ending a session");
                if let Some(audit) = audit {
                    audit.refusal(
                        Entry::new("session.ended", "ended")
                            .with("reason", reason)
                            .with("serial", serial),
                    );
                }
                *ended.lock().expect("ended lock") = Some(reason);
                None
            }
        }
    }
}

/// One occasion to push.
pub(crate) enum Push {
    Desired,
    Enrolment,
}

impl Pushes {
    async fn next(&mut self) -> Option<Push> {
        match self {
            Pushes::Desired(changes) => changes.changed().await.ok().map(|()| Push::Desired),
            Pushes::Enrolment {
                changes,
                enrolment,
                request,
            } => {
                changes.changed().await.ok()?;
                let key = request
                    .lock()
                    .expect("request lock")
                    .as_ref()
                    .map(|r| r.key_fingerprint.clone());
                if !enrolment.is_open() || key.is_some_and(|key| enrolment.is_rejected(&key)) {
                    return None;
                }
                Some(Push::Enrolment)
            }
        }
    }
}

impl AgentCases {
    /// A connection opens: an enrolling host's, or a member's. `proofs` is what admission found,
    /// `classified` how it told the certificate (SEC-admission-by-client-certificate §32 and
    /// SEC-bootstrap-certificates-and-enrolment §9).
    pub(crate) fn connect(
        &self,
        proofs: Proofs,
        classified: Option<&Peer>,
        websocket: bool,
        peer_address: Option<SocketAddr>,
    ) -> (Connection, Option<Session>) {
        let ended = Arc::new(Mutex::new(None));
        let session = |pushes: Pushes| Session {
            pushes,
            ended: ended.clone(),
            guard: self.state.revocations().map(|revocations| {
                Guard::new(
                    revocations.clone(),
                    self.state.audit().cloned(),
                    proofs.clone(),
                    ended.clone(),
                )
            }),
        };
        let peer = peer_address.map(|peer| peer.ip());
        if let (
            Some(Peer::Enrolling {
                subject,
                carried,
                fingerprint,
                id,
            }),
            Some(enrolment),
        ) = (classified, self.state.enrolment())
        {
            let enrolling = Enrolling {
                requester: Requester {
                    bootstrap_subject: subject.clone(),
                    carried: (**carried).clone(),
                    bootstrap_fingerprint: fingerprint.clone(),
                    bootstrap: id.clone(),
                    peer,
                    gateway: None,
                },
                request: Arc::new(Mutex::new(None)),
                instance_uid: Arc::new(Mutex::new(Vec::new())),
            };
            let outbound = websocket.then(|| {
                session(Pushes::Enrolment {
                    changes: enrolment.subscribe(),
                    enrolment: enrolment.clone(),
                    request: enrolling.request.clone(),
                })
            });
            return (
                Connection {
                    transport: if websocket {
                        Transport::WebSocket
                    } else {
                        Transport::Http
                    },
                    enrolling: Some(enrolling),
                    conn: None,
                    seen: Vec::new(),
                    proofs: proofs.clone(),
                    peer,
                    // A bootstrap certificate may be the whole fleet's, so an enrolling host is
                    // counted by its address.
                    subject: Subject::peer(peer),
                    ended,
                },
                outbound,
            );
        }
        let (transport, conn, outbound) = if websocket {
            (
                Transport::WebSocket,
                Some(self.state.connection_id()),
                Some(session(Pushes::Desired(self.state.subscribe()))),
            )
        } else {
            (Transport::Http, None, None)
        };
        (
            Connection {
                transport,
                enrolling: None,
                conn,
                seen: Vec::new(),
                subject: proofs.subject(peer),
                proofs,
                peer,
                ended,
            },
            outbound,
        )
    }

    /// One report (SRV-fleet-record-agents-hosts): an enrolment request, one through a Gateway,
    /// or a member's report with what it asks for. `Err(NoLease)` while this instance holds no
    /// lease it can prove.
    ///
    /// # Errors
    /// Returns [`NoLease`] while the instance's lease cannot be proved.
    pub(crate) async fn message(
        &self,
        carrier: &mut Connection,
        report: AgentToServer,
    ) -> Result<ServerToAgent, NoLease> {
        // Nothing is answered on a lease this instance cannot prove, after a suspend before its
        // listeners are closed included
        // (SRV-instances-share-one-postgresql §3).
        if !self.state.holds_its_lease() {
            return Err(NoLease);
        }
        // An enrolment request through a Gateway (CLT-modes-and-gateway §25); counted in the Gateway's
        // enrolment aggregate and, once checked, by the bootstrap certificate it names (CLT-modes-and-gateway §26).
        let through_gateway = self.enrolment_through_gateway(carrier, &report);
        // Before anything else: enrolment, the CSR, and the fleet lock (SEC-tls13-two-planes-bounded-input §24).
        if self.over_rate(
            carrier,
            Some(&report.instance_uid),
            through_gateway.as_ref(),
        ) {
            return Ok(unavailable(&report.instance_uid, THROTTLED));
        }
        if let Some(enrolling) = &carrier.enrolling {
            return Ok(self.enrol(enrolling, &report));
        }
        if through_gateway.is_some() {
            return Ok(self.enrol_through_gateway(carrier, &report));
        }
        let csr = report
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .is_some_and(|opamp| opamp.certificate_request.is_some());
        if csr {
            if let Some(revocations) = self.state.revocations() {
                // A session whose proof was revoked a moment ago is not handed a certificate
                // before its guard closes it (SEC-admission-by-client-certificate §28).
                if carrier.proofs.revoked(revocations) {
                    return Ok(bad_request("this connection is no longer admitted"));
                }
            }
        }
        // The connection's own certificate, whatever the message claims to be: behind a Gateway
        // that is the Gateway's, which renews nothing without a renewal proof (CLT-modes-and-gateway).
        let presented = carrier
            .proofs
            .certificate
            .as_ref()
            .map(|(id, not_after_ms)| Presented {
                id: id.clone(),
                host: carrier.proofs.host.clone(),
                subject_values: carrier.proofs.subject_values.clone(),
                not_after_ms: *not_after_ms,
            });
        // The report reads and writes the store, so it runs off the runtime's workers: a
        // handshake, another connection's message and a ping never wait for it.
        let (state, transport, conn) = (self.state.clone(), carrier.transport, carrier.conn);
        let outcome = match tokio::task::spawn_blocking(move || {
            state.process_presented(report, transport, conn, presented.as_ref())
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => std::panic::resume_unwind(e.into_panic()),
        };
        if let (Some(uid), Some(_)) = (outcome.uid, carrier.conn) {
            if outcome.disconnected {
                carrier.seen.retain(|s| s != &uid);
            } else if !carrier.seen.contains(&uid) {
                carrier.seen.push(uid);
            }
        }
        Ok(outcome.reply)
    }

    /// What a connection is sent on `push`: an enrolling host's certificate once approved, or each
    /// carried Agent's queued restart and offer.
    pub(crate) fn outbound(&self, carrier: &mut Connection, push: Push) -> Vec<ServerToAgent> {
        // Nothing is pushed on a lease this instance cannot prove: a queued restart stays queued,
        // and an offer or an approval waits for an instance that holds one
        // (SRV-instances-share-one-postgresql §3).
        if !self.state.holds_its_lease() {
            return Vec::new();
        }
        if let (Push::Enrolment, Some(enrolling)) = (push, &carrier.enrolling) {
            // A decision reached while the host waits: an approval is handed over at once.
            let Some(request) = enrolling.request.lock().expect("request lock").clone() else {
                return Vec::new();
            };
            let Some(enrolment) = self.state.enrolment() else {
                return Vec::new();
            };
            let uid = enrolling.instance_uid.lock().expect("uid lock").clone();
            return match enrolment.submit(request, enrolling.requester.clone()) {
                Submitted::Issued(cert) => vec![self.state.enrolment_answer(&uid, Some(cert))],
                _ => Vec::new(),
            };
        }
        // An Agent forgotten while this socket carried it has its session ended: the socket's, when
        // it carried no other.
        if let Some(conn) = carrier.conn {
            let forgotten = self.state.take_forgotten(conn);
            if !forgotten.is_empty() {
                carrier.seen.retain(|uid| !forgotten.contains(uid));
                if carrier.seen.is_empty() {
                    info!(agents = ?forgotten, "ending the session of a forgotten agent");
                    *carrier.ended.lock().expect("ended lock") = Some(FORGOTTEN);
                    return Vec::new();
                }
            }
        }
        // Only a WebSocket carries Agents to push to, and each has its connection.
        let Some(conn) = carrier.conn else {
            return Vec::new();
        };
        let mut messages = Vec::new();
        for uid in &carrier.seen {
            // A queued restart goes first, as its own message — the Baseline's command message is
            // never combined with an offer.
            if let Some(command) = self.state.restart_command_for(uid, conn) {
                debug!(agent = %uid, "pushing a restart command");
                messages.push(command);
            }
            if let Some(offer) = self.state.offer_for(uid) {
                debug!(agent = %uid, "pushing a configuration offer");
                messages.push(offer);
            }
        }
        messages
    }

    /// The answer to a message that does not decode, counted as a message
    /// (SEC-tls13-two-planes-bounded-input §24).
    pub(crate) fn unreadable(&self, carrier: &mut Connection) -> ServerToAgent {
        // It names no Agent, so its reply reaches none (SEC-tls13-two-planes-bounded-input §27).
        if self.over_rate(carrier, None, None) {
            return unavailable(&[], THROTTLED);
        }
        bad_request(match carrier.transport {
            Transport::Http => "the request body is not a valid AgentToServer message",
            Transport::WebSocket => "the frame is not a valid OpAMP message",
        })
    }

    /// Why the session was ended, once something ended it; [`FORGOTTEN`] when its every Agent was
    /// forgotten.
    pub(crate) fn ended(&self, carrier: &Connection) -> Option<&'static str> {
        *carrier.ended.lock().expect("ended lock")
    }

    /// A restart command its connection did not take stays queued
    /// (SRV-instances-share-one-postgresql §8).
    pub(crate) fn unsent(&self, message: &ServerToAgent) {
        self.state.unsent(message);
    }

    /// The connection is gone; every Agent it carried is unreachable until it reports again.
    /// An enrolling host was never one of the fleet's Agents, so there is nothing to mark.
    pub(crate) fn closed(&self, carrier: Connection) {
        if let Some(conn) = carrier.conn {
            self.state.mark_disconnected(&carrier.seen, conn);
        }
    }
}

/// An enrolment request through a Gateway, as the throttle keys it (CLT-modes-and-gateway §26).
enum Relayed {
    /// Its proof embeds a certificate a bootstrap CA of `[enrolment]` issued: its own bucket.
    Checked(CertId),
    /// Its proof embeds no such certificate, or does not parse: the enrolment aggregate alone, and
    /// refused.
    Unchecked,
}

impl AgentCases {
    /// Takes the message's token (SEC-tls13-two-planes-bounded-input) — for an enrolment request through a Gateway, from the
    /// Gateway's enrolment aggregate and the bucket of the bootstrap certificate it names, once
    /// checked (CLT-modes-and-gateway §26) — and records a message past the limit.
    fn over_rate(
        &self,
        carrier: &Connection,
        instance_uid: Option<&[u8]>,
        enrolment: Option<&Relayed>,
    ) -> bool {
        let Some(agent_rate) = self.state.agent_rate() else {
            return false;
        };
        let revocations = self.state.revocations();
        let gateway = is_gateway(&carrier.subject, revocations);
        let taken = match enrolment {
            Some(relayed) => agent_rate.admit_enrolment(
                &carrier.subject,
                gateway,
                match relayed {
                    Relayed::Checked(bootstrap) => Some(bootstrap),
                    Relayed::Unchecked => None,
                },
            ),
            None => agent_rate.admit(&carrier.subject, gateway, instance_uid),
        };
        let Err(bucket) = taken else {
            return false;
        };
        debug!(bucket = bucket.as_str(), "a message past the rate limit");
        let uid = instance_uid
            .filter(|uid| InstanceUid::from_wire(uid).is_some())
            .map(hex::encode);
        self.state.audit_refusal(
            carrier
                .proofs
                .throttled_entry(carrier.peer, revocations, bucket)
                .with("instance_uid", uid)
                .with("route", "opamp")
                .with("transport", carrier.transport.as_str()),
        );
        true
    }

    /// One message on an enrolment connection (SEC-bootstrap-certificates-and-enrolment §9). Nothing in it is read but the
    /// certificate signing request: no Agent record is created and nothing is offered but the
    /// issued certificate.
    fn enrol(&self, enrolling: &Enrolling, report: &AgentToServer) -> ServerToAgent {
        let Some(enrolment) = self.state.enrolment() else {
            return bad_request("this Server does not enrol");
        };
        // A session whose bootstrap certificate was revoked a moment ago neither queues a request
        // nor collects a certificate before its guard closes it (SEC-admission-by-client-certificate §28).
        if self.state.revocations().is_some_and(|revocations| {
            revocations.is_certificate_revoked(&enrolling.requester.bootstrap)
        }) {
            return bad_request("this connection is no longer admitted");
        }
        *enrolling.instance_uid.lock().expect("uid lock") = report.instance_uid.clone();
        let Some(csr) = report
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .and_then(|opamp| opamp.certificate_request.as_ref())
        else {
            // The capabilities say a request may be sent; nothing else is answered.
            return self.state.enrolment_answer(&report.instance_uid, None);
        };
        // A request claiming another identity never reaches the queue (SEC-admission-by-client-certificate), nor does one
        // proving another bootstrap certificate than the handshake's (CLT-modes-and-gateway §22).
        let request = match String::from_utf8(csr.csr.clone())
            .map_err(|_| "the certificate signing request is not PEM".to_string())
            .and_then(|pem| {
                crate::ca::check_claims(&pem, &report.instance_uid)?;
                proves_the_handshake(&pem, &enrolling.requester)?;
                crate::ca::enrolment_request(&pem, &report.instance_uid)
            }) {
            Ok(request) => request,
            Err(e) => return bad_request(&e),
        };
        *enrolling.request.lock().expect("request lock") = Some(request.clone());
        self.file(
            enrolment,
            request,
            &enrolling.requester,
            &report.instance_uid,
        )
    }

    /// Whether a message is an enrolment request through a Gateway (CLT-modes-and-gateway §25): a CSR
    /// carrying a bootstrap proof over the certificate of a host marked as a Gateway, unless the
    /// proof names that certificate itself — then it is the Gateway's own CSR, which `renews`
    /// decides; it names that certificate only when it embeds it byte for byte. A CSR with a
    /// renewal proof beside the bootstrap proof is no enrolment either: it stays on the route it
    /// came by, whose reader of a proof refuses it (CLT-modes-and-gateway §21). Its certificate is
    /// checked as cheaply as the throttle allows before it keys a bucket (CLT-modes-and-gateway §26): it parses, and
    /// it is one a bootstrap CA of `[enrolment]` issued as the handshake checks one — its issuer,
    /// that CA's signature over it, `CA:FALSE`, its validity, and the `clientAuth` extended key
    /// usage where it names any ([`Issuers::bootstrap_issued`]). The proof itself is checked by
    /// [`enrol_through_gateway`].
    ///
    /// [`enrol_through_gateway`]: Self::enrol_through_gateway
    /// [`Issuers::bootstrap_issued`]: crate::tls::Issuers::bootstrap_issued
    fn enrolment_through_gateway(
        &self,
        carrier: &Connection,
        report: &AgentToServer,
    ) -> Option<Relayed> {
        if carrier.enrolling.is_some() || !is_gateway(&carrier.subject, self.state.revocations()) {
            return None;
        }
        let csr = report
            .connection_settings_request
            .as_ref()?
            .opamp
            .as_ref()?
            .certificate_request
            .as_ref()?;
        // A request that is not one PEM block, or that carries both proofs, is refused where any
        // CSR is.
        let pem = std::str::from_utf8(&csr.csr).ok()?;
        let csr_der = fleet_core::csr::der(pem.as_bytes()).ok()?;
        fleet_core::csr::proof(&csr_der, fleet_core::csr::Proof::Bootstrap)
            .ok()
            .flatten()?;
        let der = match crate::ca::bootstrap_proof_certificate(pem) {
            Ok(None) => return None,
            Ok(Some(der)) => der,
            Err(_) => return Some(Relayed::Unchecked),
        };
        // The very certificate presented, byte for byte: an issuer and serial alone are anyone's
        // to copy into a certificate of their own.
        if carrier.proofs.der.as_deref() == Some(der.as_slice()) {
            return None;
        }
        Some(match self.admission.issuers.bootstrap_issued(&der) {
            Ok(Peer::Enrolling { id, .. }) => Relayed::Checked(id),
            _ => Relayed::Unchecked,
        })
    }

    /// An enrolment request through a Gateway (CLT-modes-and-gateway §25): decided as any enrolment. The
    /// proof is checked — a bootstrap certificate of `[enrolment]`, valid, not revoked, its key
    /// having signed the request's (CLT-modes-and-gateway §21) — and the request is filed under the
    /// certificate it proves, naming the Gateway's host in place of a peer address. Nothing in the
    /// message is read but the certificate signing request, and no Agent record is made. Every
    /// answer is addressed to the message's `instance_uid`, by which the Gateway routes it back.
    fn enrol_through_gateway(&self, carrier: &Connection, report: &AgentToServer) -> ServerToAgent {
        let uid = &report.instance_uid;
        let gateway = carrier.proofs.host.clone();
        let refuse = |e: &str| {
            info!(gateway = gateway.as_deref().unwrap_or(""), error = %e, "refused an enrolment request through a Gateway");
            self.state.audit_refusal(
                Entry::new("enrolment.refused", "refused")
                    .with("gateway", gateway.clone())
                    .with("reason", e.to_string())
                    .with("instance_uid", hex::encode(uid)),
            );
            let mut reply = bad_request(e);
            reply.instance_uid.clone_from(uid);
            reply
        };
        let Some(enrolment) = self.state.enrolment() else {
            return refuse("this Server does not enrol");
        };
        if self
            .state
            .revocations()
            .is_some_and(|revocations| carrier.proofs.revoked(revocations))
        {
            return refuse("this connection is no longer admitted");
        }
        let Some(csr) = report
            .connection_settings_request
            .as_ref()
            .and_then(|request| request.opamp.as_ref())
            .and_then(|opamp| opamp.certificate_request.as_ref())
        else {
            return refuse("the message carries no certificate signing request");
        };
        let filed = String::from_utf8(csr.csr.clone())
            .map_err(|_| "the certificate signing request is not PEM".to_string())
            .and_then(|pem| {
                crate::ca::check_claims(&pem, uid)?;
                let der = crate::ca::bootstrap_proof(&pem)?
                    .ok_or_else(|| "the bootstrap proof does not parse".to_string())?;
                let Peer::Enrolling {
                    subject,
                    carried,
                    fingerprint,
                    id,
                } = self
                    .admission
                    .issuers
                    .bootstrap_issued(&der)
                    .map_err(|why| format!("the bootstrap proof does not hold: {why}"))?
                else {
                    return Err("the bootstrap proof does not hold".to_string());
                };
                if self
                    .state
                    .revocations()
                    .is_some_and(|revocations| revocations.is_certificate_revoked(&id))
                {
                    return Err(
                        "the bootstrap proof does not hold: its certificate is revoked".to_string(),
                    );
                }
                let requester = Requester {
                    bootstrap_subject: subject,
                    carried: *carried,
                    bootstrap_fingerprint: fingerprint,
                    bootstrap: id,
                    peer: None,
                    gateway: gateway.clone(),
                };
                Ok((crate::ca::enrolment_request(&pem, uid)?, requester))
            });
        match filed {
            Ok((request, requester)) => self.file(enrolment, request, &requester, uid),
            Err(e) => refuse(&e),
        }
    }

    /// Takes in an enrolment request, or answers it once it has been decided (SEC-bootstrap-certificates-and-enrolment §9 and §10): what both an enrolment connection and a Gateway's relay are answered.
    fn file(
        &self,
        enrolment: &Enrolment,
        request: crate::enrolment::Request,
        requester: &Requester,
        instance_uid: &[u8],
    ) -> ServerToAgent {
        let key = request.key_fingerprint.clone();
        let new = !enrolment.pending().iter().any(|pending| pending.id == key);
        let subject = request.subject.clone();
        let answer = match enrolment.submit(request, requester.clone()) {
            Submitted::Waiting => {
                info!(key = %key, "an enrolment request waits for an operator");
                if let (true, Some(audit)) = (new, self.state.audit()) {
                    audit.refusal(
                        Entry::new("enrolment.queued", "queued")
                            .peer(requester.peer)
                            .with("gateway", requester.gateway.clone())
                            .with("id", key.clone())
                            .with("subject", subject)
                            .with("bootstrap_subject", requester.bootstrap_subject.clone())
                            .with("instance_uid", hex::encode(instance_uid)),
                    );
                }
                self.state.enrolment_answer(instance_uid, None)
            }
            Submitted::Issued(cert) => {
                info!(key = %key, "handed an enrolling host its certificate");
                self.state.enrolment_answer(instance_uid, Some(cert))
            }
            Submitted::Rejected => bad_request("an operator rejected this enrolment request"),
            Submitted::Full => {
                unavailable(instance_uid, "the enrolment queue is full — retry later")
            }
            Submitted::OverShare { first } => {
                // Audited once per certificate and window (SEC-bootstrap-certificates-and-enrolment): a host re-sending its
                // refused request does not flood the record.
                if let (true, Some(audit)) = (first, self.state.audit()) {
                    audit.refusal(
                        Entry::new("enrolment.refused", "refused")
                            .peer(requester.peer)
                            .with("gateway", requester.gateway.clone())
                            .with("reason", "bootstrap_share")
                            .with("bootstrap_serial", requester.bootstrap.serial.clone())
                            .with("bootstrap_subject", requester.bootstrap_subject.clone()),
                    );
                }
                unavailable(
                    instance_uid,
                    "this bootstrap certificate holds its share of the enrolment queue — retry \
                     later",
                )
            }
            Submitted::Closed => unavailable(instance_uid, "no enrolment window is open"),
        };
        // Addressed to the message, so a Gateway routes even a refusal back (CLT-modes-and-gateway §13).
        ServerToAgent {
            instance_uid: instance_uid.to_vec(),
            ..answer
        }
    }
}

/// Whether a request on an enrolment connection proves the certificate of its handshake (CLT-modes-and-gateway §19):
/// a bootstrap proof there must name that certificate, and hold; a request without one
/// proves the handshake's certificate alone.
///
/// # Errors
/// Returns the `BadRequest` text for a proof that does not hold or names another certificate.
fn proves_the_handshake(csr_pem: &str, handshake: &Requester) -> Result<(), String> {
    use sha2::{Digest as _, Sha256};
    match crate::ca::bootstrap_proof(csr_pem)? {
        Some(der) if hex::encode(Sha256::digest(&der)) != handshake.bootstrap_fingerprint => Err(
            "the bootstrap proof names another certificate than the one this connection presented"
                .to_string(),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A restart command the endpoint reports unsent is queued again, so it is sent once more.
    /// Verifies: SRV-instances-share-one-postgresql
    #[test]
    fn a_restart_command_reported_unsent_is_queued_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = Arc::new(AppState::new(dir.path().to_path_buf()).expect("state"));
        let uid = InstanceUid([0x30; 16]);
        state.process(
            AgentToServer {
                instance_uid: uid.as_bytes().to_vec(),
                sequence_num: 1,
                capabilities: opamp::proto::AgentCapabilities::ReportsStatus as u64
                    | opamp::proto::AgentCapabilities::AcceptsRestartCommand as u64,
                ..Default::default()
            },
            Transport::WebSocket,
            Some(1),
        );
        assert!(state.request_restart(&uid).is_ok(), "queued");
        let command = state.restart_command_for(&uid, 1).expect("sent");
        let cases = AgentCases::new(state.clone(), Admission::open());
        cases.unsent(&command);
        assert!(state.restart_command_for(&uid, 1).is_some(), "sent again");
    }
}
