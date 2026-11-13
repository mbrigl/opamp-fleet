//! No privileged step goes through a link the account could have placed (ADR-0021 clause 11).
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
//! each path-based call that changes or reads a file, unless `ALLOWED` names it with its reason.
//! An entry that no longer matches fails too, so the list stays what the code does. Code under
//! `#[cfg(test)]` is not checked.
//!
//! The run-time Client, the second service and the Gateway are not here: each works only in
//! directories its own account owns, and clause 11 covers the privileged steps alone.

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
        "src/config_file.rs",
        "reads and replaces `supervisor.toml`, for the privileged steps and the run-time Client",
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

/// Calls that change or read a file by path. `fs::` stands for `std::fs::`, `tokio::fs::` and
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
    // A read by path follows a link at the name too, and a privileged step that reads what the
    // account put there acts on the account's word (ADR-0021 clause 11).
    "fs::read(",
    "fs::read_to_string(",
    "File::open(",
];

/// The path-based calls a privileged module may make: file, function, call, and why no link the
/// account placed can carry it.
const ALLOWED: &[(&str, &str, &str, &str)] = &[
    (
        "src/config_file.rs",
        "load",
        "fs::read_to_string(",
        "the run-time Client's read of its own file, in a directory its own account owns, which \
         ADR-0021 clause 11 does not cover; every privileged step reads through `load_held`",
    ),
    (
        "src/config_init.rs",
        "in_dir",
        "fs::read_to_string(",
        "the verification key the operator put beside the program or in `--fleet-files`: a \
         directory the operator fills when `service install` runs from a payload or a download, \
         not one of the account's",
    ),
    (
        "src/service/layout.rs",
        "stage_current_exe",
        "fs::read(",
        "the running executable itself, which the administrator started",
    ),
    (
        "src/service/manager.rs",
        "read_registration",
        "fs::read_to_string(",
        "the service's registration, which only root writes (`/etc/systemd/system`, \
         `/Library/LaunchDaemons`)",
    ),
    (
        "src/service/manager.rs",
        "read_state_dir",
        "fs::read_to_string(",
        "the service's registration, which only root writes",
    ),
    (
        "src/service/manager.rs",
        "load_removed_job",
        "fs::read_to_string(",
        "a launchd job in `/Library/LaunchDaemons`, which only root writes",
    ),
    (
        "src/service/runtime.rs",
        "recovery_state_dir",
        "fs::read_to_string(",
        "the run-time Client, in its own account's directories, which ADR-0021 clause 11 does \
         not cover",
    ),
    (
        "src/tls.rs",
        "read_bounded",
        "OpenOptions::new(",
        "opens a file to read it, never to change it, and without blocking, used only when it is \
         a regular file of at most 1 MiB: the run-time Client's reads of the files `[tls]` names, \
         in its own account's directories; a privileged step reads them, and an answered CA, \
         through `nofollow::read_privileged`",
    ),
    (
        "src/main.rs",
        "install",
        "fs::create_dir_all(",
        "the data root itself: its name is the administrator's, in a directory only root writes \
         on Unix, and on Windows the hold right after it refuses a reparse point (ADR-0021 \
         clause 13)",
    ),
    (
        "src/config_file.rs",
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

/// The calls of the functions `ALLOWED` names, in a privileged module: file, the function the call
/// is made in, the function called, and why the path it hands on is one no link the account placed
/// can carry.
const CALLED_FROM: &[(&str, &str, &str, &str)] = &[
    (
        "src/main.rs",
        "service_command",
        "install",
        "`service install` itself, with the administrator's flags",
    ),
    (
        "src/main.rs",
        "install",
        "install",
        "`manager::install`, another function of the name: the registration, where only root \
         writes",
    ),
    (
        "src/main.rs",
        "install",
        "create_state_dir",
        "the state directory the install decided, and the roots it is held beneath",
    ),
    (
        "src/main.rs",
        "install",
        "restrict",
        "the data root and the directories the administrator named, which the install planned",
    ),
    (
        "src/main.rs",
        "install",
        "stage_current_exe",
        "the layout root the administrator named or the default, which only root's directory holds",
    ),
    (
        "src/config_init.rs",
        "the_program",
        "in_dir",
        "the directory of the running program, which the administrator started",
    ),
    (
        "src/config_init.rs",
        "in_named_dir",
        "in_dir",
        "the directory `--fleet-files` names, the administrator's",
    ),
    (
        "src/config_init.rs",
        "copy_ca_beside",
        "held_dir",
        "the directory of the configuration this run writes, the data root or one `--config` named",
    ),
    (
        "src/config_file.rs",
        "held_file",
        "held_dir",
        "the directory of the configuration, the data root or one `--config` named",
    ),
    (
        "src/service/layout.rs",
        "heal_current",
        "set_current",
        "a version directory `current` is pointed at, in the layout root `set_current` holds",
    ),
    (
        "src/service/layout.rs",
        "stage_current_exe",
        "set_current",
        "the version directory just staged, in the layout root `set_current` holds",
    ),
    (
        "src/service/manager.rs",
        "register",
        "install",
        "the service manager's own `install`, another function of the name: the registration, \
         where only root writes",
    ),
    (
        "src/service/manager.rs",
        "registration",
        "read_registration",
        "the service's unit or plist, where only root writes",
    ),
    (
        "src/service/manager.rs",
        "agents_registration",
        "read_registration",
        "the second service's unit or plist, where only root writes",
    ),
    (
        "src/service/manager.rs",
        "registered_state_dir",
        "read_state_dir",
        "the service's unit or plist, where only root writes",
    ),
    (
        "src/service/manager.rs",
        "start_for_change",
        "load_removed_job",
        "the service's own label, its plist in `/Library/LaunchDaemons`",
    ),
    (
        "src/service/manager.rs",
        "start",
        "load_removed_job",
        "the service's own label, its plist in `/Library/LaunchDaemons`",
    ),
    (
        "src/service/runtime.rs",
        "unreadable_config",
        "recovery_state_dir",
        "the run-time Client's own start, which ADR-0021 clause 11 does not cover",
    ),
    (
        "src/service/runtime.rs",
        "load_effective_config",
        "load",
        "the run-time Client's own configuration, which ADR-0021 clause 11 does not cover",
    ),
    (
        "src/tls.rs",
        "read",
        "read_bounded",
        "the run-time Client's `[tls]` files; a privileged step reads them through \
         `nofollow::read_privileged`",
    ),
];

/// How a call of a function `ALLOWED` names is spelled, where its bare name is a common one.
const SPELLED: &[(&str, &str)] = &[("load", "ClientConfig::load(")];

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

/// Whether `code` makes the call `banned`: `FloorFile::open(` is not `File::open(`.
fn contains_call(code: &str, banned: &str) -> bool {
    code.match_indices(banned).any(|(at, _)| {
        !code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The lines of code in `text`, outside comments and the test module, each with the function it
/// lies in and its number.
fn code_lines(text: &str) -> Vec<(String, usize, &str)> {
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
        found.push((function.clone(), index + 1, code));
    }
    found
}

/// The banned calls in `text`, outside comments and the test module, by function and call.
fn calls(text: &str) -> Vec<(String, &'static str, usize)> {
    let mut found = Vec::new();
    for (function, line, code) in code_lines(text) {
        for banned in BANNED {
            if contains_call(code, banned) {
                found.push((function.clone(), *banned, line));
            }
            // `use std::fs::{remove_file, …}` would let the call go by its bare name.
            let bare = banned.trim_start_matches("fs::").trim_end_matches('(');
            if (code.starts_with("use std::fs") || code.starts_with("use tokio::fs"))
                && code.contains(bare)
            {
                found.push((function.clone(), *banned, line));
            }
        }
    }
    found
}

/// The calls in `text` of a function `callees` names, outside comments and the test module, by
/// the function they are made in, the function called, and the line; a declaration is no call.
fn calls_of<'a>(text: &str, callees: &[&'a str]) -> Vec<(String, &'a str, usize)> {
    let mut found = Vec::new();
    for (function, line, code) in code_lines(text) {
        for callee in callees {
            let spelled = SPELLED
                .iter()
                .find(|(function, _)| function == callee)
                .map_or_else(|| format!("{callee}("), |(_, call)| (*call).to_string());
            if declared_fn(code) != Some(callee) && contains_call(code, &spelled) {
                found.push((function.clone(), *callee, line));
            }
        }
    }
    found
}

/// Every source file of the privileged modules, by its path relative to the crate, and its text.
fn privileged_sources() -> Vec<(String, String)> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
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
            found.push((
                relative,
                std::fs::read_to_string(&file).expect("read a source file"),
            ));
        }
    }
    found
}

