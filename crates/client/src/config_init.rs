//! The first configuration file, written by `service install --interactive` (ADR-0027).
//!
//! A release artifact is the bare binary (ADR-0025), so a fresh host has nothing to copy from, and
//! [`ClientConfig::load`](crate::config::ClientConfig::load) answers a missing file with the
//! development defaults — a service that installs, starts, dials `127.0.0.1`, and manages nothing.
//! This module is the way out of that state: it asks the handful of questions a fresh host cannot
//! answer for itself, writes the file, and hands it back to the ordinary loader for validation.
//!
//! The split here is deliberate. [`ask`] is the only part that touches a terminal; [`render`] is a
//! pure function from answers to TOML, and [`write_new`] is the one that refuses to overwrite. The
//! two that decide what lands on disk are therefore testable without a tty.

use std::io::{IsTerminal, Write as _};
use std::path::{Path, PathBuf};

use dialoguer::{Confirm, Input, Password, Select};

use crate::config::ClientConfig;

/// The file name written inside the install root when the operator named no `--config` path
/// (ADR-0027): one rule for systemd, launchd, and the SCM instead of an `/etc` vs `/Library` vs
/// `%ProgramData%` policy per platform.
pub const FILE_NAME: &str = "client.toml";

/// What the questionnaire asked for. Everything else in `client.toml` has a default that is right
/// on a fresh host, and is written as a comment rather than as a value (ADR-0027).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answers {
    /// The Server's OpAMP endpoint; its scheme selects the transport (ADR-0007).
    pub endpoint: String,
    /// The operator's name for this Client, reported as `service.instance.name` (ADR-0033) — which
    /// of the fleet's Clients this is. Not its `service.name`: that is the Agent *type*, the
    /// constant `opamp-fleet-client`, the same on every host and nothing to ask about.
    pub name: String,
    /// The `[auth]` block (ADR-0013), or `None` for an endpoint that needs no credential.
    pub auth: Option<Auth>,
    /// A private CA for a `wss://` / `https://` endpoint (ADR-0007), or `None` for the built-in
    /// webpki roots.
    pub ca_file: Option<PathBuf>,
    /// The package name `[self_update]` consents to (ADR-0020), or `None` — the default, and the
    /// answer the questionnaire defaults to, because consenting to have the Client's own binary
    /// replaced is the larger grant.
    pub self_update_package: Option<String>,
}

/// The one authentication scheme the file names (ADR-0013): a bearer token, or a username and
/// password together. Never both, which is what the `[auth]` block refuses at load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    Bearer(String),
    Basic { username: String, password: String },
}

/// Ask, write, and report — the whole of `--interactive` (ADR-0027).
///
/// A file that already exists is kept, not overwritten and not merged: it may hold a credential
/// that was typed once, and a re-install that eats it is a worse failure than one that declines to
/// write. The written file is left on disk even when it fails to load afterwards, so a typo is
/// corrected by editing rather than by answering everything again.
///
/// # Errors
/// Returns an error when stdin is not a terminal, when a prompt fails, or when the file cannot be
/// created.
pub fn run(path: &Path) -> Result<(), String> {
    if path.exists() {
        println!("keeping the configuration already at {}", path.display());
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "--interactive needs a terminal on stdin, and there is none. Write {} first and \
             install without the flag — a provisioning run must not block on a question nobody \
             can answer.",
            path.display()
        ));
    }
    println!(
        "No configuration at {} — answering these writes it (everything else keeps its default).",
        path.display()
    );
    let answers = ask()?;
    write_new(path, &render(&answers))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Put the questions to the operator. The only function here that reads a terminal.
