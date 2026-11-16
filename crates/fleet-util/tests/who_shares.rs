//! Who shares an item chooses its crate (ADR-0041 clause 16).
//!
//! `fleet-util` holds what the Client and the operator tool share and the Server does not use;
//! `fleet-core` holds what the Server shares with either. This test reads the code under `src/` of
//! the Server, the Client and the tool, without comments, string and character literals, and code
//! under a `cfg` that cannot hold without `test`, and decides from the module paths each names
//! where every module of the two crates belongs. It also refuses a `pub use` of a `fleet_util::`
//! item and an alias of the crate, `self as` in a group included, so an item has one path in use;
//! a manifest of the Server that names `fleet-util`; and a manifest of `fleet-util` that names a
//! workspace crate but `fleet-core`. The tests of a crate are not read: the tool's end-to-end tests
//! run the Client.

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

/// The code of a text: comments, string and character literals blanked, and every item under a
/// `cfg` attribute that cannot hold without `test` cut. A comment, a message, or a unit test that
/// names a module uses nothing in the shipped code. Line breaks are kept.
fn code_of(text: &str) -> String {
    without_test_items(&without_literals(text))
}

/// `text` with every comment and every string or character literal replaced by spaces.
fn without_literals(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let ident_char = |k: usize| chars[k].is_alphanumeric() || chars[k] == '_';
        let after_ident = i > 0 && ident_char(i - 1);
        // A byte or C string prefix, b or c, may stand before the r of a raw string: br"…", cr"…".
        let after_prefix =
            i > 0 && matches!(chars[i - 1], 'b' | 'c') && !(i > 1 && ident_char(i - 2));
        let end = if c == '/' && next == Some('/') {
            chars[i..]
                .iter()
                .position(|&x| x == '\n')
                .map_or(chars.len(), |at| i + at)
        } else if c == '/' && next == Some('*') {
            // Block comments nest in Rust.
            let (mut depth, mut j) = (0, i);
            while j < chars.len() {
                if chars[j] == '/' && chars.get(j + 1) == Some(&'*') {
                    depth += 1;
                    j += 2;
                } else if chars[j] == '*' && chars.get(j + 1) == Some(&'/') {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            j
        } else if c == 'r' && (!after_ident || after_prefix) && matches!(next, Some('"' | '#')) {
            // A raw string, r"…", r#"…"#, br"…" or cr"…", closed by a quote and as many hashes as
            // opened it.
            let hashes = chars[i + 1..].iter().take_while(|&&x| x == '#').count();
            let open = i + 1 + hashes;
            if chars.get(open) == Some(&'"') {
                let mut j = open + 1;
                while j < chars.len()
                    && !(chars[j] == '"' && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')))
                {
                    j += 1;
                }
                (j + 1 + hashes).min(chars.len())
            } else {
                i
            }
        } else if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            (j + 1).min(chars.len())
        } else if c == '\'' && next == Some('\\') {
            // An escaped character literal: '\n', '\'', '\u{…}'.
            chars[i + 2..]
                .iter()
                .skip(1)
                .position(|&x| x == '\'')
                .map_or(i, |at| i + 4 + at)
        } else if c == '\'' && chars.get(i + 2) == Some(&'\'') {
            // A plain character literal; a lifetime has no closing quote.
            i + 3
        } else {
            i
        };
        if end > i {
            out.extend(chars[i..end].iter().map(|&x| blank(x)));
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// `code` with every item under a `#[cfg(…)]` that cannot hold without `test` blanked, up to the
/// end of its braced body or its `;`: `cfg(test)` and `cfg(all(test, unix))` are test code,
/// `cfg(not(test))` and `cfg(any(test, unix))` are not.
fn without_test_items(code: &str) -> String {
    let mut out = code.to_string();
    let mut from = 0;
    while let Some(offset) = out[from..].find("#[cfg(") {
        let at = from + offset;
        let Some(close) = out[at..].find(")]").map(|c| at + c + 2) else {
            break;
        };
        let predicate = &out[at + "#[cfg(".len()..close - ")]".len()];
        if holds_without_test(predicate) != Some(false) {
            from = close;
            continue;
        }
        let mut depth = 0;
        let mut end = out.len();
        for (k, c) in out[close..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = close + k + 1;
                        break;
                    }
                }
                ';' if depth == 0 => {
                    end = close + k + 1;
                    break;
                }
                _ => {}
            }
        }
        let blanked: String = out[at..end]
            .chars()
            .map(|c| if c == '\n' { '\n' } else { ' ' })
            .collect();
        out.replace_range(at..end, &blanked);
        from = end;
    }
    out
}

/// Whether the `cfg` predicate `text` holds in a build without `test`: `Some(false)` where it
/// cannot, `None` where it depends on what else the build sets, such as `unix` or a feature.
fn holds_without_test(text: &str) -> Option<bool> {
    let (value, _) = predicate(text.trim());
    value
}

/// The value of the predicate `text` starts with, and the text after it.
fn predicate(text: &str) -> (Option<bool>, &str) {
    let text = text.trim_start();
    let name_len = text
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    let (name, rest) = text.split_at(name_len);
    let rest = rest.trim_start();
    if let Some(mut inner) = rest.strip_prefix('(') {
        let mut values = Vec::new();
        loop {
            inner = inner.trim_start();
            if let Some(after) = inner.strip_prefix(')') {
                inner = after;
                break;
            }
            if inner.is_empty() {
                break;
            }
            let (value, after) = predicate(inner);
            if after.len() == inner.len() {
                // A token no predicate starts with, such as a macro's `$m`: the rest is unknown.
                return (None, "");
            }
            values.push(value);
            inner = after.trim_start().strip_prefix(',').unwrap_or(after);
        }
        let value = match name {
            "not" => values.first().copied().flatten().map(|v| !v),
            "all" if values.contains(&Some(false)) => Some(false),
            "all" if values.iter().all(|v| *v == Some(true)) => Some(true),
            "any" if values.contains(&Some(true)) => Some(true),
            "any" if values.iter().all(|v| *v == Some(false)) => Some(false),
            _ => None,
        };
        return (value, inner);
    }
    if let Some(after) = rest.strip_prefix('=') {
        // `key = "value"`; the literal is blanked to spaces by now, up to the next , or ).
        let end = after.find([',', ')']).unwrap_or(after.len());
        return (None, &after[end..]);
    }
    let value = (name == "test").then_some(false);
    (value, rest)
}

/// What in `code` aliases the crate `fleet_util`, so that a path could name its items without
/// `fleet_util::`: `fleet_util as …`, and `self as …` in a `fleet_util::{…}` group.
fn aliases_of_the_crate(code: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in code.match_indices("fleet_util as ") {
        found.push(
            code[at..]
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        );
    }
    for (at, group) in code.match_indices("fleet_util::{") {
        let start = at + group.len();
        let (mut depth, mut top, mut end) = (1, String::new(), code.len());
        for (k, c) in code[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + k;
                        break;
                    }
                }
                _ if depth == 1 => top.push(c),
                _ => {}
            }
        }
        let aliased = top.split(',').any(|item| {
            let mut words = item.split_whitespace();
            words.next() == Some("self") && words.next() == Some("as")
        });
        if aliased {
            found.push(
                code[at..end]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    }
    found
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

/// The package names every dependency table of `crates/<krate>/Cargo.toml` names: normal, build
/// and development dependencies, for every target.
fn dependencies_of(krate: &str) -> BTreeSet<String> {
    let path = crates_dir().join(krate).join("Cargo.toml");
    let manifest: toml::Table = fs::read_to_string(&path)
        .expect("read a manifest")
        .parse()
        .expect("the manifest parses");
    let mut tables = vec![&manifest];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().filter_map(toml::Value::as_table));
    }
    let mut names = BTreeSet::new();
    for table in tables {
        for kind in ["dependencies", "build-dependencies", "dev-dependencies"] {
            let Some(deps) = table.get(kind).and_then(toml::Value::as_table) else {
                continue;
            };
            for (key, value) in deps {
                let package = value.get("package").and_then(toml::Value::as_str);
                names.insert(package.unwrap_or(key).to_string());
            }
        }
    }
    names
}

