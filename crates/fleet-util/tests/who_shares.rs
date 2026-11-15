//! Who shares an item chooses its crate (ADR-0041 clause 16).
//!
//! `fleet-util` holds what the Client and the operator tool share and the Server does not use;
//! `fleet-core` holds what the Server shares with either. This test reads the code under `src/` of
//! the Server, the Client and the tool, `//` comments cut, and decides from the module paths each
//! names where every module of the two crates belongs. It also refuses a `pub use` of a
//! `fleet_util::` item, so an item has one path in use, and a manifest of the Server that names
//! `fleet-util`. The tests of a crate are not read: the tool's end-to-end tests run the Client.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The code of a text, every `//` comment cut: a comment that names a module uses nothing.
fn code_of(text: &str) -> String {
    text.lines()
        .map(|line| line.find("//").map_or(line, |at| &line[..at]))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The code of every source file under `crates/<krate>/src/`, with the file it came from.
fn sources(krate: &str) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rust_files(&crates_dir().join(krate).join("src"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let text = fs::read_to_string(&file).expect("read a source file");
            (file, code_of(&text))
        })
        .collect()
}

/// The modules of `crate_name` that `code` names: the first segment after each `crate_name::`,
/// and every top-level name of a `crate_name::{…}` group.
fn modules_named(code: &str, crate_name: &str) -> BTreeSet<String> {
    let prefix = format!("{crate_name}::");
    let mut named = BTreeSet::new();
    let mut from = 0;
    while let Some(offset) = code[from..].find(&prefix) {
        let at = from + offset;
        from = at + prefix.len();
        // `xfleet_util::` is another name.
        if code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let tail = &code[from..];
        if let Some(group) = tail.strip_prefix('{') {
            let mut depth = 0;
            let mut item = String::new();
            for c in group.chars() {
                match c {
                    '{' => depth += 1,
                    '}' if depth == 0 => break,
                    '}' => depth -= 1,
                    ',' if depth == 0 => {
                        named.extend(first_segment(&item));
                        item.clear();
                    }
                    _ if depth == 0 => item.push(c),
                    _ => {}
                }
            }
            named.extend(first_segment(&item));
        } else {
            named.extend(first_segment(tail));
        }
    }
    named
}

fn first_segment(text: &str) -> Option<String> {
    let name: String = text
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty() && name != "self").then_some(name)
}

/// The modules a crate root declares with `pub mod`, behind a `cfg` too.
fn modules_of(krate: &str) -> BTreeSet<String> {
    let root = crates_dir().join(krate).join("src/lib.rs");
    code_of(&fs::read_to_string(&root).expect("read a crate root"))
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("pub mod ")?
                .strip_suffix(';')
                .map(str::to_owned)
        })
        .collect()
}

fn named_by(krate: &str, crate_name: &str) -> BTreeSet<String> {
    sources(krate)
        .iter()
        .flat_map(|(_, code)| modules_named(code, crate_name))
        .collect()
}

/// Every module of `fleet-util` is named by the Client and by the tool and not by the Server;
/// every module of `fleet-core` is named by the Server; no crate re-exports a `fleet_util::`
/// item; the Server's manifest does not name `fleet-util`.
/// Verifies: ADR-0041
#[test]
fn each_shared_module_sits_in_the_crate_its_users_choose() {
    let mut wrong = Vec::new();

    let client = named_by("fleet-agent", "fleet_util");
    let tool = named_by("fleet-tools", "fleet_util");
    let server = named_by("fleet-server", "fleet_util");
    for module in modules_of("fleet-util") {
        if !client.contains(&module) {
            wrong.push(format!("fleet_util::{module}: the Client does not name it"));
        }
        if !tool.contains(&module) {
            wrong.push(format!(
                "fleet_util::{module}: the operator tool does not name it"
            ));
        }
        if server.contains(&module) {
            wrong.push(format!(
                "fleet_util::{module}: the Server names it, so it belongs in fleet-core"
            ));
        }
    }

    let server_core = named_by("fleet-server", "fleet_core");
    for module in modules_of("fleet-core") {
        if !server_core.contains(&module) {
            wrong.push(format!(
                "fleet_core::{module}: no Server code names it, so it belongs in fleet-util"
            ));
        }
    }

    for krate in [
        "fleet-agent",
        "fleet-tools",
        "fleet-server",
        "fleet-core",
        "fleet-util",
    ] {
        for (file, code) in sources(krate) {
            for line in code.lines() {
                let line = line.trim_start();
                if line.starts_with("pub")
                    && line.contains(" use ")
                    && (line.contains("fleet_util::") || krate == "fleet-util")
                {
                    wrong.push(format!("{}: re-exports: {line}", file.display()));
                }
            }
        }
    }

    let manifest = crates_dir().join("fleet-server/Cargo.toml");
    if code_of(
        &fs::read_to_string(&manifest)
            .expect("read the Server's manifest")
            .replace('#', "//"),
    )
    .contains("fleet-util")
    {
        wrong.push(format!("{}: names fleet-util", manifest.display()));
    }

    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The reader finds a module in a plain path and in a group, across lines, and ignores a longer
/// name that ends in the crate's.
#[test]
fn the_reader_finds_the_modules_code_names() {
    let names = |code: &str| {
        modules_named(code, "fleet_util")
            .into_iter()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names("let x = fleet_util::archive::detect(p);"),
        ["archive"]
    );
    assert_eq!(
        names("use fleet_util::{\n    nofollow::{read_privileged, Named},\n    tree,\n};"),
        ["nofollow", "tree"]
    );
    assert!(names("use not_fleet_util::archive;").is_empty());
    assert_eq!(code_of("x // fleet_util::tree"), "x ");
}
