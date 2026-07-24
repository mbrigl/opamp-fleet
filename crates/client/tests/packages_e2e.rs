        tokio::time::sleep(Duration::from_millis(100)).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
