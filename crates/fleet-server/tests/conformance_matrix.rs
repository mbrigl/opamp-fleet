//! The capability matrix of `docs/CONFORMANCE.md` against the code (goal G-12): every capability
//! of the Baseline has a row, each row's bit is the Baseline's, and a capability is listed as
//! implemented or partial exactly when the side it belongs to declares it.
//!
//! The code is read as a syntax tree, without the items under a `cfg` that cannot hold without
//! `test`, by the reader in `tests/source`, which this crate's structural tests share.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opamp::proto::{AgentCapabilities, ServerCapabilities};
use quote::ToTokens;

mod source;
use source::{production, shipped, spaced};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The rows of the table under `heading`: capability name to (bit, status).
fn matrix(heading: &str) -> BTreeMap<String, (String, String)> {
    let text = std::fs::read_to_string(workspace().join("docs/CONFORMANCE.md")).expect("read");
    let section = text
        .split(&format!("\n## {heading}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("no section {heading:?}"));
    let section = section.split("\n## ").next().expect("a section");
    section
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            let unquote = |cell: &str| cell.trim_matches('`').to_string();
            (unquote(cells[1]), (unquote(cells[2]), cells[5].to_string()))
        })
        .collect()
}

// --- The matrix ---------------------------------------------------------------------------------

/// Whether `code`, tokens separated by whitespace, names the path `enum_name::variant`.
fn names(code: &str, enum_name: &str, variant: &str) -> bool {
    let tokens: Vec<&str> = code.split_whitespace().collect();
    tokens
        .windows(3)
        .any(|window| window == [enum_name, "::", variant])
}

/// Checks one table against one enum: `variants` are (Rust name, proto name, bit).
fn check(heading: &str, enum_name: &str, variants: &[(String, String, i32)], code: &str) {
    let rows = matrix(heading);
    let mut problems = Vec::new();
    for (rust, proto, bit) in variants {
        let Some((listed_bit, status)) = rows.get(proto) else {
            problems.push(format!("{proto} has no row"));
            continue;
        };
        let expected = format!("0x{bit:04X}");
        if !listed_bit.eq_ignore_ascii_case(&expected) {
            problems.push(format!(
                "{proto}: bit {listed_bit}, the Baseline says {expected}"
            ));
        }
        let declared = names(code, enum_name, rust);
        let listed = matches!(status.as_str(), "implemented" | "partial");
        if declared != listed {
            problems.push(format!(
                "{proto}: listed {status:?}, but the code {} it",
                if declared {
                    "declares"
                } else {
                    "never declares"
                }
            ));
        }
    }
    for name in rows.keys() {
        if !variants.iter().any(|(_, proto, _)| proto == name) {
            problems.push(format!("{name} is listed but not in the Baseline"));
        }
    }
    assert!(
        problems.is_empty(),
        "{heading}:\n  {}",
        problems.join("\n  ")
    );
}

fn variants<E: std::fmt::Debug>(
    from: impl Fn(i32) -> Option<E>,
    name: impl Fn(&E) -> &'static str,
    prefix: &str,
) -> Vec<(String, String, i32)> {
    (0..31)
        .map(|shift| 1i32 << shift)
        .filter_map(|bit| from(bit).map(|value| (value, bit)))
        .map(|(value, bit)| {
            (
                format!("{value:?}"),
                name(&value).trim_start_matches(prefix).to_string(),
                bit,
            )
        })
        .collect()
}

/// Verifies: G-12
#[test]
fn the_agent_capability_matrix_matches_what_the_client_declares() {
    check(
        "Agent capabilities",
        "AgentCapabilities",
        &variants(
            |bit| AgentCapabilities::try_from(bit).ok(),
            |value| value.as_str_name(),
            "AgentCapabilities_",
        ),
        &[
            production(&workspace().join("crates/fleet-agent/src")),
            production(&workspace().join("crates/opamp/src")),
        ]
        .concat(),
    );
}

/// Verifies: G-12
#[test]
fn the_server_capability_matrix_matches_what_the_server_declares() {
    check(
        "Server capabilities",
        "ServerCapabilities",
        &variants(
            |bit| ServerCapabilities::try_from(bit).ok(),
            |value| value.as_str_name(),
            "ServerCapabilities_",
        ),
        &production(&workspace().join("crates/fleet-server/src")),
    );
}

