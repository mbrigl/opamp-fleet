//! The capability matrix of `docs/CONFORMANCE.md` against the code (goal G-12): every capability
//! of the Baseline has a row, each row's bit is the Baseline's, and a capability is listed as
//! implemented or partial exactly when the side it belongs to declares it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opamp::proto::{AgentCapabilities, ServerCapabilities};

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

/// The production code of the crate whose sources are under `src`: the modules its `lib.rs` and
/// `main.rs` reach, each without its comments and its items under `#[cfg(test)]`. A module
/// declared under `#[cfg(test)]` is never reached, so its file is not read.
fn production(src: &Path) -> String {
    let mut out = String::new();
    for root in ["lib.rs", "main.rs"] {
        let root = src.join(root);
        if root.exists() {
            read_module(&root, src, &mut out);
        }
    }
    out
}

/// Appends the production code of the module in `file` and of the modules it declares, whose
/// files are under `dir`.
fn read_module(file: &Path, dir: &Path, out: &mut String) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
    let code = without_test_items(&text);
    for name in declared_modules(&code) {
        let child = dir.join(&name);
        let path = [dir.join(format!("{name}.rs")), child.join("mod.rs")]
            .into_iter()
            .find(|path| path.exists())
            .unwrap_or_else(|| panic!("no file for `mod {name};` of {file:?}"));
        read_module(&path, &child, out);
    }
    out.push_str(&code);
}

/// The names of the out-of-line modules `code` declares: each `mod name;`.
fn declared_modules(code: &str) -> Vec<String> {
    let tokens = tokens(code);
    tokens
        .windows(3)
        .filter(|window| window[0] == "mod" && window[2] == ";")
        .map(|window| window[1].trim_start_matches("r#").to_string())
        .collect()
}

/// The tokens of `code` that are not whitespace or comments.
fn tokens(code: &str) -> Vec<String> {
    let chars: Vec<char> = code.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let end = token_end(&chars, at);
        if !is_trivia(&chars, at) {
            tokens.push(chars[at..end].iter().collect());
        }
        at = end;
    }
    tokens
}

const TEST_ONLY: &str = "#[cfg(test)]";

/// `code` without its comments and without each item under `#[cfg(test)]`, wherever it stands:
/// a module, a function, a field, a statement, a match arm.
fn without_test_items(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut at = 0;
    while at < chars.len() {
        if chars[at..].starts_with(&TEST_ONLY.chars().collect::<Vec<_>>()) {
            at = item_end(&chars, at + TEST_ONLY.len());
            continue;
        }
        let end = token_end(&chars, at);
        if !is_comment(&chars, at) {
            out.extend(&chars[at..end]);
        }
        at = end;
    }
    out
}

/// Where the item that starts at `at` ends: after a `;`, after a `,` outside generics and a
/// `where` clause, or after its closing brace unless an `else`, a `.` or a `?` goes on with it,
/// and before a closing delimiter it did not open. One that runs to the end of the file is
/// misread, and fails the matrix rather than hide the production code after it.
fn item_end(chars: &[char], mut at: usize) -> usize {
    let (mut depth, mut angle, mut in_where) = (0usize, 0usize, false);
    while at < chars.len() {
        let end = token_end(chars, at);
        let token: String = chars[at..end].iter().collect();
        let before = at.checked_sub(1).map(|i| chars[i]);
        match token.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" if depth == 0 => return at,
            ")" | "]" => depth -= 1,
            "}" => {
                depth -= 1;
                if depth == 0 {
                    let next = skip_trivia(chars, end);
                    let follow: String = chars[next..token_end(chars, next)].iter().collect();
                    match follow.as_str() {
                        ";" | "," => return token_end(chars, next),
                        "else" | "." | "?" => {}
                        _ => return end,
                    }
                }
            }
            ";" if depth == 0 => return end,
            "," if depth == 0 && angle == 0 && !in_where => return end,
            "where" if depth == 0 => in_where = true,
            "<" if depth == 0
                && before.is_some_and(|c| c.is_alphanumeric() || "_:".contains(c)) =>
            {
                angle += 1;
            }
            ">" if depth == 0 && angle > 0 && before.is_some_and(|c| !"-=".contains(c)) => {
                angle -= 1;
            }
            _ => {}
        }
        at = end;
    }
    panic!("an item under #[cfg(test)] runs to the end of its file: the reader misread it");
}

