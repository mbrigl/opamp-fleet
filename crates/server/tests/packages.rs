    let (addr, rest_addr) =
        support::serve(state.clone(), server::transport::Admission::open()).await;
            rest_addr,
    )
}

/// The artifact download of one Set — on the **Agent plane** (ADR-0023), which is where the
/// `download_url` in an offer points and the one `/api/v1` route the Operator plane does not serve.
    format!(
            "{}?os=linux&arch=amd64",
/// ADR-0023: the offered `download_url` is a path the Client resolves against **its own OpAMP
/// endpoint**, so the artifact has to be served by the listener the Agents already talk to — not by
/// the Operator plane, which is where authentication is going and where no Agent will ever look.
#[tokio::test]
async fn the_artifact_is_served_where_the_agents_are_and_not_on_the_operator_plane() {
    let path = format!(
        support::AGENT_TYPE
    );

        .get(format!("http://{}{path}", server.addr))
    assert_eq!(served.status(), 200);
    assert_eq!(
        served.bytes().await.expect("bytes").as_ref(),
        b"the-new-binary"
    );

    let elsewhere = reqwest::Client::new()
        .get(format!("http://{}{path}", server.rest_addr))
        .send()
        .await
        .expect("request");
    assert_eq!(
        elsewhere.status(),
        404,
        "one resource, one address: the Operator plane does not serve artifacts"
    );
}

            "{}?os=linux&arch=amd64",
    let (addr, rest_addr) =
        support::serve(state.clone(), server::transport::Admission::open()).await;
        rest_addr,
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
            server.rest_addr
            server.rest_addr
            server.rest_addr
            .get(format!("http://{}/api/v1/packages", server.rest_addr))
    let (addr, rest_addr) =
        support::serve(state.clone(), server::transport::Admission::open()).await;
        rest_addr,