/// The package names of the workspace's members.
fn workspace_packages() -> BTreeSet<String> {
    let read = |path: PathBuf| -> toml::Table {
        fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
            .parse()
            .expect("the manifest parses")
    };
    let root = read(crates_dir().join("../Cargo.toml"));
    root["workspace"]["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|member| {
            let dir = crates_dir()
                .join("..")
                .join(member.as_str().expect("a path"));
            let manifest = read(dir.join("Cargo.toml"));
            manifest["package"]["name"]
                .as_str()
                .expect("a name")
                .to_string()
        })
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
            // An alias would let a `pub use` name the crate's items by another path.
            for alias in aliases_of_the_crate(&code) {
                wrong.push(format!("{}: aliases fleet_util: {alias}", file.display()));
            }
        }
    }

    for package in dependencies_of("fleet-server") {
        if package == "fleet-util" {
            wrong.push("crates/fleet-server/Cargo.toml: names fleet-util".to_string());
        }
    }
    let workspace = workspace_packages();
    for package in dependencies_of("fleet-util") {
        if workspace.contains(&package) && package != "fleet-core" {
            wrong.push(format!(
                "crates/fleet-util/Cargo.toml: names {package}, a workspace crate but fleet-core"
            ));
        }
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
    assert!(modules_named(&code_of("x // fleet_util::tree"), "fleet_util").is_empty());
}