///
/// # Errors
/// Returns an error if a prompt cannot be read.
pub fn ask() -> Result<Answers, String> {
    let defaults = ClientConfig::default();

    let endpoint: String = Input::new()
        .with_prompt("Server OpAMP endpoint")
        .default(defaults.endpoint.clone())
        .validate_with(|input: &String| validate_endpoint(input))
        .interact_text()
        .map_err(prompt_failed)?;

    let name: String = Input::new()
        .with_prompt("This Agent's name (service.instance.name)")
        .default(defaults.name.clone())
        .validate_with(|input: &String| {
            if input.trim().is_empty() {
                Err("the name cannot be empty".to_string())
            } else {
                Ok(())
            }
        })
        .interact_text()
        .map_err(prompt_failed)?;

    // The credential is typed into a hidden prompt rather than passed as a flag: a flag would
    // stand in the shell history and in the process list of every host it was run on.
    let scheme = Select::new()
        .with_prompt("Authentication toward the Server")
        .items(["none", "bearer token", "username and password"])
        .default(0)
        .interact()
        .map_err(prompt_failed)?;
    let auth = match scheme {
        1 => Some(Auth::Bearer(
            Password::new()
                .with_prompt("Bearer token")
                .interact()
                .map_err(prompt_failed)?,
        )),
        2 => Some(Auth::Basic {
            username: Input::new()
                .with_prompt("Username")
                .interact_text()
                .map_err(prompt_failed)?,
            password: Password::new()
                .with_prompt("Password")
                .interact()
                .map_err(prompt_failed)?,
        }),
        _ => None,
    };

    // Only worth asking when the endpoint is one TLS applies to: a private CA behind `ws://` is a
    // question with no consequence.
    let ca_file = if is_tls_endpoint(&endpoint)
        && Confirm::new()
            .with_prompt("Does the Server present a certificate from a private CA?")
            .default(false)
            .interact()
            .map_err(prompt_failed)?
    {
        let file: String = Input::new()
            .with_prompt("PEM CA bundle path")
            .validate_with(|input: &String| {
                if Path::new(input.trim()).is_file() {
                    Ok(())
                } else {
                    Err("no readable file at that path".to_string())
                }
            })
            .interact_text()
            .map_err(prompt_failed)?;
        Some(PathBuf::from(file.trim()))
    } else {
        None
    };

    // Last, and defaulting to no. This is consent for the Server to replace the binary that
    // manages every other binary on the host (ADR-0020) — a larger grant than the rest of this
    // file put together, and one that stays a deliberate answer.
    let self_update_package =
        if Confirm::new()
            .with_prompt("Allow the Server to update this Client's own binary?")
            .default(false)
            .interact()
            .map_err(prompt_failed)?
        {
            Some(
            Input::new()
                .with_prompt("Name of the package that carries this Client")
                .default("opamp-fleet-client".to_string())
                .validate_with(|input: &String| {
                    if input.trim().is_empty() {
                        Err("the package name is the whole of the protection; it cannot be empty"
                            .to_string())
                    } else {
                        Ok(())
                    }
                })
                .interact_text()
                .map_err(prompt_failed)?,
        )
        } else {
            None
        };

    Ok(Answers {
        endpoint: endpoint.trim().to_string(),
        name: name.trim().to_string(),
        auth,
        ca_file,
        self_update_package,
    })
}

/// Render the answers as `client.toml`. Pure, so what lands on disk is testable without a tty.
///
/// Values go through `toml`'s own string encoder rather than into `"{}"`: a password may contain
/// a quote or a backslash, and a Windows CA path is full of them.
#[must_use]
pub fn render(answers: &Answers) -> String {
    let mut out = String::new();
    out.push_str(
        "# OpAMP Fleet Client configuration (ADR-0008), written by\n\
         # `opamp-fleet-client service install --interactive` (ADR-0027). It is an ordinary file\n\
         # from here on: edit it by hand, and restart the service to apply.\n\n",
    );
    out.push_str(&format!("endpoint = {}\n", toml_string(&answers.endpoint)));
    out.push_str(&format!("name = {}\n", toml_string(&answers.name)));

    if let Some(auth) = &answers.auth {
        out.push_str(
            "\n# Authentication toward the Server (ADR-0013). The Server may rotate this\n\
             # credential on its own (ADR-0014); the rotated value lives in the state directory\n\
             # and wins over what stands here.\n[auth]\n",
        );
        match auth {
            Auth::Bearer(token) => {
                out.push_str(&format!("bearer_token = {}\n", toml_string(token)));
            }
            Auth::Basic { username, password } => {
                out.push_str(&format!("username = {}\n", toml_string(username)));
                out.push_str(&format!("password = {}\n", toml_string(password)));
            }
        }
    }

    if let Some(ca) = &answers.ca_file {
        out.push_str(
            "\n# Trust for the Server's certificate (ADR-0007): this bundle *replaces* the\n\
             # built-in webpki roots.\n[tls]\n",
        );
        out.push_str(&format!(
            "ca_file = {}\n",
            toml_string(&ca.to_string_lossy())
        ));
    }

    if let Some(package) = &answers.self_update_package {
        out.push_str(
            "\n# Consent for the Server to replace this Client's own binary (ADR-0020). The name\n\
             # is the whole of the protection: an offer under any other name is refused and\n\
             # reported, never applied. Remove this section to withdraw the consent.\n\
             [self_update]\n",
        );
        out.push_str(&format!("package = {}\n", toml_string(package)));
    }

    out.push_str(
        "\n# Not asked, because these are right on a fresh host. Uncomment to change:\n\
         # poll_interval_secs = 30          # plain-HTTP polling only; WebSocket is pushed\n\
         # heartbeat_interval_secs = 30     # 0 disables heartbeats\n\
         # max_message_size_bytes = 67108864\n\
         # state_dir = \"/absolute/path\"     # an absolute path here is what the service unit\n\
         #                                  # carries; otherwise the install root's state/ is used\n\
         # supervisor_dir = \"/opt/opamp-fleet/supervisors\"   # (ADR-0021)\n\
         \n\
         # Machine-level attributes the Server's Selectors match on (ADR-0012):\n\
         # [attributes]\n\
         # env = \"prod\"\n\
         \n\
         # Processes this Client manages (ADR-0011) are added by hand, one [[supervisor]] block\n\
         # each; see docs/manual/client.md for the blocks and what each key means.\n",
    );
    out
}

