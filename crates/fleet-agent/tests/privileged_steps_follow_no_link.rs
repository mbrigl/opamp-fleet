//! No privileged step goes through a link the account could have placed (ADR-0037 clause 11).
//!
//! `service install`, `service config` and `service uninstall` run as root or as an
//! Administrator, and they write into the data root, the state directory and the layout root,
//! which belong to the service's account. A call that names a path there — `std::fs::write`,
//! `rename`, `remove_file`, `set_permissions`, `File::create` and their kind — resolves every
//! component of it again, and a link the account put on the way carries the privileged step
//! wherever it points. So the modules those steps run in reach the account's directories only
//! through the adapters that hold a directory and never follow a link: `nofollow::HeldDir`,
//! `storage::handle` and `win32::files`.
//!
//! This test reads the source text of those modules, every `cfg` branch included, and fails on
//! each path-based call that changes the file system, unless `ALLOWED` names it with its reason.
//! An entry that no longer matches fails too, so the list stays what the code does. Code under
//! `#[cfg(test)]` is not checked.
//!
//! The run-time Client, the second service and the Gateway are not here: each writes only in
//! directories its own account owns and no other account may write to (ADR-0037 clause 21).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The modules a privileged step runs in, relative to the crate, and why. A directory covers every
/// file in it.
const PRIVILEGED: &[(&str, &str)] = &[
    (
        "src/main.rs",
        "`service install`, `service config` and `service uninstall` themselves",
    ),
    (
        "src/config_init.rs",
        "writes `supervisor.toml` and the CA copy into the data root",
    ),
    (
        "src/service",
        "the layout, the hand-over, the registration, the rights and the enrolment",
    ),
    (
        "src/csr.rs",
        "the enrolment's key, request and certificates in the state directory",
    ),
    (
        "src/connection.rs",
        "the connection settings an enrolment stores in the state directory",
    ),
    (
        "src/tls.rs",
        "reads the state directory's key and certificates",
    ),
];

/// Calls that change the file system by path. `fs::` stands for `std::fs::`, `tokio::fs::` and
/// `std::os::unix::fs::` alike.
const BANNED: &[&str] = &[
    "fs::write(",
    "fs::rename(",
    "fs::remove_file(",
    "fs::remove_dir(",
    "fs::remove_dir_all(",
    "fs::set_permissions(",
    "fs::copy(",
    "fs::create_dir(",
    "fs::create_dir_all(",
    "fs::hard_link(",
    "fs::symlink(",
    "fs::chown(",
    "fs::lchown(",
    "symlink_file(",
    "symlink_dir(",
    "File::create(",
    "OpenOptions::new(",
    "File::options(",
    "DirBuilder",
    // A link or junction a subprocess makes is a path-based change all the same.
    "\"mklink\"",
    "set_readonly(",
];

/// The path-based calls a privileged module may make: file, function, call, and why no link the
/// account placed can carry it.
const ALLOWED: &[(&str, &str, &str, &str)] = &[
    (
        "src/config_init.rs",
        "read_certificates",
        "OpenOptions::new(",
        "opens the CA file the answer names to read it, never to change it, and without blocking: \
         what it opened is used only when it is a regular file, and only its certificates are \
         copied",
    ),
    (
        "src/main.rs",
        "install",
        "fs::create_dir_all(",
        "the data root itself: its name is the administrator's, in a directory only root writes \
         on Unix, and on Windows the hold right after it refuses a reparse point (ADR-0037 \
         clause 13)",
    ),
    (
        "src/config_init.rs",
        "held_dir",
        "fs::create_dir_all(",
        "the configuration's directory itself, the data root or one `--config` named, held right \
         after it as the data root is held",
    ),
    (
        "src/service/layout.rs",
        "create_state_dir",
        "fs::create_dir_all(",
        "a root itself, held right after it, and a state directory outside every root, which the \
         administrator named",
    ),
    (
        "src/service/layout.rs",
        "stage_current_exe",
        "fs::create_dir_all(",
        "the layout root itself, held right after it",
    ),
    (
        "src/service/layout.rs",
        "set_current",
        "fs::remove_file(",
        "the temporary name in the layout root, whose own name lies in a directory only root writes \
         on Unix and is held on Windows: an unlink removes a link as itself",
    ),
    (
        "src/service/layout.rs",
        "set_current",
        "fs::symlink(",
        "creates the link at a name in the layout root, failing where anything stands",
    ),
    (
        "src/service/layout.rs",
        "set_current",
        "fs::rename(",
        "renames within the layout root: a link at `current` is replaced, never followed",
    ),
    (
        "src/service/layout.rs",
        "set_current",
        "fs::remove_dir(",
        "`current` in the held layout root on Windows: a junction is removed as itself",
    ),
    (
        "src/service/layout.rs",
        "set_current",
        "\"mklink\"",
        "creates the junction `current` in the held layout root on Windows, where a junction \
         that stood there was removed as itself; `mklink` fails where anything stands",
    ),
    (
        "src/service/windows_rights.rs",
        "restrict",
        "fs::create_dir_all(",
        "the data root and the directories outside it the administrator named, each opened as \
         itself right after it, a reparse point refused",
    ),
];

