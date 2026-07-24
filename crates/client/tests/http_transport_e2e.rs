        tokio::time::sleep(Duration::from_millis(100)).await;
fn spawn_client(config_path: &Path) -> ClientUnderTest {
    ClientUnderTest(
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
    let state_dir: PathBuf = dir.path().join("client-state");
            "[[supervisor]]\n",
            "type = \"collector\"\n",
            "name = \"otelcol\"\n",
    state
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
