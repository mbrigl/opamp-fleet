//! The operator tool depends on neither end (ADR-0041 clause 22).
//!
//! What it shares with the Client is in `fleet-util`, so its `[dependencies]`, in every target
//! table, name neither `fleet-agent` nor `fleet-server`. Its `[dev-dependencies]` may: the
//! end-to-end tests run the real Server and the real Client in-process, and a development
//! dependency links into no artifact.

use std::path::Path;

/// The two ends, which the tool's shipped binary must not link.
const ENDS: &[&str] = &["fleet-agent", "fleet-server"];

/// Every end a dependency table of `manifest` names, with the table it is named in: the top-level
/// `[dependencies]` and each `[target.<cfg>.dependencies]`. Development and build dependencies
/// are not read.
fn ends_named(manifest: &str) -> Vec<String> {
    let manifest: toml::Table = manifest.parse().expect("the manifest parses");
    let mut tables = Vec::new();
    if let Some(deps) = manifest.get("dependencies").and_then(toml::Value::as_table) {
        tables.push(("[dependencies]".to_string(), deps));
    }
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for (cfg, target) in targets {
            if let Some(deps) = target.get("dependencies").and_then(toml::Value::as_table) {
                tables.push((format!("[target.{cfg}.dependencies]"), deps));
            }
        }
    }
    let mut named = Vec::new();
    for (table, deps) in tables {
        for (key, value) in deps {
            // A renamed dependency keeps its package name in `package`.
            let package = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key);
            if ENDS.contains(&package) {
                named.push(format!("{table} names {package}"));
            }
        }
    }
    named
}

/// The tool's manifest names neither end outside its development dependencies.
/// Verifies: ADR-0041
#[test]
fn the_operator_tool_depends_on_neither_end() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&path).expect("read the tool's manifest");
    let named = ends_named(&manifest);
    assert!(
        named.is_empty(),
        "{}: the operator tool depends on an end: {named:?}",
        path.display()
    );
}

/// The reader finds an end in each table that ships, under a rename too, and lets the
/// development dependencies name both.
#[test]
fn an_end_in_any_shipped_table_is_found() {
    assert!(ends_named(
        "[dependencies]\nfleet-util = { path = \"x\" }\n[dev-dependencies]\nfleet-agent = { path = \"y\" }\nfleet-server = { path = \"z\" }\n"
    )
    .is_empty());
    assert_eq!(
        ends_named("[dependencies]\nfleet-agent = { path = \"y\" }\n"),
        ["[dependencies] names fleet-agent"]
    );
    assert_eq!(
        ends_named("[target.'cfg(unix)'.dependencies]\nserver = { package = \"fleet-server\", path = \"z\" }\n"),
        ["[target.cfg(unix).dependencies] names fleet-server"]
    );
}
