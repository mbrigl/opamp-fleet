//! What the operator tool takes from the Client: only the items its ADR names
//! (ADR-0008 clause 22).
//!
//! The tool depends on `fleet-agent`, so the compiler lets it name anything the Client makes
//! public. This test reads the tool's own code under `src/` and holds every `fleet_agent::` path
//! it names to the clause's list, so a new reach into the Client is a decision, not a drift. The
//! integration tests under `tests/` may run the Client itself and are not read.

use std::fs;
use std::path::{Path, PathBuf};

/// The Client's items clause 22 names. A path is allowed when it is one of these or lies below
/// one of `ALLOWED_MODULES`.
const ALLOWED: &[&str] = &[
    "fleet_agent::archive",
    "fleet_agent::tls::pkcs12_identity",
    "fleet_agent::nofollow::read_privileged",
    "fleet_agent::nofollow::Named",
    "fleet_agent::config_init::read_password_file",
];

/// What clause 22 names as a whole, a module or a type: its items or variants are allowed too.
const ALLOWED_MODULES: &[&str] = &["fleet_agent::archive", "fleet_agent::nofollow::Named"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read the tool's sources") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The code of a line, without a `//` comment: a comment that names an item uses nothing.
fn code_of(line: &str) -> &str {
    line.find("//").map_or(line, |at| &line[..at])
}

/// The code of a file, every `//` comment cut, with its lines kept so a position names its line.
fn code_of_file(text: &str) -> String {
    text.lines().map(code_of).collect::<Vec<_>>().join("\n")
}

/// The text inside the braces that open at the start of `text`, up to the brace that closes them.
fn braced(text: &str) -> &str {
    let mut depth = 0;
    for (at, c) in text.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[1..at];
                }
            }
            _ => {}
        }
    }
    &text[1.min(text.len())..]
}

/// The items of a group, split at the commas outside any inner braces.
fn items_of(group: &str) -> Vec<&str> {
    let (mut items, mut depth, mut from) = (Vec::new(), 0, 0);
    for (at, c) in group.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                items.push(group[from..at].trim());
                from = at + 1;
            }
            _ => {}
        }
    }
    items.push(group[from..].trim());
    items.into_iter().filter(|item| !item.is_empty()).collect()
}

/// The full paths a path with an optional group after it names: `a::{b, c::{d}}` under `prefix`.
fn expand(prefix: &str, text: &str, found: &mut Vec<String>) {
    let path: String = text
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    let rest = text[path.len()..].trim_start();
    let full = format!("{prefix}{path}");
    if rest.starts_with('{') && (path.is_empty() || path.ends_with("::")) {
        for item in items_of(braced(rest)) {
            expand(&full, item, found);
        }
    } else {
        // `a as b` names `a`.
        found.push(full.trim_end_matches(':').to_string());
    }
}

/// Every `fleet_agent::…` path the code names, with the position it starts at: a grouped
/// `use fleet_agent::{a, b::{c, d}}` expanded into one path per item, across lines too.
fn paths_in(code: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = code[from..].find("fleet_agent") {
        let at = from + offset;
        from = at + "fleet_agent".len();
        let before = code[..at].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let tail = &code[at..];
        let mut paths = Vec::new();
        match tail.strip_prefix("fleet_agent::") {
            Some(after) => expand("fleet_agent::", after, &mut paths),
            // `use fleet_agent;` or `fleet_agent as x` would hide every later path.
            None => paths.push(tail.split_whitespace().next().unwrap_or(tail).to_string()),
        }
        found.extend(paths.into_iter().map(|path| (at, path)));
    }
    found
}

/// The paths alone, for the reader's own test.
fn names_in(code: &str) -> Vec<String> {
    paths_in(code).into_iter().map(|(_, path)| path).collect()
}

fn allowed(path: &str) -> bool {
    ALLOWED.contains(&path)
        || ALLOWED_MODULES
            .iter()
            .any(|m| path.strip_prefix(m).is_some_and(|r| r.starts_with("::")))
}

/// Verifies: ADR-0008
#[test]
fn the_operator_tool_uses_only_the_client_items_its_adr_names() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let mut outside = Vec::new();
    let mut seen = 0;
    for file in &files {
        let code = code_of_file(&fs::read_to_string(file).expect("read a source file"));
        for (at, path) in paths_in(&code) {
            seen += 1;
            if !allowed(&path) {
                let line = code[..at].matches('\n').count() + 1;
                outside.push(format!("{}:{line}: {path}", file.display()));
            }
        }
    }
    assert!(
        seen > 0,
        "the tool names no item of the Client: the reader is broken"
    );
    assert!(
        outside.is_empty(),
        "the operator tool names Client items its ADR does not (clause 22):\n{}",
        outside.join("\n")
    );
}

/// The reader finds what code can name, and lets an allowed item pass.
/// Verifies: ADR-0008
#[test]
fn the_reader_finds_every_form_a_path_takes() {
    assert_eq!(
        names_in("fleet_agent::archive::extract(x)"),
        ["fleet_agent::archive::extract"]
    );
    assert_eq!(
        names_in("use fleet_agent::{tls::pkcs12_identity, shutdown};"),
        ["fleet_agent::tls::pkcs12_identity", "fleet_agent::shutdown"]
    );
    assert_eq!(
        names_in("use fleet_agent::nofollow::{read_privileged, Named};"),
        [
            "fleet_agent::nofollow::read_privileged",
            "fleet_agent::nofollow::Named"
        ]
    );
    // A group rustfmt wraps over several lines, a nested group inside it.
    let wrapped = code_of_file(
        "use fleet_agent::{\n    shutdown::shutdown_channel, // why\n    tls::{pkcs12_identity, x},\n};",
    );
    assert_eq!(
        names_in(&wrapped),
        [
            "fleet_agent::shutdown::shutdown_channel",
            "fleet_agent::tls::pkcs12_identity",
            "fleet_agent::tls::x"
        ]
    );
    assert_eq!(
        names_in("use fleet_agent::archive as a;"),
        ["fleet_agent::archive"]
    );
    assert_eq!(names_in("use fleet_agent as client;"), ["fleet_agent"]);
    assert!(names_in("my_fleet_agent::x").is_empty());
    assert!(names_in(code_of("// fleet_agent::packages::verify")).is_empty());
    assert!(allowed("fleet_agent::archive::unix_mode_attributes"));
    assert!(allowed("fleet_agent::archive"));
    assert!(allowed("fleet_agent::nofollow::Named::ByAdministrator"));
    assert!(!allowed("fleet_agent::archives"));
    assert!(!allowed("fleet_agent::shutdown::shutdown_channel"));
    assert!(!allowed("fleet_agent"));
}
