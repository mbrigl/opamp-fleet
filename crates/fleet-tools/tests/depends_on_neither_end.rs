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

/// The packages `root` links through normal dependencies alone, read from `cargo metadata`'s
/// resolved graph, each with the chain that reaches it.
fn linked(metadata: &serde_json::Value, root: &str) -> Vec<(String, Vec<String>)> {
    let name_of = |id: &str| -> String {
        metadata["packages"]
            .as_array()
            .expect("packages")
            .iter()
            .find(|p| p["id"] == id)
            .and_then(|p| p["name"].as_str())
            .unwrap_or(id)
            .to_string()
    };
    let nodes = metadata["resolve"]["nodes"].as_array().expect("nodes");
    let start = nodes
        .iter()
        .find(|n| name_of(n["id"].as_str().expect("id")) == root)
        .expect("the root is a workspace package");
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = vec![(start, vec![root.to_string()])];
    let mut out = Vec::new();
    while let Some((node, chain)) = queue.pop() {
        for dep in node["deps"].as_array().expect("deps") {
            let normal = dep["dep_kinds"]
                .as_array()
                .expect("dep_kinds")
                .iter()
                .any(|k| k["kind"].is_null());
            let id = dep["pkg"].as_str().expect("pkg");
            if !normal || !seen.insert(id.to_string()) {
                continue;
            }
            let mut next = chain.clone();
            next.push(name_of(id));
            out.push((name_of(id), next.clone()));
            if let Some(child) = nodes.iter().find(|n| n["id"] == id) {
                queue.push((child, next));
            }
        }
    }
    out
}

/// What the tool links, through any chain of normal dependencies on the host's platform, includes
/// neither end: a workspace crate the tool depends on cannot bring one in either
/// (ADR-0041 clause 22). The manifest test above covers the tool's tables for every platform.
/// Verifies: ADR-0041
#[test]
fn the_operator_tool_links_neither_end() {
    let rustc = std::process::Command::new(std::env::var("RUSTC").unwrap_or("rustc".into()))
        .arg("-vV")
        .output()
        .expect("run rustc");
    let host = String::from_utf8(rustc.stdout)
        .expect("utf-8")
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_owned))
        .expect("the host triple");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    // Filtered to the host, the graph needs no package that is not already downloaded.
    let output = std::process::Command::new(std::env::var("CARGO").unwrap_or("cargo".into()))
        .args(["metadata", "--format-version", "1", "--locked", "--filter-platform", &host])
        .arg("--manifest-path")
        .arg(&workspace)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).expect("metadata");
    let ends: Vec<String> = linked(&metadata, "fleet-tools")
        .into_iter()
        .filter(|(name, _)| ENDS.contains(&name.as_str()))
        .map(|(_, chain)| chain.join(" → "))
        .collect();
    assert!(ends.is_empty(), "the operator tool links an end: {ends:?}");
}

/// The walk follows a chain through another package and skips development dependencies.
#[test]
fn a_chain_through_another_package_is_followed() {
    let normal = serde_json::json!([{ "kind": null, "target": null }]);
    let dev = serde_json::json!([{ "kind": "dev", "target": null }]);
    let metadata = serde_json::json!({
        "packages": [
            { "id": "t", "name": "fleet-tools" }, { "id": "u", "name": "fleet-util" },
            { "id": "s", "name": "fleet-server" }, { "id": "a", "name": "fleet-agent" }
        ],
        "resolve": { "nodes": [
            { "id": "t", "deps": [
                { "pkg": "u", "dep_kinds": normal }, { "pkg": "a", "dep_kinds": dev }
            ] },
            { "id": "u", "deps": [{ "pkg": "s", "dep_kinds": normal }] },
            { "id": "s", "deps": [] }, { "id": "a", "deps": [] }
        ] }
    });
    let names: Vec<String> = linked(&metadata, "fleet-tools")
        .into_iter()
        .map(|(name, chain)| format!("{name}: {}", chain.join(" → ")))
        .collect();
    assert_eq!(
        names,
        [
            "fleet-util: fleet-tools → fleet-util",
            "fleet-server: fleet-tools → fleet-util → fleet-server"
        ]
    );
}