/// The first position from `at` that is not whitespace or a comment.
fn skip_trivia(chars: &[char], mut at: usize) -> usize {
    while at < chars.len() && is_trivia(chars, at) {
        at = token_end(chars, at);
    }
    at
}

fn is_trivia(chars: &[char], at: usize) -> bool {
    chars[at].is_whitespace() || is_comment(chars, at)
}

fn is_comment(chars: &[char], at: usize) -> bool {
    chars[at] == '/' && matches!(chars.get(at + 1), Some('/' | '*'))
}

/// Where the token that starts at `at` ends: a comment, a string or character literal, an
/// identifier, a lifetime, or else a single character.
fn token_end(chars: &[char], at: usize) -> usize {
    let len = chars.len();
    if at >= len {
        return len;
    }
    let quote_end = |mut i: usize| {
        while i < len && chars[i] != '"' {
            i += if chars[i] == '\\' { 2 } else { 1 };
        }
        (i + 1).min(len)
    };
    match chars[at] {
        '/' if chars.get(at + 1) == Some(&'/') => {
            (at..len).find(|&i| chars[i] == '\n').unwrap_or(len)
        }
        '/' if chars.get(at + 1) == Some(&'*') => {
            let (mut i, mut open) = (at + 2, 1);
            while i < len && open > 0 {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    open += 1;
                    i += 2;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    open -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            i
        }
        '"' => quote_end(at + 1),
        '\'' if chars.get(at + 1) == Some(&'\\') => ((at + 3)..len)
            .find(|&i| chars[i] == '\'')
            .map_or(len, |i| i + 1),
        '\'' if chars.get(at + 2) == Some(&'\'') => at + 3,
        c if c.is_alphanumeric() || c == '_' => {
            let end = (at..len)
                .find(|&i| !(chars[i].is_alphanumeric() || chars[i] == '_'))
                .unwrap_or(len);
            let word: String = chars[at..end].iter().collect();
            match (word.as_str(), chars.get(end)) {
                ("b" | "c", Some('"')) => quote_end(end + 1),
                ("b", Some('\'')) => token_end(chars, end),
                ("r" | "br" | "cr", Some('"' | '#')) => {
                    let hashes = (end..len).take_while(|&i| chars[i] == '#').count();
                    let open = end + hashes;
                    if chars.get(open) != Some(&'"') {
                        // A raw identifier, `r#name`.
                        return token_end(chars, open);
                    }
                    let close: Vec<char> = std::iter::once('"')
                        .chain(std::iter::repeat_n('#', hashes))
                        .collect();
                    ((open + 1)..len)
                        .find(|&i| chars[i..].starts_with(&close))
                        .map_or(len, |i| i + close.len())
                }
                _ => end,
            }
        }
        _ => at + 1,
    }
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
        let declared = code.contains(&format!("{enum_name}::{rust}"));
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

/// Verifies: G-12
#[test]
fn the_reader_finds_a_declaration_after_a_test_only_item() {
    let code = without_test_items(
        "struct Held {\n    #[cfg(test)]\n    dropped: HashMap<Uid, Vec<u8>>,\n    kept: u8,\n}\n\
         impl Held {\n    #[cfg(test)]\n    #[must_use]\n    fn keeps<T>(&self, uid: T) -> bool {\n        \
         if self.kept > 0 { true } else { '}' == '{' }\n    }\n\n    \
         fn capabilities() -> u64 {\n        #[cfg(test)]\n        let unused = \"}\";\n        \
         ServerCapabilities::AcceptsStatus as u64\n    }\n}\n",
    );
    assert!(code.contains("ServerCapabilities::AcceptsStatus"), "{code}");
    assert!(code.contains("kept: u8"), "{code}");
    for test_only in ["dropped", "keeps", "unused"] {
        assert!(!code.contains(test_only), "{test_only} in {code}");
    }
}

/// Verifies: G-12
#[test]
fn the_reader_skips_everything_inside_a_test_module() {
    let code = without_test_items(
        "pub fn serve() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    \
         #[test]\n    fn declares() {\n        let _ = ServerCapabilities::OffersRemoteConfig;\n    \
         }\n}\n\npub fn after() {}\n",
    );
    assert!(!code.contains("OffersRemoteConfig"), "{code}");
    assert!(code.contains("pub fn after"), "{code}");
}

/// Verifies: G-12
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
    assert!(!code.contains("OffersRemoteConfig"), "{code}");
    assert!(code.contains("ServerCapabilities::AcceptsStatus"), "{code}");
}
