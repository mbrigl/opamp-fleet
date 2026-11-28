//! The Plugin port is plain data (CLT-wrappers-act-through-the-core §2).
//!
//! What it decides: no type of `supervisor::port`, nor the struct of another crate it embeds,
//! names a host resource: a path, a handle, a channel, a task, a span, a time read or a secret. A
//! WebAssembly component can then implement the port later. The test reads the port's shipped
//! syntax through the Server's reader of shipped code, so a comment, a string or a test is no
//! dependency.
//!
//! Why it lives here: the rule binds the Client's port, so `cargo test -p fleet-agent` decides it
//! ("A test runs with the crate it decides" in `docs/CONVENTIONS.md`). It reads `fleet-util`'s
//! `tree.rs` by path, as the port embeds its `KeptLink`.

use std::fs;
use std::path::Path;

// The Server's reader of shipped code, which cuts what only the tests build.
#[path = "../../fleet-server/tests/source/mod.rs"]
mod source;

/// The names a type of the Plugin port must not hold: a host path, a file or other handle, a
/// channel, a task, a span, a time read or a secret (CLT-wrappers-act-through-the-core §2).
const HOST_RESOURCES: &[&str] = &[
    "PathBuf",
    "Path",
    "OsString",
    "OsStr",
    "File",
    "TcpStream",
    "Command",
    "Child",
    "mpsc",
    "oneshot",
    "watch",
    "Sender",
    "Receiver",
    "JoinHandle",
    "Span",
    "Instant",
    "SystemTime",
    "Zeroizing",
    "Shutdown",
    "Arc",
    "Mutex",
];

/// Every name of a path in `file`'s shipped syntax, with the struct `only` alone where it is
/// given: the code, never a comment, a string or a test.
fn path_names(file: &syn::File, only: Option<&str>) -> Vec<String> {
    use syn::visit::Visit;
    struct Names(Vec<String>);
    impl<'ast> Visit<'ast> for Names {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            self.0.push(segment.ident.to_string());
            syn::visit::visit_path_segment(self, segment);
        }
    }
    let mut names = Names(Vec::new());
    match only {
        None => names.visit_file(file),
        Some(only) => {
            for item in &file.items {
                if let syn::Item::Struct(item) = item {
                    if item.ident == only {
                        names.visit_item_struct(item);
                    }
                }
            }
        }
    }
    names.0
}

/// The host resources among `names`.
fn host_resources(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|n| HOST_RESOURCES.contains(&n.as_str()))
        .cloned()
        .collect()
}

/// The Plugin port is plain data: no type of it, nor the struct of another crate it embeds, names
/// a host resource, so a WebAssembly component can implement it.
/// Verifies: CLT-wrappers-act-through-the-core, Q-5
#[test]
fn the_port_carries_no_host_resource() {
    let read = |path: &str| {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        source::shipped(&fs::read_to_string(crates.join(path)).unwrap()).unwrap()
    };
    let mut names = path_names(&read("fleet-agent/src/supervisor/port.rs"), None);
    names.extend(path_names(
        &read("fleet-util/src/tree.rs"),
        Some("KeptLink"),
    ));
    assert!(names.iter().any(|n| n == "Manifest"), "the port is read");
    let found = host_resources(&names);
    assert!(found.is_empty(), "the port names host resources: {found:?}");
}

/// The reading above finds a host resource held by a port type, and not one in a comment, a
/// string or a test, so its silence on the port means something.
#[test]
fn the_port_rule_finds_a_host_resource() {
    for (field, resource) in [
        ("std::path::PathBuf", "PathBuf"),
        ("std::fs::File", "File"),
        ("tokio::sync::mpsc::Sender<u8>", "Sender"),
        ("tracing::Span", "Span"),
        ("zeroize::Zeroizing<String>", "Zeroizing"),
    ] {
        let file = source::shipped(&format!("pub enum Input {{ A {{ x: {field} }} }}")).unwrap();
        assert_eq!(
            host_resources(&path_names(&file, None))
                .last()
                .map(String::as_str),
            Some(resource)
        );
    }
    let quiet = source::shipped(
        "/// A `PathBuf` in a comment.\n\
         pub struct S { s: &'static str }\n\
         const T: &str = \"Span\";\n\
         #[cfg(test)] mod tests { struct P(std::path::PathBuf); }",
    )
    .unwrap();
    assert!(host_resources(&path_names(&quiet, None)).is_empty());
    let only = source::shipped("pub struct A(std::fs::File); pub struct B(u8);").unwrap();
    assert!(host_resources(&path_names(&only, Some("B"))).is_empty());
}
