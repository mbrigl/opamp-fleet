//! The Client's `*at` system calls have two homes (the `fleet-util` bullet of
//! `docs/ARCHITECTURE.md`).
//!
//! What it decides: the Unix `*at` system calls of `nofollow` are wrapped once in
//! `fleet_util::nofollow::at`, and the calls only the Client makes stay in its `storage::handle`.
//! Every walk by directory handle goes through one of the two, so a call that follows no link is
//! written, and reviewed, in one place. This test reads the sources of `fleet-agent` and
//! `fleet-util`, test code and every `cfg` branch included, as a syntax tree, and fails on each
//! `libc::` `*at` call outside those two modules, and on each `*at` name a `use` imports from
//! `libc` there. Tokens inside a macro's arguments are read too; comments and literals are no
//! tokens.
//!
//! Why it lives here: the rule binds the Client's walks, and `fleet-util`'s `nofollow::at` only as
//! the Client's other home, so `cargo test -p fleet-agent` has to decide it. It reads
//! `fleet-util`'s sources by path, as the Client depends on that crate.

use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use syn::Item;

/// Each crate read, and the module of it that holds its `*at` calls.
const HOMES: [(&str, &[&str]); 2] = [
    ("fleet-agent", &["storage", "handle"]),
    ("fleet-util", &["nofollow", "at"]),
];

/// The `*at` system calls of `libc`: each takes a directory handle with a name. A list rather
/// than a rule on the name, which `creat` and `stat` would pass or fail by accident.
const AT_CALLS: &[&str] = &[
    "execveat",
    "faccessat",
    "faccessat2",
    "fchmodat",
    "fchmodat2",
    "fchownat",
    "fstatat",
    "fstatat64",
    "futimesat",
    "linkat",
    "mkdirat",
    "mkfifoat",
    "mknodat",
    "name_to_handle_at",
    "open_by_handle_at",
    "openat",
    "openat2",
    "openat64",
    "readlinkat",
    "renameat",
    "renameat2",
    "renameatx_np",
    "statx",
    "symlinkat",
    "unlinkat",
    "utimensat",
];