/// Every `.rs` file of `path`, a file or a directory.
fn sources(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(path).expect("list a privileged directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// The name of the function a line declares, if it declares one.
fn declared_fn(line: &str) -> Option<&str> {
    let mut words = line.split_whitespace().peekable();
    while let Some(word) = words.next() {
        if word == "fn" {
            let name = words.next()?;
            return Some(name.split(['(', '<']).next().unwrap_or(name));
        }
        let modifier = word.starts_with("pub")
            || matches!(word, "const" | "async" | "unsafe" | "extern" | "\"C\"");
        if !modifier {
            return None;
        }
    }
    None
}

/// The banned calls in `text`, outside comments and the test module, by function and call.
fn calls(text: &str) -> Vec<(String, &'static str, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    let mut function = String::from("(module)");
    for (index, line) in lines.iter().enumerate() {
        let code = line.trim();
        if code == "#[cfg(test)]"
            && lines[index + 1..]
                .iter()
                .find(|next| !next.trim().is_empty())
                .is_some_and(|next| next.trim_start().starts_with("mod "))
        {
            break;
        }
        if code.starts_with("//") {
            continue;
        }
        if let Some(name) = declared_fn(code) {
            function = name.to_string();
        }
        for banned in BANNED {
            if code.contains(banned) {
                found.push((function.clone(), *banned, index + 1));
            }
            // `use std::fs::{remove_file, …}` would let the call go by its bare name.
            let bare = banned.trim_start_matches("fs::").trim_end_matches('(');
            if (code.starts_with("use std::fs") || code.starts_with("use tokio::fs"))
                && code.contains(bare)
            {
                found.push((function.clone(), *banned, index + 1));
            }
        }
    }
    found
}

/// Verifies: ADR-0037
#[test]
fn privileged_modules_change_the_account_s_directories_through_held_directories_only() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    let mut used = BTreeSet::new();
    for (module, _why) in PRIVILEGED {
        let root = crate_dir.join(module);
        assert!(
            root.exists(),
            "{module} is listed as privileged and does not exist"
        );
        for file in sources(&root) {
            let relative = file
                .strip_prefix(crate_dir)
                .expect("beneath the crate")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&file).expect("read a source file");
            for (function, call, line) in calls(&text) {
                let allowed = ALLOWED
                    .iter()
                    .position(|(f, func, c, _)| *f == relative && *func == function && *c == call);
                match allowed {
                    Some(entry) => {
                        used.insert(entry);
                    }
                    None => violations.push(format!(
                        "{relative}:{line}: `{call}` in `{function}` names a path in a privileged \
                         module; reach the directory through `nofollow::HeldDir`, \
                         `storage::handle` or `win32::files`, or list it in ALLOWED with why no \
                         link can carry it"
                    )),
                }
            }
        }
    }
    let stale: Vec<String> = ALLOWED
        .iter()
        .enumerate()
        .filter(|(index, _)| !used.contains(index))
        .map(|(_, (file, function, call, _))| format!("{file}: `{call}` in `{function}`"))
        .collect();
    assert!(
        violations.is_empty(),
        "path-based changes in privileged modules:\n{}",
        violations.join("\n")
    );
    assert!(
        stale.is_empty(),
        "ALLOWED names calls the code no longer makes:\n{}",
        stale.join("\n")
    );
}

/// The scanner itself: what it finds, and what it passes over.
#[test]
fn the_scanner_finds_path_based_calls_by_function() {
    let src = r#"
        use std::fs::remove_file;
        pub(crate) fn a() { std::fs::write(p, b); }
        // std::fs::rename(a, b) in a comment
        async fn b() {
            tokio::fs::remove_dir_all(p).await;
        }
        #[cfg(test)]
        mod tests { fn c() { std::fs::write(p, b); } }
    "#;
    let found: Vec<(String, &str)> = calls(src)
        .into_iter()
        .map(|(function, call, _)| (function, call))
        .collect();
    assert_eq!(
        found,
        [
            ("(module)".to_string(), "fs::remove_file("),
            ("a".to_string(), "fs::write("),
            ("b".to_string(), "fs::remove_dir_all("),
        ]
    );
}
