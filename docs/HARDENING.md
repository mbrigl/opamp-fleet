- **Connection setup bounded on both of the Server's planes** (ADR-0023): a peer has 30 seconds to
  send its request line and headers, and 10 seconds to complete the TLS handshake, before it is hung
  up on — enforced below every other limit in this list, because it applies before a request exists
  and therefore before Admission ever runs.
### Where each bound applies today

The list above is per mechanism; this is the same state per **surface**, since a rule that holds on
one listener and not on its neighbour is the failure mode worth seeing at a glance. ✅ in force,
⚠️ partial, ❌ absent.

- **Agent plane** — `0.0.0.0:4320`, public by default (ADR-0023).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s (HTTP/1) · ✅ message size, in both directions ·
    ✅ gzip bounded *after* decompression · ✅ Admission, cumulative
  - ❌ concurrent connections: uncapped (**H16**) · ❌ HTTP/2 has no header bound (**H17**) ·
    ❌ attempt rate: unthrottled (**H10**)
- **Operator plane** — `127.0.0.1:4321` until an operator publishes it (ADR-0023).
  - ✅ TLS handshake ≤ 10 s · ✅ headers ≤ 30 s · ✅ optional Basic over the whole plane (ADR-0026) ·
    ✅ Fetch-Metadata CSRF guard on the body-less `POST` routes
  - ⚠️ the package upload is unbounded in **time** and, by decision, in size (ADR-0025) — the one
    route where that is intended · ❌ H16, H17, H10 as above
- **Client → Server**, outbound (`transport/http.rs`, `transport/ws.rs`).
  - ✅ request timeout 30 s on the polling transport · ✅ redirects refused outright ·
    ✅ reconnect backoff · ✅ message size in both directions
- **Gateway endpoint** — the Client serving OpAMP downstream (ADR-0034).
  - ✅ message size, gzip after decompression, per-hop exchange timeout, `max_carried_agents`
  - ❌ **no header-read bound**: it runs on `axum::serve` and `axum_server` without a timer, which is
    exactly the state the Server was in before ADR-0023 (**H18**)
- **Supervisor Endpoint** — loopback, one Managed Process (`supervisor/endpoint.rs`).
  - ✅ message size in both directions
  - ❌ **no handshake bound**, and connections are served one at a time by design: a local process
    that connects and never completes the WebSocket upgrade holds the endpoint against the Agent it
    exists for (**H18**)

ADR-0023)*
The listener split this measure asked for is **done**: the REST API and the UI have their own
listener (ADR-0023, superseding ADR-0025 on that point), and the OpAMP endpoint no longer shares a
port with a browser. What has *not* changed is the verifier: client authentication is still
*optional* at the TLS layer and required on the route
([`tls.rs`](../crates/server/src/tls.rs)) — and the reason is now a different one. The Agent plane
also serves the **package download**, which a Client fetches presenting no certificate (the artifact
is protected by its hash and signature, ADR-0018), so requiring one in the handshake today would
break every rollout.
**To work out:** whether the Client's downloader should present its certificate when the artifact
host is its own Server — and what that means for a `download_url` pointing at a mirror, where
sending it would be wrong — or whether the download plane gets a listener of its own. Only then can
the handshake require the certificate, so that an unauthorized peer dies before it reaches any
handler and the route check becomes a second line rather than the only one. Needs an ADR for
whichever shape wins.
ADR-0023 made each connection cheap and short-lived while it is still unproven, but not *few*:
nothing bounds how many a peer may hold open at once, and `max_agents` bounds the fleet, not the
sockets. The cap belongs at the accept loop, where refusing costs one `accept` and a close — and it
has to be a number an operator can raise, since a legitimate fleet reconnecting after a Server
with the Baseline's own answer (`retry_info`), this bounds the *number* held simultaneously, which
no protocol message expresses.

The TLS listeners offer `h2` by ALPN, and hyper's header-read timeout is HTTP/1 only — HTTP/2 has no
equivalent, because there is no header phase to time. Its analogues are `max_concurrent_streams`, the
header-list size, and keep-alive pings that evict a peer which stops answering. None is set today, so
an h2 peer is bounded by message size and by nothing else. Cheap to take, but it is a set of numbers
that wants measuring against a real fleet rather than guessing — and it is the reason ADR-0023 says
"HTTP/1" and not "the transport".

Two surfaces on the Client speak the server side of this protocol and were untouched by ADR-0023:
the **Gateway** endpoint (ADR-0034), which runs on `axum::serve` and `axum_server` with no timer
installed and is therefore in exactly the state the Server was in; and the **Supervisor Endpoint**,
which wraps `accept_async_with_config` in no timeout at all and serves connections one at a time, so
a half-finished handshake does not merely cost memory — it holds the endpoint against the Managed
Process it exists for. The Gateway half is the same three lines as ADR-0023 applied to a different
binary. The Supervisor Endpoint half is a `tokio::time::timeout` around the upgrade, and is the
cheapest item in this document.

**H18 first — it is the smallest item here and it closes a gap the Server no longer has.** ADR-0023
bounded the Server's two planes; leaving the Client's two listeners unbounded means the fleet's
weakest surface is now the one running on the hosts, and the fix is already written next door.

**Then H1 + H2 + H10 as one decision, then H3, H9, H12.** That is the largest gain in what the Server
can actually enforce, for the smallest architectural commitment — and of those, H3, H11, H12, H13,
H17, and H18 need no ADR at all.
| H9 | The listener split is in force and covered: the REST API answers on the Operator plane and `404`s on the Agent plane, and the artifact download does the reverse (ADR-0023, `auth.rs` and `packages.rs` integration tests). What remains unverified is the handshake half: a peer presenting no client certificate must fail in the **TLS handshake** on the Agent plane — an error at the transport, not a `401` from a handler — while a browser reaching the Operator plane with none is served. The distinction between those two failures is the rest of the measure. |
| H16 | Connections past the configured cap are refused while the ones already established keep working, and the cap is reached by opening sockets that send nothing — the same peer ADR-0023 hangs up on, in quantity. |
| H17 | An HTTP/2 peer that opens streams past `max_concurrent_streams` is refused, and one that stops answering keep-alive pings is dropped. Neither happens today, which is what the check must first show. |
| H18 | On the Gateway: a downstream connection that never finishes its headers is closed, exactly as [`connection_setup.rs`](../crates/server/tests/connection_setup.rs) shows for the Server. On the Supervisor Endpoint: a local connection that never completes the WebSocket upgrade is dropped, **and a second connection is served afterwards** — the second clause is the measure, since the first would pass on a listener that simply died. |