/// Create the file, never replacing one that is there, and never wider than its owner on Unix.
///
/// `create_new` is what makes "do not overwrite" a property of the syscall rather than of a check
/// that raced. The mode is set in the same call for the same reason: the file may hold a bearer
/// token before any later `set_permissions` could narrow it.
///
/// # Errors
/// Returns an error if the parent cannot be created or the file already exists.
pub fn write_new(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Whether TLS applies to this endpoint, and a private CA is therefore worth asking about.
fn is_tls_endpoint(endpoint: &str) -> bool {
    let endpoint = endpoint.trim();
    endpoint.starts_with("wss://") || endpoint.starts_with("https://")
}

/// Reuse the loader's own rule for what an endpoint may look like, so the questionnaire cannot
/// accept a value the next step rejects.
fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    ClientConfig {
        endpoint: endpoint.trim().to_string(),
        ..ClientConfig::default()
    }
    .transport()
    .map(|_| ())
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn prompt_failed(e: dialoguer::Error) -> String {
    format!("cannot read the answer: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers() -> Answers {
        Answers {
            endpoint: "wss://fleet.example.com/v1/opamp".to_string(),
            name: "host-01".to_string(),
            auth: None,
            ca_file: None,
            self_update_package: None,
        }
    }

    /// The point of the whole exercise: what the questionnaire writes must load, and must load as
    /// the answers that were given.
    #[test]
    fn the_rendered_file_loads_as_what_was_answered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        let given = Answers {
            auth: Some(Auth::Bearer("a-long-random-token".to_string())),
            ca_file: Some(PathBuf::from("/etc/ssl/private-ca.pem")),
            self_update_package: Some("opamp-fleet-client".to_string()),
            ..answers()
        };
        write_new(&path, &render(&given)).expect("write");

        let loaded = ClientConfig::load(&path).expect("the written file loads");
        assert_eq!(loaded.endpoint, given.endpoint);
        assert_eq!(loaded.name, given.name);
        assert_eq!(
            loaded.authorization_value().expect("authorization"),
            Some("Bearer a-long-random-token".to_string())
        );
        assert_eq!(
            loaded.tls.expect("tls").ca_file,
            PathBuf::from("/etc/ssl/private-ca.pem")
        );
        assert_eq!(
            loaded.self_update.expect("self_update").package,
            "opamp-fleet-client"
        );
    }

    #[test]
    fn a_basic_credential_round_trips_as_the_header_it_becomes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        let given = Answers {
            auth: Some(Auth::Basic {
                username: "fleet".to_string(),
                password: "a-strong-password".to_string(),
            }),
            ..answers()
        };
        write_new(&path, &render(&given)).expect("write");

        let loaded = ClientConfig::load(&path).expect("load");
        // base64("fleet:a-strong-password")
        assert_eq!(
            loaded.authorization_value().expect("authorization"),
            Some("Basic ZmxlZXQ6YS1zdHJvbmctcGFzc3dvcmQ=".to_string())
        );
    }

    /// A password is not a well-behaved identifier. Rendering it into `"{}"` would produce a file
    /// that either fails to parse or parses as a different secret.
    #[test]
    fn quotes_and_backslashes_in_a_secret_survive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        let nasty = r#"a"b\c	d"#;
        let given = Answers {
            auth: Some(Auth::Bearer(nasty.to_string())),
            ca_file: Some(PathBuf::from(r"C:\ProgramData\opamp\ca.pem")),
            ..answers()
        };
        write_new(&path, &render(&given)).expect("write");

        let loaded = ClientConfig::load(&path).expect("load");
        assert_eq!(
            loaded.authorization_value().expect("authorization"),
            Some(format!("Bearer {nasty}"))
        );
        assert_eq!(
            loaded.tls.expect("tls").ca_file,
            PathBuf::from(r"C:\ProgramData\opamp\ca.pem")
        );
    }

    /// Nothing optional appears as an empty section: a bare `[auth]` would fail the load, and an
    /// absent `[self_update]` is what keeps the Client from accepting packages at all (ADR-0020).
    #[test]
    fn declined_sections_are_absent_rather_than_empty() {
        let rendered = render(&answers());
        assert!(!rendered.contains("\n[auth]"));
        assert!(!rendered.contains("\n[tls]"));
        assert!(!rendered.contains("\n[self_update]"));

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        write_new(&path, &rendered).expect("write");
        let loaded = ClientConfig::load(&path).expect("load");
        assert!(loaded.auth.is_none());
        assert!(loaded.tls.is_none());
        assert!(loaded.self_update.is_none());
    }

    /// The commented tail must stay comments: an operator who never touches it has a file that
    /// still loads, and the keys named there must be spelled the way the loader knows them
    /// (`deny_unknown_fields` would catch a typo the moment they uncomment one).
    #[test]
    fn the_commented_tail_is_inert_and_uncommenting_it_works() {
        let rendered = render(&answers());
        let uncommented: String = rendered
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                // Only the key lines, not the prose: the prose is indented under a key or ends in
                // a full sentence, and none of it starts with a known key.
                Some(rest)
                    if rest.starts_with("poll_interval_secs")
                        || rest.starts_with("heartbeat_interval_secs")
                        || rest.starts_with("max_message_size_bytes") =>
                {
                    rest.split('#').next().unwrap_or(rest).trim().to_string()
                }
                _ => line.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        let loaded: ClientConfig = toml::from_str(&uncommented).expect("the tail's keys are real");
        assert_eq!(loaded.poll_interval_secs, 30);
        assert_eq!(loaded.heartbeat_interval_secs, 30);
        assert_eq!(loaded.max_message_size_bytes, 67_108_864);
    }

    /// The refusal that protects a credential typed once (ADR-0027).
    #[test]
    fn an_existing_file_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        write_new(&path, "endpoint = \"ws://first/v1/opamp\"\n").expect("first write");

        let err = write_new(&path, "endpoint = \"ws://second/v1/opamp\"\n")
            .expect_err("the second write is refused");
        assert!(err.contains("cannot create"), "{err}");
        let kept = std::fs::read_to_string(&path).expect("read");
        assert!(kept.contains("first"), "the original survives: {kept}");
    }

    /// `run` on an existing file is not an error — a re-install keeps what is there and carries
    /// on, which is what makes `service install` idempotent (ADR-0010). Reached without a tty
    /// precisely because the existing-file branch returns before the terminal is consulted.
    #[test]
    fn run_keeps_an_existing_file_without_asking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        write_new(&path, "endpoint = \"ws://kept/v1/opamp\"\n").expect("write");

        run(&path).expect("an existing file is kept, not an error");
        assert!(std::fs::read_to_string(&path)
            .expect("read")
            .contains("kept"));
    }

    /// Without a terminal there is nobody to answer, and blocking a provisioning run forever is
    /// the failure mode this refuses (ADR-0027). Under `cargo test` stdin is not a tty, which is
    /// exactly the condition being asserted.
    #[test]
    fn interactive_without_a_terminal_fails_instead_of_blocking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        let err = run(&path).expect_err("no terminal, no questionnaire");
        assert!(err.contains("needs a terminal"), "{err}");
        assert!(!path.exists(), "nothing was written");
    }

    #[test]
    fn the_file_is_not_readable_by_the_rest_of_the_machine() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(FILE_NAME);
        write_new(&path, "endpoint = \"ws://x/v1/opamp\"\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "a file holding a token is the owner's alone"
            );
        }
    }

    #[test]
    fn an_endpoint_is_validated_by_the_loaders_own_rule() {
        assert!(validate_endpoint("wss://fleet.example.com/v1/opamp").is_ok());
        assert!(validate_endpoint("  http://127.0.0.1:4320/v1/opamp  ").is_ok());
        let err = validate_endpoint("fleet.example.com").expect_err("no scheme");
        assert!(err.contains("must start with"), "{err}");
    }

    #[test]
    fn a_private_ca_is_only_asked_about_where_tls_applies() {
        assert!(is_tls_endpoint("wss://x/v1/opamp"));
        assert!(is_tls_endpoint("https://x/v1/opamp"));
        assert!(!is_tls_endpoint("ws://x/v1/opamp"));
        assert!(!is_tls_endpoint("http://x/v1/opamp"));
    }
}
