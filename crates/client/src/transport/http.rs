//! The plain-HTTP(S) transport (ADR-0007): one POST per exchange, polling at the configured
//! interval (the Baseline's default: 30 seconds), with an immediate follow-up when something
//! changed — so a config outcome is acknowledged now, not a poll later.
//!
//! Every Agent the [`Engine`] holds is polled each cycle — one exchange per Agent, since a
//! plain-HTTP exchange carries exactly one `AgentToServer`; the shared connection pool of the
//! HTTP client is the m = 1 of ADR-0003 here.

use std::time::Duration;

use opamp::proto::{AgentToServer, ServerToAgent};
use prost::Message;
use tracing::{info, warn};

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::service::runtime::Shutdown;
use crate::transport::{OfferOutcome, ReportSink, RunOutcome};

use opamp::endpoint::PROTOBUF_CONTENT_TYPE;
use opamp::uid::InstanceUid;

pub async fn run(
    engine: &mut Engine,
    config: &mut ClientConfig,
    shutdown: &mut Shutdown,
    telemetry: &crate::telemetry::Telemetry,
) -> Result<RunOutcome, String> {
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        // The OpAMP endpoint is a fixed, operator-configured address; it never legitimately
        // redirects, so following one would only let a compromised or misconfigured Server bounce
        // the authenticated session elsewhere. Refuse them.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30));
    if let Some(value) = config.authorization_value()? {
        // The Authorization header (ADR-0013, rotated per ADR-0014) rides every request, the
        // disconnect included.
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|e| format!("the [auth] credentials are not a valid header: {e}"))?;
        value.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
        if config.sends_credentials_in_cleartext() {
            warn!(
                "sending credentials over unencrypted http:// beyond the loopback — use https://"
            );
        }
    }
    // Trust, plus this Client's own certificate when it has one — a Server on mutual TLS asks for
    // it on every request of this transport (ADR-0035).
    builder = crate::tls::trust_and_identity(builder, config)?;
    let client = builder
        .build()
        .map_err(|e| format!("cannot build the HTTP client: {e}"))?;

    let poll = Duration::from_secs(config.poll_interval_secs.max(1));
    let limit = config.max_message_size_bytes;
    info!(endpoint = %config.endpoint, interval = ?poll, "polling");
    engine.force_full_all();

    // Set when a self-update wants the process to exit for its restart (ADR-0020): the loop leaves
    // through the same graceful shutdown a normal stop uses, then reports it as a restart.
    let mut restarting = false;
    'poll: loop {
        // The routine cycle, then immediate follow-ups until no Agent owes a report — a config
        // outcome is acknowledged now, not a poll later.
        let mut reports = engine.poll_reports();
        loop {
            for report in reports {
                match exchange(&client, &config.endpoint, report, limit).await {
                    Ok(reply) => {
                        let handled = engine.handle(&reply);
                        if let Some(delay) = handled.retry_after {
                            tokio::select! {
                                _ = tokio::time::sleep(delay) => {}
                                _ = shutdown.requested() => break 'poll,
                            }
                            continue 'poll;
                        }
                    }
                    // The Server is overloaded, not out of step: wait as it asked and come back
                    // with the same reports. Nothing was processed, so nothing has to be rebuilt.
                    Err(Exchange::Throttled { after, status }) => {
                        warn!(%status, delay = ?after, "the server is throttling; waiting");
                        tokio::select! {
                            _ = tokio::time::sleep(after) => {}
                            _ = shutdown.requested() => break 'poll,
                        }
                        continue 'poll;
                    }
                    // Refused on its own terms — retrying it unchanged is what the Baseline forbids
                    // for a `413`, and a full snapshot would make the retry larger still.
                    Err(e @ Exchange::Refused(_)) => warn!(error = %e, "report refused"),
                    Err(e) => {
                        warn!(error = %e, "exchange failed");
                        // A report was lost; full snapshots so the Server can rebuild.
                        engine.force_full_all();
                    }
                }
            }
            // Enrolment (ADR-0035): with the Server's capabilities now known, ask it to sign a
            // certificate if it signs them and this Client needs one. The answer arrives as an
            // ordinary connection-settings offer, handled just below.
            engine.request_certificate(config);
            // A connection-settings offer (ADR-0014, ADR-0086). An OpAMP half is verified by
            // connecting and ends this run so the runtime reconnects with it; a telemetry-only
            // offer is applied in place and the acknowledgement rides the reports owed below.
            match crate::transport::process_connection_offer(engine, config, telemetry).await {
                OfferOutcome::Reconnect => return Ok(RunOutcome::Reconfigured),
                OfferOutcome::None | OfferOutcome::Applied => {}
            }
            // A package offer (ADR-0015): download and verify; the outcome rides the owed reports.
            let endpoint = config.endpoint.clone();
            let mut sink = PollSink {
                client: &client,
                endpoint: &endpoint,
                limit,
            };
            crate::transport::process_package_downloads(engine, config, &mut sink).await;
            // The self-Agent's configuration is its Supervisor set (ADR-0056): apply it — stop
            // what left, rewrite `supervisor.toml`, start what arrived — and send the retired
            // Agents' goodbyes; the outcome rides the owed reports below.
            crate::transport::process_self_configuration(engine, config, shutdown, &mut sink).await;
            reports = engine.owed_reports();
            if engine.restart_for_update() {
                // Send the owed `Installing`, then leave through the graceful shutdown below so the
                // Managed Processes are stopped and the goodbyes sent before this process exits for
                // the restart (ADR-0020): the pointer already points at the new version, and this
                // one exists only to get out of its way — cleanly, not by abandoning its children.
                if !reports.is_empty() {
                    let _ = sink.send(reports).await;
                }
                restarting = true;
                break 'poll;
            }
            if reports.is_empty() {
                break;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            // A Managed Process changed some Agent's state: exchange now, not at the next poll.
            _ = engine.changed() => {}
            _ = shutdown.requested() => break,
        }
    }

    // Managed Processes stop first; then the Baseline's final messages, one per Agent — which
    // v0.19.0 asks the plain-HTTP transport for too, so the Server marks the Agent disconnected
    // now instead of after a missed poll.
    engine.shutdown_processes().await;
    for goodbye in engine.disconnect_messages() {
        let _ = exchange(&client, &config.endpoint, goodbye, limit).await;
    }
    info!("disconnected");
    Ok(if restarting {
        RunOutcome::RestartForUpdate
    } else {
        RunOutcome::Shutdown
    })
}