/// The production code of one source text, as `production` reads it.
fn shipped_code(text: &str) -> String {
    let mut out = String::new();
    spaced(
        shipped(text).expect("the fixture parses").to_token_stream(),
        &mut out,
    );
    out
}

#[test]
fn the_reader_finds_a_declaration_after_a_test_only_item() {
    let code = shipped_code(
        "struct Held {\n    #[cfg(test)]\n    dropped: HashMap<Uid, Vec<u8>>,\n    kept: u8,\n}\n\
         impl Held {\n    #[cfg(test)]\n    #[must_use]\n    fn keeps<T>(&self, uid: T) -> bool {\n        \
         if self.kept > 0 { true } else { '}' == '{' }\n    }\n\n    \
         fn capabilities() -> u64 {\n        #[cfg(test)]\n        let unused = \"}\";\n        \
         #[cfg(test)]\n        let pair = |a, b| (a, b);\n        \
         ServerCapabilities::AcceptsStatus as u64\n    }\n}\n",
    );
    assert!(
        names(&code, "ServerCapabilities", "AcceptsStatus"),
        "{code}"
    );
    assert!(code.contains("kept : u8"), "{code}");
    for test_only in ["dropped", "keeps", "unused", "pair"] {
        assert!(!code.contains(test_only), "{test_only} in {code}");
    }
}

#[test]
fn the_reader_skips_everything_inside_a_test_module() {
    let code = shipped_code(
        "pub fn serve() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    \
         #[test]\n    fn declares() {\n        let _ = ServerCapabilities::OffersRemoteConfig;\n    \
         }\n}\n\npub fn after() {}\n",
    );
    assert!(!code.contains("OffersRemoteConfig"), "{code}");
    assert!(code.contains("pub fn after"), "{code}");
}

#[test]
fn the_reader_skips_the_file_of_a_test_only_module() {
    let src = tempfile::tempdir().expect("tempdir");
    let write =
        |name: &str, text: &str| std::fs::write(src.path().join(name), text).expect("write");
    write(
        "lib.rs",
        "#[cfg(test)]\nmod fixtures;\nmod held;\n#[cfg(test)]\nmod tests {\n    mod inner;\n}\n",
    );
    write(
        "fixtures.rs",
        "const C: u64 = ServerCapabilities::OffersRemoteConfig as u64;\n",
    );
    write(
        "held.rs",
        "const C: u64 = ServerCapabilities::AcceptsStatus as u64;\n",
    );
    let code = production(src.path());
    assert!(
        !names(&code, "ServerCapabilities", "OffersRemoteConfig"),
        "{code}"
    );
    assert!(
        names(&code, "ServerCapabilities", "AcceptsStatus"),
        "{code}"
    );
}

/// A `cfg` that needs `test` among other options keeps its item out as `cfg(test)` does, the file
/// of a module included; one that holds without `test` keeps it in.
#[test]
fn the_reader_skips_code_under_cfg_all_test_and_unix() {
    let src = tempfile::tempdir().expect("tempdir");
    let write =
        |name: &str, text: &str| std::fs::write(src.path().join(name), text).expect("write");
    write(
        "lib.rs",
        "#[cfg(all(test, unix))]\nmod lock_tests;\nmod held;\n\
         #[cfg(all(test, unix))]\nmod peer {\n    \
         fn f() { let _ = ServerCapabilities::OffersPackages; }\n}\n\
         #[cfg(all(test, unix))]\npub(crate) fn skipped() { ServerCapabilities::AcceptsEffectiveConfig; }\n\
         #[cfg(any(test, unix))]\nfn kept() { ServerCapabilities::OffersConnectionSettings; }\n",
    );
    write(
        "lock_tests.rs",
        "const C: u64 = ServerCapabilities::OffersRemoteConfig as u64;\n",
    );
    write(
        "held.rs",
        "const C: u64 = ServerCapabilities::AcceptsStatus as u64;\n",
    );
    let code = production(src.path());
    for test_only in [
        "OffersRemoteConfig",
        "OffersPackages",
        "AcceptsEffectiveConfig",
    ] {
        assert!(
            !names(&code, "ServerCapabilities", test_only),
            "{test_only} in {code}"
        );
    }
    assert!(
        names(&code, "ServerCapabilities", "AcceptsStatus"),
        "{code}"
    );
    assert!(
        names(&code, "ServerCapabilities", "OffersConnectionSettings"),
        "{code}"
    );
}
