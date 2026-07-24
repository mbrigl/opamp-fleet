/// Serves both planes over TLS on ephemeral ports (ADR-0023), the Agent plane through the acceptor
/// that carries the peer certificate into the request — the thing under test. Answers with the
/// OpAMP endpoint, the Operator plane's port, and the CA a client must trust.
async fn serve(
    pki: &Pki,
    admission: Admission,
    client_ca: Option<ClientCa>,
) -> (String, u16, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent_acceptor = server::tls::PeerCertAcceptor::new(rustls_config.clone());
    let operator_acceptor = server::tls::PeerCertAcceptor::new(rustls_config);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Agent plane");
    let agents = server::agent_app(state.clone(), admission);
            .acceptor(agent_acceptor)
            .serve(agents.into_make_service())
            .expect("serve the Agent plane");
    });
    // The Operator plane, over the same certificate on its own listener (ADR-0023) — the half a
    // browser reaches, and the reason the verifier stays optional is no longer that it is here.
    let operator_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Operator plane");
    let operator_addr = operator_listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum_server::from_tcp(operator_listener)
            .acceptor(operator_acceptor)
            .serve(operators.into_make_service())
            .await
            .expect("serve the Operator plane");
    (endpoint, operator_addr.port(), pki.ca_pem.clone())
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
/// OpAMP endpoint and one that presents none is refused — while the Operator plane on its own
/// listener (ADR-0023) keeps serving the REST API to a peer with no certificate at all, and the
/// package download on the *same* listener as OpAMP stays reachable without one too. Those two are
/// why client authentication is optional at the TLS layer and required on the route (ADR-0026).
    let (endpoint, operator_port, ca_pem) = serve(&pki, Admission::new(None, true), None).await;
    // The Operator plane, on its own listener, stays reachable without one: a browser presents
    // nothing, and ADR-0026 leaves that plane open on purpose.
    let agents = format!("https://localhost:{operator_port}/api/v1/agents");

    // And on the Agent plane the artifact download is deliberately outside the guard (ADR-0023):
    // a Client fetching a package presents no certificate, so this must reach the handler — `404`
    // for a package nobody uploaded, never the `401` the OpAMP route answers above.
    let download = endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    let response = without.get(&download).send().await.expect("send");
        reqwest::StatusCode::NOT_FOUND,
        "the download must reach the handler without a certificate"
    );
    let (endpoint, _, ca_pem) = serve(&pki, Admission::new(Some(auth), true), None).await;
    let (endpoint, _, ca_pem) = serve(&pki, Admission::open(), Some(client_ca)).await;
    let (endpoint, _, ca_pem) = serve(&pki, Admission::open(), None).await;