/// This transport's way of putting reports on the wire — one exchange each — for jobs that report
/// while they run: a package download reporting its progress (ADR-0015). The Server's replies are
/// discarded: an interim progress report asks nothing, and anything the Server wants to say is
/// picked up by the polling loop that resumes right after.
struct PollSink<'a> {
    client: &'a reqwest::Client,
    endpoint: &'a str,
    limit: usize,
}

impl crate::transport::ReportSink for PollSink<'_> {
    async fn send(&mut self, reports: Vec<AgentToServer>) -> Result<(), ()> {
        for report in reports {
            exchange(self.client, self.endpoint, report, self.limit)
                .await
                .map_err(|_| ())?;
        }
        Ok(())
    }
}

/// One exchange: `AgentToServer` out, `ServerToAgent` back — both under `limit`, the size limit
/// the Baseline requires on this transport in either direction.
async fn exchange(
    client: &reqwest::Client,
    endpoint: &str,
    report: AgentToServer,
    limit: usize,
) -> Result<ServerToAgent, Exchange> {
    // The send side: a request past the limit is not made at all.
    let body = report.encode_to_vec();
    if body.len() > limit {
        // Refused rather than failed, for the same reason a `413` is: re-arming a full snapshot
        // would only make the next attempt bigger.
        return Err(Exchange::Refused(format!(
            "discarding a report of {} bytes: it exceeds the {limit}-byte message size limit",
            body.len()
        )));
    }
    let mut request = client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, PROTOBUF_CONTENT_TYPE)
        .body(body);
    // The Baseline's SHOULD: the instance_uid of the message this request carries, in the canonical
    // UUID string form. It lets a proxy or a log route by Agent without parsing the protobuf body —
    // which is the whole reason it is a header and not only a field.
    if let Some(uid) = InstanceUid::from_wire(&report.instance_uid) {
        request = request.header(INSTANCE_UID_HEADER, uid.to_string());
    }
    let response = request
        .send()
        .await
        .map_err(|e| Exchange::failed(&format!("cannot reach {endpoint}: {e}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Exchange::failed(
            "the server rejected the credentials (HTTP 401) — check [auth]",
        ));
    }
    // Throttling (the Baseline, *Throttling*, plain HTTP): the Server may answer 503 or 429 and
    // *"MAY optionally set Retry-After … The Client SHOULD honour the corresponding requirements of
    // HTTP specification."* Answered by waiting rather than by polling on regardless.
    if status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        let after = retry_after(response.headers()).unwrap_or(MIN_RETRY_INTERVAL);
        return Err(Exchange::Throttled { status, after });
    }
    // `413` is the size limit the Baseline puts on the Server's receiving side, and it adds: *"after
    // which the Client MUST NOT retry the same request."* So it is reported as a refusal of *this*
    // report rather than as a lost exchange — the difference matters, because a lost exchange arms
    // a full snapshot, which would make the next request larger than the one just refused.
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return Err(Exchange::Refused(format!(
            "the server refused the report as too large ({status}); not retrying it"
        )));
    }
    if !status.is_success() {
        return Err(Exchange::failed(&format!("the server answered {status}")));
    }
    let body = read_within(response, limit)
        .await
        .map_err(|e| Exchange::failed(&e))?;
    ServerToAgent::decode(body.as_slice())
        .map_err(|e| Exchange::failed(&format!("undecodable response: {e}")))
}