/// Comments, literals and test code name nothing; a lifetime and the code beside them stay.
#[test]
fn comments_literals_and_test_code_are_not_read() {
    let names = |code: &str| {
        modules_named(&code_of(code), "fleet_util")
            .into_iter()
            .collect::<Vec<_>>()
    };
    assert!(names("/* fleet_util::tree /* nested */ fleet_util::archive */").is_empty());
    assert!(names(r#"let m = "see fleet_util::tree \" fleet_util::archive";"#).is_empty());
    assert!(names(r###"let r = r#"a "quoted" fleet_util::tree"#;"###).is_empty());
    assert!(names("#[cfg(test)]\nmod tests {\n    use fleet_util::tree;\n}").is_empty());
    assert!(names("#[cfg(all(test, unix))]\nfn t() { fleet_util::tree::x(); }").is_empty());
    assert_eq!(
        names("fn f<'a>(c: char) -> &'a str { let _ = '\"'; fleet_util::archive::x() }"),
        ["archive"]
    );
    assert_eq!(
        names("#[cfg(unix)]\nfn f() { fleet_util::nofollow::x(); }"),
        ["nofollow"]
    );
    assert_eq!(
        names(r###"let b = br#"\" fleet_util::tree"#; fleet_util::nofollow::x();"###),
        ["nofollow"]
    );
    assert_eq!(names("let c = cr\"\\\"; fleet_util::tree::x();"), ["tree"]);
    assert_eq!(
        names("#[cfg(not($m))]\nfn f() { fleet_util::tree::x(); }"),
        ["tree"]
    );
    assert_eq!(
        names("let b = br\"\\\"; fleet_util::archive::x();"),
        ["archive"]
    );
    assert_eq!(
        names("#[cfg(not(test))]\nfn f() { fleet_util::nofollow::x(); }"),
        ["nofollow"]
    );
    assert_eq!(
        names("#[cfg(any(test, unix))]\nfn f() { fleet_util::tree::x(); }"),
        ["tree"]
    );
    assert!(
        names("#[cfg(all(unix, not(not(test))))]\nfn t() { fleet_util::tree::x(); }").is_empty()
    );
    assert_eq!(
        names("#[cfg(all(unix, feature = \"x\"))]\nfn f() { fleet_util::archive::x(); }"),
        ["archive"]
    );
}

/// An alias of the crate is found whether it stands alone or as `self` in a group.
#[test]
fn an_alias_of_the_crate_is_found() {
    assert_eq!(aliases_of_the_crate("use fleet_util as u;").len(), 1);
    assert_eq!(
        aliases_of_the_crate("use fleet_util::{self as u};").len(),
        1
    );
    assert_eq!(
        aliases_of_the_crate("use fleet_util::{\n    tree,\n    self as u,\n};").len(),
        1
    );
    assert!(aliases_of_the_crate("use fleet_util::{self, tree};").is_empty());
    assert!(aliases_of_the_crate("use fleet_util::{nofollow::{self as nf}, tree};").is_empty());
}
