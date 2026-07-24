    let server = spawn().await;
    let client = reqwest::Client::new();
        .get(url(server.rest_addr, "/api/v1/configurations"))
        .put(url(server.rest_addr, "/api/v1/configurations/base"))
        .get(url(server.rest_addr, "/api/v1/configurations/base"))
        .get(url(server.rest_addr, "/api/v1/configurations"))
        .delete(url(server.rest_addr, "/api/v1/configurations/base"))
        .delete(url(server.rest_addr, "/api/v1/configurations/base"))
        .get(url(server.rest_addr, "/api/v1/configurations/base"))
    let server = spawn().await;
    let client = reqwest::Client::new();
        .put(url(server.rest_addr, "/api/v1/configurations/base"))
        .put(url(server.rest_addr, "/api/v1/configurations/ruleset"))
        .get(url(server.rest_addr, "/api/v1/configurations/ruleset"))
        .put(url(server.rest_addr, "/api/v1/configurations/other"))
        .put(url(server.rest_addr, "/api/v1/configurations/fleet"))
    let view = agent_view(&client, server.rest_addr, &uid).await;
        .post(url(
            server.rest_addr,
            "/api/v1/configurations/fleet/rollout",
        ))
    let view = agent_view(&client, server.rest_addr, &uid).await;
        .put(url(server.rest_addr, "/api/v1/configurations/fleet"))
    let view = agent_view(&client, server.rest_addr, &uid).await;
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{uid}/rollout"),
        ))
        .post(url(
            server.rest_addr,
            "/api/v1/configurations/missing/rollout",
        ))
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{uid}/rollout"),
        ))
        .put(url(server.rest_addr, "/api/v1/configurations/fleet"))
        .post(url(
            server.rest_addr,
            "/api/v1/configurations/fleet/rollout",
        ))
    let view = agent_view(&client, server.rest_addr, &late).await;
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{late}/rollout"),
        ))
            .put(url(
                server.rest_addr,
                &format!("/api/v1/configurations/{name}"),
            ))
        matched_configurations(&client, server.rest_addr, &uid).await,
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{uid}/rollout"),
        ))
                server.rest_addr,
    let server = spawn().await;
    let response = reqwest::Client::new()
        .get(url(server.rest_addr, "/api/v1/openapi.json"))
    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
    let server = spawn().await;
    let client = reqwest::Client::new();
        .get(url(server.rest_addr, "/api/v1/docs"))
        .get(url(server.rest_addr, "/api/v1/docs/redoc.js"))
        .delete(url(server.rest_addr, &format!("/api/v1/agents/{uid}")))
    assert_eq!(
        agents(&client, server.rest_addr).await.len(),
        1,
        "the row stays"
    );
    assert_eq!(agents(&client, server.rest_addr).await.len(), 1);
        .delete(url(server.rest_addr, &format!("/api/v1/agents/{uid}")))
        agents(&client, server.rest_addr).await.is_empty(),
        agents(&client, server.rest_addr).await.len(),
    let server = spawn().await;
    let client = reqwest::Client::new();
            server.rest_addr,
        .delete(url(server.rest_addr, "/api/v1/agents/not-a-uid"))
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{uid}/restart"),
        ))
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{uid}/restart"),
        ))
        .post(url(
            server.rest_addr,
            &format!("/api/v1/agents/{uid}/restart"),
        ))
        .put(url(server.rest_addr, "/api/v1/configurations/canary"))
        matched_configurations(&client, server.rest_addr, &uid)
        server.rest_addr,
        matched_configurations(&client, server.rest_addr, &uid).await,
        set_labels(&client, server.rest_addr, &uid, serde_json::json!({}))
    assert!(matched_configurations(&client, server.rest_addr, &uid)
        server.rest_addr,
            server.rest_addr,
    let agents = agents(&client, server.rest_addr).await;
    let view = &agents(&client, server.rest_addr).await[0];
    let view = &agents(&client, server.rest_addr).await[0];
    let view = &agents(&client, server.rest_addr).await[0];
            server.rest_addr,
        .delete(url(server.rest_addr, &format!("/api/v1/agents/{uid}")))
    assert!(agents(&client, server.rest_addr).await.is_empty());
    let agents = agents(&client, server.rest_addr).await;
