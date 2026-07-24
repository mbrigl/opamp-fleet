/// `[auth]` guards the OpAMP endpoint and nothing else: the REST API answers on the Operator
/// plane's own listener (ADR-0012), where authenticating it is the separate decision — and it is
/// *not* served on the Agent plane at all, which is what the split is.
async fn the_rest_api_stays_open_on_its_own_listener_when_the_opamp_endpoint_is_guarded() {
        .get(format!("http://{}/api/v1/agents", server.rest_addr))

    let on_the_agent_plane = client
        on_the_agent_plane.status(),
        404,
        "the fleet view is not served where the Agents connect"
    );
