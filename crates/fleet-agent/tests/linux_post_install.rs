//! What the Linux post-install prints (`packaging/linux/postinst`), read as text the way
//! `msi_exe_command.rs` reads the WiX source: the package is not built here, so the guidance it
//! shows an operator is checked by `cargo test` on every platform.

use std::path::Path;

fn post_install() -> String {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/linux/postinst");
    std::fs::read_to_string(&script)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", script.display()))
}

/// The lines between each `<<'EOF'` and its `EOF`: what the script prints, not its comments.
fn printed(script: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in script.lines() {
        if inside {
            if line == "EOF" {
                inside = false;
            } else {
                lines.push(line);
            }
        } else if line.contains("<<'EOF'") {
            inside = true;
        }
    }
    lines
}

/// Every endpoint the post-install shows an operator is one the Client accepts at startup: the
/// startup rule (`ClientConfig::transport`, which `ClientConfig::load` applies) refuses plaintext
/// off the loopback literals, so a `ws://` or `http://localhost` example would teach a value the
/// service then refuses. A placeholder host stands in for the operator's Server.
/// Verifies: ADR-0047
#[test]
fn the_post_install_prints_no_endpoint_the_client_refuses_at_startup() {
    let script = post_install();
    let printed = printed(&script);
    assert!(!printed.is_empty(), "the post-install prints its guidance");
    for endpoint in printed
        .iter()
        .flat_map(|line| line.split_whitespace())
        .filter(|word| word.contains("://"))
    {
        let endpoint = endpoint
            .trim_matches(|c: char| c == '`' || c == '\'' || c == '"')
            .replace("<server>", "fleet.example.com");
        fleet_agent::config::ClientConfig {
            endpoint: endpoint.clone(),
            ..Default::default()
        }
        .transport()
        .unwrap_or_else(|e| panic!("the post-install prints {endpoint}, refused at startup: {e}"));
    }
}

/// The first step the post-install prints is the questionnaire, which asks for the credential and
/// the certificate; an endpoint alone is never offered as a complete step, since a Client with
/// nothing more refuses to start (ADR-0047 clause 13).
/// Verifies: ADR-0047
#[test]
fn the_post_install_steps_ask_for_the_credential() {
    let script = post_install();
    let printed = printed(&script);
    let first_step = printed
        .iter()
        .find(|line| line.trim_start().starts_with("opamp-fleet service install"))
        .expect("a step that writes the configuration");
    assert!(first_step.contains("--interactive"), "{first_step}");
    assert!(
        !printed.iter().any(|line| line.contains("--endpoint")),
        "an endpoint-only install is not offered as a step"
    );
    let text = printed.join("\n");
    assert!(
        text.contains("[auth]") && text.contains("cert_file"),
        "{text}"
    );
}