/// The header the Baseline asks a plain-HTTP Client to set, alongside the body's own field.
const INSTANCE_UID_HEADER: &str = "OpAMP-Instance-UID";

/// The Baseline's *"minimum recommended retry interval"* when the Server throttles without saying
/// for how long.
const MIN_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// How an exchange failed, because the three cases are answered differently.
pub enum Exchange {
    /// The exchange was lost — the Server may be missing state, so the next report is a full one.
    Failed(String),
    /// The Server is throttling: wait, then carry on. Not a lost exchange; nothing was processed,
    /// and nothing about the Agent's state changed.
    Throttled {
        status: reqwest::StatusCode,
        after: Duration,
    },
    /// The Server refused this report and it must not be sent again as it stands.
    Refused(String),
}

impl Exchange {
    fn failed(message: &str) -> Self {
        Exchange::Failed(message.to_string())
    }
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Exchange::Failed(message) | Exchange::Refused(message) => f.write_str(message),
            Exchange::Throttled { status, after } => {
                write!(f, "the server is throttling ({status}); waiting {after:?}")
            }
        }
    }
}

/// `Retry-After` as delta-seconds, the form a throttling Server sends.
///
/// The header's other form — an HTTP date — is deliberately not parsed: doing so means an
/// IMF-fixdate parser, and pulling a dependency in for it is an architecture-relevant decision
/// (`AGENTS.md` §3.1) that a fallback answers just as well. An unparsable value yields `None`, and
/// the caller then waits the Baseline's *"minimum recommended retry interval"* of 30 seconds — which
/// for a Server that is overloaded is the conservative reading of a header it sent to be given room.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Reads a response body, refusing one that grows past `limit`.
///
/// The Baseline requires the Client to enforce the limit on what it *receives*, after any
/// decompression — so the body is taken chunk by chunk (reqwest has already inflated gzip by
/// then) and abandoned the moment it grows too big, rather than buffered whole and measured
/// afterwards, which is the allocation the limit exists to prevent.
async fn read_within(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("cannot read the response: {e}"))?
    {
        if body.len() + chunk.len() > limit {
            return Err(format!(
                "discarding the response: it exceeds the {limit}-byte message size limit"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// The Baseline: the Client MUST NOT make a request whose body exceeds the size limit — it
    /// never reaches the network, so no server has to refuse it.
    #[tokio::test]
    async fn an_oversized_report_is_never_sent() {
        crate::tls::install_ring_provider();
        let report = AgentToServer {
            instance_uid: vec![9; 512],
            ..Default::default()
        };
        // Port 1 is unreachable: if the check did not fire first, this would fail as a connection
        // error instead — which is exactly what the assertion tells apart.
        let err = exchange(
            &reqwest::Client::new(),
            "http://127.0.0.1:1/v1/opamp",
            report,
            64,
        )
        .await
        .expect_err("the report exceeds the limit");
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    /// A stub that answers every request with one canned response and records what it was sent, so
    /// a test can assert on the request as well as on how the answer was handled.
    async fn stub(response: &'static str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut scratch = [0u8; 2048];
                let read = stream.read(&mut scratch).await.unwrap_or(0);
                recorder
                    .lock()
                    .expect("lock")
                    .push(String::from_utf8_lossy(&scratch[..read]).to_string());
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            }
        });
        (addr, seen)
    }

    fn report_for(uid: &InstanceUid) -> AgentToServer {
        AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    /// The Baseline: *"The Client SHOULD set 'OpAMP-Instance-UID' request header to the value of the
    /// instance_uid field encoded in canonical string representation of the UUID."* It is what lets
    /// an intermediary route by Agent without decoding the body.
    #[tokio::test]
    async fn the_instance_uid_rides_as_a_header() {
        crate::tls::install_ring_provider();
        let (addr, seen) =
            stub("HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
        let uid = InstanceUid::default();

        let _ = exchange(
            &reqwest::Client::new(),
            &format!("http://{addr}/v1/opamp"),
            report_for(&uid),
            8192,
        )
        .await;

        let requests = seen.lock().expect("lock");
        let request = requests.first().expect("a request reached the stub");
        let expected = format!("{INSTANCE_UID_HEADER}: {uid}").to_lowercase();
        assert!(
            request.to_lowercase().contains(&expected),
            "expected {expected:?} in:\n{request}"
        );
    }

    /// Throttling on plain HTTP: a `503` with `Retry-After` is honoured as the Server asked, and
    /// reported as a throttle rather than as a lost exchange — nothing was processed, so nothing
    /// about the Agent's state needs rebuilding.
    #[tokio::test]
    async fn a_throttling_response_is_honoured_for_the_interval_it_names() {
        crate::tls::install_ring_provider();
        let (addr, _) = stub(
            "HTTP/1.1 503 Service Unavailable\r\nretry-after: 2\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await;

        let err = exchange(
            &reqwest::Client::new(),
            &format!("http://{addr}/v1/opamp"),
            AgentToServer::default(),
            8192,
        )
        .await
        .expect_err("a throttled exchange does not produce a reply");
        match err {
            Exchange::Throttled { after, status } => {
                assert_eq!(status, reqwest::StatusCode::SERVICE_UNAVAILABLE);
                assert_eq!(after, Duration::from_secs(2));
            }
            other => panic!("expected a throttle, got {other}"),
        }
    }

    /// And `429` without a `Retry-After` falls back to the Baseline's *"minimum recommended retry
    /// interval"* rather than polling straight on.
    #[tokio::test]
    async fn throttling_without_a_hint_waits_the_recommended_minimum() {
        crate::tls::install_ring_provider();
        let (addr, _) = stub(
            "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await;

        let err = exchange(
            &reqwest::Client::new(),
            &format!("http://{addr}/v1/opamp"),
            AgentToServer::default(),
            8192,
        )
        .await
        .expect_err("a throttled exchange does not produce a reply");
        match err {
            Exchange::Throttled { after, .. } => assert_eq!(after, MIN_RETRY_INTERVAL),
            other => panic!("expected a throttle, got {other}"),
        }
    }

    /// The Baseline on the Server's receive limit: *"the Server MUST respond with HTTP 413 …, after
    /// which the Client MUST NOT retry the same request."* Reported as a refusal, which is what
    /// keeps the poll loop from arming a full snapshot — that would make the retry *larger* than the
    /// report just refused.
    #[tokio::test]
    async fn an_oversized_report_is_refused_rather_than_treated_as_a_lost_exchange() {
        crate::tls::install_ring_provider();
        let (addr, _) = stub(
            "HTTP/1.1 413 Content Too Large\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
        )
        .await;

        let err = exchange(
            &reqwest::Client::new(),
            &format!("http://{addr}/v1/opamp"),
            AgentToServer::default(),
            8192,
        )
        .await
        .expect_err("413 is not a reply");
        assert!(
            matches!(err, Exchange::Refused(_)),
            "413 must not arm a full snapshot, got {err}"
        );
    }

    /// And the receive side: a response body past the limit is discarded rather than decoded,
    /// with the limit applied as the body arrives instead of after it is buffered whole.
    #[tokio::test]
    async fn an_oversized_response_is_discarded() {
        crate::tls::install_ring_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // A minimal HTTP server: one response, 4 KiB of body, no protobuf in sight — the limit
        // decides before anything is decoded.
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch).await;
                let body = vec![0u8; 4096];
                // `connection: close` keeps each exchange on a fresh connection: this stub serves
                // one request per connection, so a pooled keep-alive would fail the next one.
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\nconnection: close\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
                let _ = stream.flush().await;
            }
        });

        let url = format!("http://{addr}/v1/opamp");
        let client = reqwest::Client::new();
        let err = exchange(&client, &url, AgentToServer::default(), 1024)
            .await
            .expect_err("the response exceeds the limit");
        assert!(err.to_string().contains("exceeds"), "{err}");

        // The same exchange with room for the body gets past the limit check and fails only on
        // the payload not being a message — proof the limit, not the transport, refused it above.
        let err = exchange(&client, &url, AgentToServer::default(), 8192)
            .await
            .expect_err("the body is not a valid message");
        assert!(err.to_string().contains("undecodable"), "{err}");
    }
}
