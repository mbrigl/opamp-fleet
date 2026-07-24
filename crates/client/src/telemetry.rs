use crate::config::ClientConfig;

        config: &ClientConfig,
            match check(settings, "own_metrics")
                .and_then(|()| metric_provider(settings, resource.clone(), config))
            {
            match check(settings, "own_traces")
                .and_then(|()| trace_provider(settings, resource.clone(), config))
            {
                Ok(provider) => {
                    opentelemetry::global::set_tracer_provider(provider.clone());
                }
            match check(settings, "own_logs")
                .and_then(|()| log_provider(settings, resource, config))
            {
                Ok(provider) => {
                    set_bridge(Some(&provider));
                }
    // An offered certificate *is* honoured (ADR-0022 point 22) — but only its `cert`, paired with
    // the key this Client already holds. A `private_key` in the offer is a key the Server generated
    // for us, and ADR-0026's rule is that this Client's private key never leaves its host and is
    // never handed to it: that is the whole point of asking for a certificate through a CSR. Refused
    // by name rather than quietly ignored, so a Server issuing pairs learns why nothing happened.
    if settings
        .certificate
        .as_ref()
        .is_some_and(|certificate| !certificate.private_key.is_empty())
    {
        unhonoured.push("certificate.private_key");
    }
/// The HTTP client the OTLP exporters send through: this Client's TLS trust, plus the client
/// certificate the offer named, if it named one.
///
/// The certificate machinery is ADR-0026's, reused as-is (ADR-0022 point 22): the offered `cert` is
/// paired with the key already on disk — the one the CSR was made for — because that key is what
/// proves the certificate belongs to this host, and it never travels.
///
/// `ca_cert` is deliberately *not* added to the trust store. The Baseline is explicit about it:
/// *"It is not recommended that the Agent accepts this CA as an authority for any purposes."* It
/// exists so a TLS-terminating intermediary can verify the client later, not so the Agent can widen
/// whom it trusts on a Server's say-so — the same reasoning that refuses `tls`.
fn exporter_client(
    field: &str,
    config: &ClientConfig,
    let offered = settings
        .certificate
        .as_ref()
        .map(|certificate| certificate.cert.as_slice())
        .filter(|cert| !cert.is_empty());
    let builder = crate::tls::trust_and_identity_for(
        config,
        offered,
    )
    .map_err(|e| format!("{field}: {e}"))?;
        .build()
}

    config: &ClientConfig,
        .with_http_client(exporter_client(settings, "own_metrics", config)?)
    config: &ClientConfig,
        .with_http_client(exporter_client(settings, "own_traces", config)?)
    config: &ClientConfig,
        .with_http_client(exporter_client(settings, "own_logs", config)?)
    use opamp::proto::{
        any_value, AnyValue, KeyValue as ProtoKeyValue, TlsCertificate, TlsConnectionSettings,
    };
        let refused = telemetry.apply(
            &ConnectionSettingsOffers::default(),
            &description(),
            &ClientConfig::default(),
        );
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
    /// ADR-0022 point 22: the offered `certificate` is *honoured*, not refused — the ADR-0026
    /// machinery is reused as-is, which means the offered `cert` is paired with the key this Client
    /// already generated for its CSR. With that key present, an offer naming a certificate builds
    /// an exporter that presents it.
        let dir = tempfile::tempdir().expect("tempdir");
        // What the CSR flow leaves behind: the key the request was made for, and the certificate
        // the Server signed for it. Self-signed here — nothing verifies the chain in this test, the
        // point is that key and certificate pair into a usable identity.
        let key = rcgen::KeyPair::generate().expect("key");
        let params =
            rcgen::CertificateParams::new(vec!["agent.example".to_string()]).expect("params");
        let cert = params.self_signed(&key).expect("cert");
        std::fs::write(
            dir.path().join(crate::tls::ISSUED_KEY_FILE),
            key.serialize_pem(),
        )
        .expect("write key");

        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };
            own_metrics: Some(TelemetryConnectionSettings {
                destination_endpoint: "http://127.0.0.1:4318/v1/metrics".to_string(),
                certificate: Some(TlsCertificate {
                    cert: cert.pem().into_bytes(),

        let refused = telemetry.apply(&offer, &description(), &config);
    /// And an offered certificate with no key to go with it is refused *by name* rather than
    /// dropped: without the CSR key there is nothing to prove possession with, so an exporter that
    /// silently connected without the certificate would be reporting success it did not have.
    #[test]
    fn an_offered_certificate_without_its_key_is_refused_and_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };
            own_metrics: Some(TelemetryConnectionSettings {
                destination_endpoint: "http://127.0.0.1:4318/v1/metrics".to_string(),
                certificate: Some(TlsCertificate {
                    cert: b"-----BEGIN CERTIFICATE-----".to_vec(),

        let refused = telemetry.apply(&offer, &description(), &config);
        assert!(refused[0].contains("own_metrics"), "{}", refused[0]);
        assert!(refused[0].contains("no key"), "{}", refused[0]);
    /// But a private key *in the offer* is refused by name. ADR-0026's rule is that this Client's
    /// private key never leaves its host and is never handed to it — which is the whole reason the
    /// certificate is obtained through a CSR.
    #[test]
    fn an_offered_private_key_is_refused_by_name() {
            own_traces: Some(TelemetryConnectionSettings {
                destination_endpoint: "https://collector.example:4318/v1/traces".to_string(),
                certificate: Some(TlsCertificate {
                    cert: b"-----BEGIN CERTIFICATE-----".to_vec(),
                    private_key: b"-----BEGIN PRIVATE KEY-----".to_vec(),

        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert!(
            refused[0].contains("certificate.private_key"),
            "{}",
            refused[0]
        );
