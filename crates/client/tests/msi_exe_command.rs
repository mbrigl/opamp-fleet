//! The MSI's custom-action command lines, parsed the way Windows will parse them
//! (`packaging/windows/opamp-fleet-client.wxs`, ADR-0046).
//!
//! Regression test. `[INSTALLFOLDER]` always resolves with a trailing backslash, and the C
//! runtime that builds a process's argv treats a backslash before a quote as an escaped, literal
//! quote — so `--root &quot;[INSTALLFOLDER]&quot;` did not end the argument at the closing quote.
//! The root swallowed the rest of the command line, `service install` staged into an impossible
//! path, and every MSI install died with error 1722 ("A program run as part of the setup did not
//! finish as expected"). The `.wxs` now doubles the backslash; this test formats each ExeCommand
//! the way msiexec does, splits it under the CRT's documented rules, and feeds the result to the
//! real CLI parser — pure string handling, so the Windows failure is caught on every platform.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use client::cli::{self, Command, ServiceAction};

/// A realistic resolution of `ProgramFiles64Folder\OpAMP Fleet Client`: spaces, and — the whole
/// point — the trailing backslash every Windows Installer directory property carries.
const INSTALLFOLDER: &str = r"C:\Program Files\OpAMP Fleet Client\";
const ENDPOINT: &str = "wss://fleet.example.com/v1/opamp";
const PROGRAM: &str = r"C:\Program Files\OpAMP Fleet Client\opamp-fleet-client.exe";

/// Split a command line into arguments by the C runtime's rules ("Parsing C++ command-line
/// arguments"): whitespace separates arguments outside quotes; 2n backslashes before a quote
/// become n backslashes and the quote opens/closes; 2n+1 backslashes before a quote become n
/// backslashes and a literal quote; backslashes not before a quote are literal.
fn split_as_crt(line: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut arg = String::new();
    let mut in_arg = false;
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                in_arg = true;
                let mut n = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    n += 1;
                }
                if chars.peek() == Some(&'"') {
                    arg.push_str(&"\\".repeat(n / 2));
                    if n % 2 == 1 {
                        chars.next();
                        arg.push('"');
                    }
                    // n even: the quote stays for the next iteration and opens/closes there.
                } else {
                    arg.push_str(&"\\".repeat(n));
                }
            }
            '"' => {
                in_arg = true;
                if in_quotes && chars.peek() == Some(&'"') {
                    chars.next();
                    arg.push('"');
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ' ' | '\t' if !in_quotes => {
                if in_arg {
                    args.push(std::mem::take(&mut arg));
                    in_arg = false;
                }
            }
            _ => {
                in_arg = true;
                arg.push(c);
            }
        }
    }
    if in_arg {
        args.push(arg);
    }
    args
}

/// The splitter against the reference table in Microsoft's "Parsing C++ command-line arguments" —
/// what makes the assertions below evidence about the `.wxs` rather than about this file.
#[test]
fn splitter_follows_the_crt_reference_table() {
    assert_eq!(split_as_crt(r#""abc" d e"#), ["abc", "d", "e"]);
    assert_eq!(split_as_crt(r#"a\\\b d"e f"g h"#), [r"a\\\b", "de fg", "h"]);
    assert_eq!(split_as_crt(r#"a\\\"b c d"#), [r#"a\"b"#, "c", "d"]);
    assert_eq!(split_as_crt(r#"a\\\\"b c" d e"#), [r"a\\b c", "d", "e"]);
}

fn package_source() -> String {
    let wxs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/windows/opamp-fleet-client.wxs");
    std::fs::read_to_string(&wxs).unwrap_or_else(|e| panic!("cannot read {}: {e}", wxs.display()))
}

/// Every `ExeCommand` in the package source, keyed by its custom action's `Id`, with the XML
/// entities decoded.
fn exe_commands() -> BTreeMap<String, String> {
    package_source()
        .split("<CustomAction")
        .skip(1)
        .filter_map(|block| {
            let element = &block[..block.find('>').expect("unterminated CustomAction element")];
            Some((attribute(element, "Id")?, attribute(element, "ExeCommand")?))
        })
        .collect()
}

fn attribute(element: &str, name: &str) -> Option<String> {
    let opener = format!("{name}=\"");
    let start = element.find(&opener)? + opener.len();
    let value = &element[start..start + element[start..].find('"')?];
    Some(
        value
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&"),
    )
}

/// A type 18 custom action's command line is the quoted file followed by the ExeCommand, with the
/// bracketed properties formatted in.
fn parse(exe_command: &str) -> cli::Parsed {
    let line = format!("\"{PROGRAM}\" {exe_command}")
        .replace("[INSTALLFOLDER]", INSTALLFOLDER)
        .replace("[ENDPOINT]", ENDPOINT);
    cli::parse_from(split_as_crt(&line)).unwrap_or_else(|e| panic!("the CLI refuses `{line}`: {e}"))
}

fn install_args(parsed: cli::Parsed) -> cli::InstallArgs {
    match parsed.cli.command {
        Some(Command::Service {
            action: ServiceAction::Install(args),
        }) => args,
        other => panic!("expected `service install`, parsed {other:?}"),
    }
}

#[test]
fn register_service_with_endpoint_survives_the_crt() {
    let args = install_args(parse(&exe_commands()["RegisterServiceWithEndpoint"]));
    assert_eq!(args.root, Some(PathBuf::from(INSTALLFOLDER)));
    assert_eq!(args.endpoint.as_deref(), Some(ENDPOINT));
    assert!(!args.interactive);
}

#[test]
fn register_service_survives_the_crt() {
    let args = install_args(parse(&exe_commands()["RegisterService"]));
    assert_eq!(args.root, Some(PathBuf::from(INSTALLFOLDER)));
    assert_eq!(args.endpoint, None);
}

/// The endpoint prefill (ADR-0049): the development Server in its `http://` form, held to the
/// loader's own endpoint rule so the dialog can never offer a value that `service install
/// --endpoint` would then reject. And it must stay confined to the UI sequence: leaking it into a
/// silent install would write the development default on every unattended host, the state
/// ADR-0046 refuses to manufacture.
#[test]
fn endpoint_prefill_is_the_development_server_and_interactive_only() {
    let source = package_source();
    let element = source
        .split("<SetProperty")
        .nth(1)
        .expect("no ENDPOINT prefill in the package source");
    let element = &element[..element.find('>').expect("unterminated SetProperty element")];
    assert_eq!(attribute(element, "Id").as_deref(), Some("ENDPOINT"));
    let value = attribute(element, "Value").expect("the prefill has no Value");
    assert_eq!(value, "http://localhost:4320/v1/opamp");
    client::config::ClientConfig {
        endpoint: value,
        ..Default::default()
    }
    .transport()
    .expect("the loader rejects the prefilled endpoint");
    assert_eq!(attribute(element, "Sequence").as_deref(), Some("ui"));
}

#[test]
fn stop_and_unregister_survive_the_crt() {
    let commands = exe_commands();
    assert!(matches!(
        parse(&commands["StopService"]).cli.command,
        Some(Command::Service {
            action: ServiceAction::Stop(_)
        })
    ));
    assert!(matches!(
        parse(&commands["UnregisterService"]).cli.command,
        Some(Command::Service {
            action: ServiceAction::Uninstall(_)
        })
    ));
}