/// Verifies: ADR-0021
#[test]
fn privileged_modules_change_the_account_s_directories_through_held_directories_only() {
    let mut violations = Vec::new();
    let mut used = BTreeSet::new();
    for (relative, text) in privileged_sources() {
        {
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
        pub(crate) fn a() { std::fs::write(p, b); let _ = std::fs::read(p); FloorFile::open(p); }
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
            ("a".to_string(), "fs::read("),
            ("b".to_string(), "fs::remove_dir_all("),
        ]
    );
}

/// A function `ALLOWED` names is allowed its path-based call for the paths it is handed, so each
/// call of it in a privileged module is named here too, by the file and the function it is made
/// in, with why the path it hands on is one no link the account placed can carry. A new caller
/// is a finding until it is listed, and an entry that no longer matches fails, as in `ALLOWED`.
/// Verifies: ADR-0021
#[test]
fn each_call_of_an_allowed_function_is_named_with_its_reason() {
    let allowed: BTreeSet<&str> = ALLOWED
        .iter()
        .map(|(_, function, _, _)| *function)
        .collect();
    let allowed: Vec<&str> = allowed.into_iter().collect();
    let mut unnamed = Vec::new();
    let mut used = BTreeSet::new();
    for (relative, text) in privileged_sources() {
        for (caller, callee, line) in calls_of(&text, &allowed) {
            match CALLED_FROM
                .iter()
                .position(|(f, c, d, _)| *f == relative && *c == caller && *d == callee)
            {
                Some(entry) => {
                    used.insert(entry);
                }
                None => unnamed.push(format!(
                    "{relative}:{line}: `{caller}` calls `{callee}`, which ALLOWED lets name a \
                     path; list the call in CALLED_FROM with why no link the account placed can \
                     carry the path it hands on, or read through `nofollow`"
                )),
            }
        }
    }
    let stale: Vec<String> = CALLED_FROM
        .iter()
        .enumerate()
        .filter(|(index, _)| !used.contains(index))
        .map(|(_, (file, caller, callee, _))| format!("{file}: `{caller}` calls `{callee}`"))
        .collect();
    assert!(
        unnamed.is_empty(),
        "calls of allowed functions without a reason:\n{}",
        unnamed.join("\n")
    );
    assert!(
        stale.is_empty(),
        "CALLED_FROM names calls the code no longer makes:\n{}",
        stale.join("\n")
    );
}

/// The call scanner: a call by any path is found, a declaration and a longer name are not.
#[test]
fn the_call_scanner_finds_calls_and_passes_over_declarations() {
    let src = r#"
        fn in_dir(p: &Path) {}
        fn a() { let _ = Self::in_dir(p); begin_dir(p); }
        fn b() { crate::config_init::in_dir(p); }
        #[cfg(test)]
        mod tests { fn c() { in_dir(p); } }
    "#;
    assert_eq!(
        calls_of(src, &["in_dir"]),
        [
            ("a".to_string(), "in_dir", 3),
            ("b".to_string(), "in_dir", 4)
        ]
    );
}
