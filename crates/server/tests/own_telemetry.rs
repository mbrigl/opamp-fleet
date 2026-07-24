    let reply = exchange(&server, report(AgentCapabilities::ReportsOwnMetrics as u64)).await;

    assert!(
        reply.capabilities & opamp::proto::ServerCapabilities::OffersConnectionSettings as u64 != 0,
/// ADR-0013 clause 3: a Server that can offer *anything* declares `OffersConnectionSettings`.
/// Keying the bit on `[connection_offer]` alone left this Server exercising a capability it had not
/// declared — and a Client that took the bitmask literally would then have withheld the
/// acknowledgement, leaving the hash gate open and this offer repeating for ever.
#[tokio::test]
async fn a_telemetry_only_server_declares_that_it_offers_connection_settings() {
    let reply = exchange(&server, report(AgentCapabilities::ReportsOwnMetrics as u64)).await;

    assert!(
        reply.capabilities & opamp::proto::ServerCapabilities::OffersConnectionSettings as u64 != 0,
        "the offer below is exactly the capability being exercised"
    );
    assert!(reply.connection_settings.is_some());
}