fn at_call(name: &str) -> bool {
    AT_CALLS.contains(&name)
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

/// The module a file under `src/` holds, by its path: `service/run_as.rs` and
/// `service/run_as/mod.rs` hold `service::run_as`; a crate root holds the crate's top.
fn module_of(relative: &Path) -> Vec<String> {
    let mut path: Vec<String> = relative
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if path.first().is_some_and(|first| first == "bin") {
        path.clear();
    }
    if path.last().is_some_and(|last| last == "mod") {
        path.pop();
    }
    if path.len() == 1 && matches!(path[0].as_str(), "lib" | "main") {
        path.clear();
    }
    path
}

/// The `*at` calls of `libc` in `tokens`, by name, groups and macro arguments included: each
/// `libc::name`, each name of a `libc::{…}` group a `use` imports, and a glob `libc::*` as `*`.
fn at_calls_in(tokens: TokenStream, out: &mut Vec<String>) {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    for (at, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Group(group) => at_calls_in(group.stream(), out),
            TokenTree::Ident(ident) if ident == "libc" => {
                let sep = |i: usize, spacing: Spacing| {
                    matches!(trees.get(i), Some(TokenTree::Punct(p))
                        if p.as_char() == ':' && p.spacing() == spacing)
                };
                if !(sep(at + 1, Spacing::Joint) && sep(at + 2, Spacing::Alone)) {
                    continue;
                }
                match trees.get(at + 3) {
                    Some(TokenTree::Ident(name)) if at_call(&name.to_string()) => {
                        out.push(name.to_string());
                    }
                    Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Brace => {
                        names_in(group.stream(), out);
                    }
                    Some(TokenTree::Punct(p)) if p.as_char() == '*' => out.push("*".to_string()),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The `*at` names in a `use libc::{…}` group, nested groups and renamed names included.
fn names_in(tokens: TokenStream, out: &mut Vec<String>) {
    for tree in tokens {
        match tree {
            TokenTree::Group(group) => names_in(group.stream(), out),
            TokenTree::Ident(name) if at_call(&name.to_string()) => out.push(name.to_string()),
            TokenTree::Punct(p) if p.as_char() == '*' => out.push("*".to_string()),
            _ => {}
        }
    }
}

/// The `*at` calls in `items`, each with the module it is made in, inline modules entered.
fn at_calls(items: &[Item], module: &[String], out: &mut Vec<(Vec<String>, String)>) {
    for item in items {
        if let Item::Mod(inline) = item {
            if let Some((_, content)) = &inline.content {
                let mut inner = module.to_vec();
                inner.push(inline.ident.to_string());
                at_calls(content, &inner, out);
                continue;
            }
        }
        let mut found = Vec::new();
        at_calls_in(item.to_token_stream(), &mut found);
        out.extend(found.into_iter().map(|name| (module.to_vec(), name)));
    }
}

/// The `*at` calls in a source text, each with its module, the file's own module `module`.
fn at_calls_of(text: &str, module: &[String]) -> Vec<(Vec<String>, String)> {
    let file = syn::parse_file(text).expect("parse a source file");
    let mut out = Vec::new();
    at_calls(&file.items, module, &mut out);
    out
}

#[test]
fn the_clients_at_calls_are_in_its_handle_layer_or_in_nofollow_at() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut misplaced = Vec::new();
    let mut seen = 0;
    for (krate, home) in HOMES {
        let src = workspace.join("crates").join(krate).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        files.sort();
        for file in files {
            let text = fs::read_to_string(&file).expect("read a source file");
            let relative = file.strip_prefix(&src).expect("beneath src");
            for (module, call) in at_calls_of(&text, &module_of(relative)) {
                seen += 1;
                if !(module.len() >= home.len() && module.iter().zip(home).all(|(m, h)| m == h)) {
                    misplaced.push(format!(
                        "{krate}/src/{}: `libc::{call}` in `{}`",
                        relative.display(),
                        module.join("::")
                    ));
                }
            }
        }
    }
    assert!(seen > 0, "no `*at` call found: the reader reads nothing");
    assert!(
        misplaced.is_empty(),
        "`*at` system calls outside `storage::handle` and `fleet_util::nofollow::at`; move each \
         into one of them:\n{}",
        misplaced.join("\n")
    );
}

/// The reader itself: what it finds, where, and what it passes over.
#[test]
fn the_reader_finds_at_calls_by_module() {
    let module = vec!["service".to_string()];
    let found = at_calls_of(
        r#"
        fn a() { unsafe { libc::fchownat(fd, c"".as_ptr(), 0, 0, 0) }; }
        mod walk {
            fn b() { assert!(unsafe { libc::renameat2(a, b, c, d, 0) } == 0); }
            fn c() { let _ = unsafe { libc::fstat(fd, p) }; let _: libc::stat; }
        }
        // libc::unlinkat(dir, name, 0) in a comment
        fn d() { let _ = "libc::openat"; let _ = libc::AT_FDCWD; }
        fn e() { let _ = unsafe { libc::creat(p, 0) } + unsafe { libc::creat64(p, 0) }; }
        use libc::{c_int, fchmodat, nested::{mkdirat as make}};
        "#,
        &module,
    );
    assert_eq!(
        found,
        vec![
            (vec!["service".to_string()], "fchownat".to_string()),
            (
                vec!["service".to_string(), "walk".to_string()],
                "renameat2".to_string()
            ),
            (vec!["service".to_string()], "fchmodat".to_string()),
            (vec!["service".to_string()], "mkdirat".to_string()),
        ]
    );
    assert_eq!(
        at_calls_of("use libc::*;", &module),
        [(vec!["service".to_string()], "*".to_string())]
    );
    assert!(at_call("renameatx_np") && at_call("fstatat64") && at_call("statx"));
    for not_at in [
        "lstat",
        "fstat64",
        "fdopendir",
        "creat",
        "creat64",
        "tcgetattr",
    ] {
        assert!(!at_call(not_at), "{not_at}");
    }
    assert_eq!(
        module_of(Path::new("service/run_as.rs")),
        ["service", "run_as"]
    );
    assert_eq!(module_of(Path::new("gateway/mod.rs")), ["gateway"]);
    assert!(module_of(Path::new("lib.rs")).is_empty());
}
