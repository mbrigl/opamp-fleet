//! The dependency direction of DEV-ports-and-adapters: a module of the core depends on no adapter and on no
//! technology. Every source file of the workspace's library crates has a role in `ROLES`; the core
//! files are read and every path they name is held to the rule. A new module fails until it is
//! given a role, so where it belongs is decided when it is written, and a role whose module is
//! gone fails until it is removed, so the table never names a module the tree lacks.
//!
//! The test reads source text, not the compiled crate, so every `cfg` branch is checked whatever
//! the host — the Windows and macOS paths included. Code under `#[cfg(test)]` is not checked.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;

// The Server's reader of shipped code, which cuts what only the tests build.
#[path = "../../fleet-server/tests/source/mod.rs"]
mod source;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    /// Domain or port: holds the rule.
    Core,
    /// Binds a port to a technology.
    Adapter,
    /// Composition root: wires adapters to the core and may depend on everything.
    Root,
}
use Role::*;

/// Every module of the checked crates, by crate directory and module path. A module path is the
/// file's path under `src` without `.rs`, `mod.rs`, or `lib.rs`; `""` is the crate root.
const ROLES: &[(&str, &[(&str, Role)])] = &[
    (
        "opamp",
        &[
            ("", Root),
            // Inline in lib.rs: the generated protobuf types.
            ("proto", Core),
            ("attributes", Core),
            ("endpoint", Core),
            ("frame", Core),
            ("uid", Core),
            ("client", Core),
            ("client::protocol", Core),
            ("client::backoff", Adapter),
            ("client::connection", Adapter),
            ("client::http", Adapter),
            ("client::ws", Adapter),
            ("server", Adapter),
            ("server::listen", Adapter),
            ("server::pace", Adapter),
            // The system's randomness, with `ring`.
            ("random", Adapter),
            ("tls", Adapter),
            // The WebSocket configuration both ends share, with `tungstenite`.
            ("websocket", Adapter),
        ],
    ),
    (
        "fleet-core",
        &[
            ("", Root),
            ("bootstrap_proof", Core),
            // Reading what a CSR asks for, with `x509-parser`.
            ("csr", Adapter),
            ("ending", Core),
            // A certificate's end read with `x509-parser` and `time`.
            ("ending::read", Adapter),
            ("host", Core),
            ("naming", Core),
            ("package", Core),
            // The bootstrap PKCS#12 file (SEC-bootstrap-certificates-and-enrolment): its rule, and its binding to `p12-keystore`.
            ("pkcs12", Core),
            ("pkcs12::file", Adapter),
            // What the Server and the operator program do alike on a PKCS#11 token, with
            // `cryptoki` (SEC-admission-by-client-certificate).
            ("pkcs11", Adapter),
            ("platform", Core),
            ("rate", Core),
            ("renewal", Core),
            // The shape of the Gateways' revocation list, which the Server writes and a Gateway reads.
            ("revocation_list", Core),
            ("version", Core),
            // What a client certificate is known by; read with `x509-parser`, and issued with
            // `ring` and `rcgen`.
            ("x509", Core),
            ("x509::issue", Adapter),
            ("x509::read", Adapter),
        ],
    ),
    (
        "fleet-util",
        &[
            ("", Root),
            // The unpackers, with `tar`, `flate2`, `zip` and `sevenz-rust2`, writing to disk.
            ("archive", Adapter),
            // A bootstrap identity as PEM: a value a core module of the Client names.
            ("identity", Core),
            // A bootstrap identity opened through `fleet_core::pkcs12::file`.
            ("identity::pkcs12", Adapter),
            // The file operations that follow no link, with `libc` on Unix.
            ("nofollow", Adapter),
            // A password file read through `nofollow`.
            ("password", Adapter),
            ("tree", Core),
        ],
    ),
    (
        "fleet-server",
        &[
            ("", Root),
            ("main", Root),
            // What the binary runs as an instance: its lease, its planes and the revocations of
            // the other instances, wired together.
            ("instance", Root),
            // The Agent plane's use cases (SRV-two-use-case-layers §1). It reads certificates and
            // CSRs through `ca` and `tls` and waits on timers, as the code it took from `transport`
            // did, so it is no core module; what it may not name is `PLANE_REACH`'s.
            ("agent_cases", Adapter),
            // The Operator plane's use cases (SRV-two-use-case-layers §1). It checks the plane's
            // credential through `credentials` and reads the request's headers, as the guard it took
            // from `api` did, so it is no core module either.
            ("operator_cases", Adapter),
            ("agent_rate", Core),
            ("agent_store", Core),
            // The HTTP answers and the peer's address both planes' endpoints share, with `axum`.
            ("answers", Adapter),
            ("audit", Core),
            ("blobs", Core),
            ("bootstrap", Core),
            ("changed", Core),
            ("configs", Core),
            ("deployments", Core),
            ("enrolment", Core),
            ("fleet", Core),
            ("fleet::approval", Core),
            ("fleet::delivery", Core),
            // Test-only: it wires the filesystem adapters the tests of the core run on, as
            // `test_support` does.
            ("fleet::fixtures", Adapter),
            ("fleet::held", Core),
            ("fleet::placement", Core),
            ("fleet::rollout", Core),
            ("fleet::session", Core),
            ("fleet::views", Core),
            ("floors", Core),
            ("labels", Core),
            ("listing", Core),
            ("packages", Core),
            ("revocation", Core),
            ("throttle", Core),
            ("api", Adapter),
            ("api::paging", Adapter),
            ("audit_export", Adapter),
            ("audit_log", Adapter),
            ("bridge", Adapter),
            ("ca", Adapter),
            ("checkpoint", Adapter),
            ("clock", Adapter),
            ("config", Adapter),
            ("credentials", Adapter),
            ("fs", Adapter),
            ("fs::agents", Adapter),
            ("fs::audit", Adapter),
            ("fs::blobs", Adapter),
            ("fs::bootstrap", Adapter),
            ("fs::configs", Adapter),
            ("fs::deployments", Adapter),
            ("fs::enrolment", Adapter),
            ("fs::floors", Adapter),
            ("fs::packages", Adapter),
            ("fs::revocation", Adapter),
            ("listen", Adapter),
            ("migrate", Adapter),
            ("pki", Adapter),
            ("pkcs11", Adapter),
            ("postgres", Adapter),
            ("postgres::agent_page", Adapter),
            ("postgres::agents", Adapter),
            ("postgres::bootstrap", Adapter),
            ("postgres::checkpoint", Adapter),
            ("postgres::configs", Adapter),
            ("postgres::deployments", Adapter),
            ("postgres::enrolment", Adapter),
            ("postgres::floors", Adapter),
            ("postgres::audit", Adapter),
            ("postgres::audit_prune", Adapter),
            ("postgres::import", Adapter),
            ("postgres::lease", Adapter),
            ("postgres::ledger", Adapter),
            ("postgres::notify", Adapter),
            ("postgres::packages", Adapter),
            ("postgres::schema", Adapter),
            ("s3", Adapter),
            // Test-only: the log capture the unit tests share, bound to `tracing_subscriber`.
            ("test_support", Adapter),
            ("tls", Adapter),
            ("transport", Adapter),
        ],
    ),
    (
        "fleet-agent",
        &[
            ("", Root),
            ("main", Root),
            ("bin::stub_agent", Root),
            ("bin::stub_crasher", Root),
            ("supervisor", Root),
            ("service::enrol", Root),
            ("service::runtime", Root),
            ("config", Core),
            ("engine", Core),
            ("product", Core),
            ("shutdown", Core),
            ("span", Core),
            ("supervisor::agent", Core),
            ("supervisor::agent::connection_settings", Core),
            ("supervisor::agent::description", Core),
            ("supervisor::agent::packages", Core),
            ("supervisor::agent::remote_config", Core),
            ("supervisor::block", Core),
            ("supervisor::delivered", Core),
            // The Plugin port of CLT-wrappers-act-through-the-core: plain data and calls.
            ("supervisor::port", Core),
            ("supervisor::ports", Core),
            ("supervisor::restart", Core),
            ("update", Core),
            ("cli", Adapter),
            ("config_file", Adapter),
            ("config_init", Adapter),
            ("connection", Adapter),
            ("csr", Adapter),
            ("floors", Adapter),
            ("gateway", Adapter),
            ("gateway::cache", Adapter),
            ("gateway::floors", Adapter),
            ("gateway::policy", Core),
            ("gateway::pool", Adapter),
            // The downstream hosts' buckets, read against the Gateway's clock (CLT-gateway-rate-limits-hosts).
            ("gateway::rate", Adapter),
            ("gateway::registry", Adapter),
            ("gateway::revocations", Adapter),
            ("gateway::store", Adapter),
            ("host", Adapter),
            ("install", Adapter),
            ("logging", Adapter),
            ("packages", Adapter),
            ("reconfigure", Adapter),
            ("service", Adapter),
            ("service::layout", Adapter),
            ("service::manager", Adapter),
            ("service::run_as", Adapter),
            ("service::windows", Adapter),
            ("service::windows_config", Adapter),
            ("service::windows_rights", Adapter),
            ("service::withdrawal", Adapter),
            ("storage", Adapter),
            ("supervisor::channel", Adapter),
            ("supervisor::channel::windows", Adapter),
            ("supervisor::collector", Adapter),
            ("supervisor::command", Adapter),
            ("supervisor::endpoint", Adapter),
            // The core's side of the Plugin port, over the process runner.
            ("supervisor::host", Adapter),
            // A Plugin's roots and the file operations in them, through the no-follow adapter.
            ("supervisor::host::files", Adapter),
            // The timers a Plugin sets.
            ("supervisor::host::timers", Adapter),
            ("supervisor::process", Adapter),
            ("telemetry", Adapter),
            // Test-only: the log capture the unit tests share, bound to `tracing_subscriber`.
            ("test_support", Adapter),
            ("tls", Adapter),
            ("transport", Adapter),
            ("update::installer", Adapter),
            ("win32", Adapter),
            ("win32::acl", Adapter),
            ("win32::files", Adapter),
        ],
    ),
    (
        "fleet-tools",
        &[
            // One command-line program and no library (OPS-operator-program-fleetctl): nothing here is core, and a
            // module added to the crate has to be given a role like anywhere else.
            ("main", Root),
            ("fetch", Adapter),
            ("fetch::upstream", Adapter),
            ("gpgv", Adapter),
            ("package", Adapter),
            ("pki", Adapter),
            ("signing", Adapter),
            ("token", Adapter),
        ],
    ),
];

/// The external crates the core may name: data formats, codecs, the logging facade, and the buffer
/// a secret is wiped from on drop (SEC-secrets-wiped-on-drop) — none of them reaches a file, a socket, a process, or
/// a clock.
const CORE_CRATES: &[&str] = &[
    "prost", "serde", "sha2", "hex", "base64", "uuid", "flate2", "tracing", "zeroize",
];

/// What the core may name of a technology, by path prefix. `tokio::sync` is channels, which need
/// no runtime and do no I/O — and CLT-supervisor-mode-and-kinds makes a Port a message pair over them; `tokio::select`
/// and `tokio::pin` are how the core waits on several of them, control flow over futures that
/// starts no task and touches nothing outside the process. `toml::Table` and
/// `toml::Value` are the configuration's values (SRV-configuration-toml-and-environment §1), which CLT-supervisor-mode-and-kinds hands a
/// Plugin as its block; reading and parsing the file stays at the edge. An address is a value, and
/// `env::consts` is fixed at compile time.
const CORE_PREFIXES: &[&[&str]] = &[
    &["tokio", "sync"],
    &["tokio", "select"],
    &["tokio", "pin"],
    &["toml", "Table"],
    &["toml", "Value"],
    &["std", "net", "IpAddr"],
    &["std", "net", "Ipv4Addr"],
    &["std", "net", "Ipv6Addr"],
    &["std", "net", "SocketAddr"],
    &["std", "env", "consts"],
];

/// The parts of the standard library that are technology rather than language.
const STD_TECHNOLOGY: &[&str] = &["fs", "net", "process", "env", "thread", "os"];

/// The clocks: reading the time is a technology, holding a time is not.
const CLOCKS: &[&str] = &["SystemTime", "Instant"];

/// The workspace crates a core module may reach into, by the name code uses for them.
const WORKSPACE_CRATES: &[(&str, &str)] = &[
    ("opamp", "opamp"),
    ("fleet_core", "fleet-core"),
    ("fleet_util", "fleet-util"),
];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn roles_of(krate: &str) -> &'static [(&'static str, Role)] {
    ROLES
        .iter()
        .find(|(k, _)| *k == krate)
        .map(|(_, r)| *r)
        .unwrap_or_else(|| panic!("no roles for crate {krate}"))
}

/// Whether a path into `krate` leaves the core: the longest module prefix decides, and a module
/// that is not core — an adapter, or a composition root — is outside it. A path
/// that matches no module names an item of the crate root, which is the crate's own vocabulary.
fn leaves_the_core(krate: &str, segments: &[String]) -> bool {
    let (module, role) = module_of(krate, segments);
    !module.is_empty() && role != Core
}

fn module_path(src: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(src).unwrap().with_extension("");
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if matches!(parts.last().map(String::as_str), Some("mod" | "lib")) {
        parts.pop();
    }
    parts.join("::")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The names of the external crates in the lockfile, as code spells them. The workspace's own
/// members are not technology: a path into opamp or fleet-core is judged by their roles.
fn locked_crates() -> BTreeSet<String> {
    let lock = fs::read_to_string(crates_dir().join("../Cargo.lock")).unwrap();
    let members: Vec<String> = ROLES.iter().map(|(k, _)| k.replace('-', "_")).collect();
    lock.lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .map(|n| n.trim_end_matches('"').replace('-', "_"))
        .filter(|n| !members.contains(n))
        .collect()
}

#[derive(Clone, PartialEq, Debug)]
enum Token {
    Ident(String),
    PathSep,
    Punct(char),
}

/// The shipped code of Rust source `src`, as [`source::shipped`] cuts it, as identifiers, `::`,
/// and punctuation, each token with its line. Comments, literals and the quote of a lifetime are
/// no tokens; a doc comment leaves its attribute's `doc`, which names no path.
fn tokenize(src: &str) -> Vec<(Token, usize)> {
    let file = source::shipped(src).expect("the source parses");
    let mut out = Vec::new();
    push_tokens(file.to_token_stream(), &mut out);
    out
}

/// Appends the tokens of `stream` to `out`, as [`tokenize`] reads them. A group's delimiters
/// stand as tokens of their own.
fn push_tokens(stream: TokenStream, out: &mut Vec<(Token, usize)>) {
    let trees: Vec<TokenTree> = stream.into_iter().collect();
    let mut i = 0;
    while i < trees.len() {
        let line = trees[i].span().start().line;
        match &trees[i] {
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                let name = name.strip_prefix("r#").unwrap_or(&name);
                out.push((Token::Ident(name.to_owned()), line));
            }
            TokenTree::Punct(p)
                if p.as_char() == ':'
                    && p.spacing() == Spacing::Joint
                    && matches!(trees.get(i + 1), Some(TokenTree::Punct(next)) if next.as_char() == ':') =>
            {
                out.push((Token::PathSep, line));
                i += 1;
            }
            TokenTree::Punct(p) if p.as_char() == '\'' => {}
            TokenTree::Punct(p) => out.push((Token::Punct(p.as_char()), line)),
            TokenTree::Group(group) => {
                let delimiters = match group.delimiter() {
                    Delimiter::Parenthesis => Some(('(', ')')),
                    Delimiter::Brace => Some(('{', '}')),
                    Delimiter::Bracket => Some(('[', ']')),
                    Delimiter::None => None,
                };
                if let Some((open, _)) = delimiters {
                    out.push((Token::Punct(open), group.span_open().start().line));
                }
                push_tokens(group.stream(), out);
                if let Some((_, close)) = delimiters {
                    out.push((Token::Punct(close), group.span_close().start().line));
                }
            }
            TokenTree::Literal(_) => {}
        }
        i += 1;
    }
}

/// A path the source names, where, and — for `use … as` — the name it is known by.
struct Named {
    segments: Vec<String>,
    line: usize,
    alias: Option<String>,
}

impl Named {
    /// The name a `use` makes the path known by in the declaring module.
    fn name(&self) -> Option<&str> {
        self.alias
            .as_deref()
            .or(self.segments.last().map(String::as_str))
    }
}

/// A token and the line it is on.
type Located = (Token, usize);

/// The tokens split into the `pub use` declarations — re-exports, a module's API rather than a
/// dependency of its own code — and everything else.
fn split_reexports(tokens: Vec<Located>) -> (Vec<Located>, Vec<Located>) {
    let (mut rest, mut reexports) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < tokens.len() {
        let mut j = i;
        if tokens[j].0 == Token::Ident("pub".into()) {
            j += 1;
            if tokens.get(j).is_some_and(|t| t.0 == Token::Punct('(')) {
                while tokens.get(j).is_some_and(|t| t.0 != Token::Punct(')')) {
                    j += 1;
                }
                j += 1;
            }
            if tokens
                .get(j)
                .is_some_and(|t| t.0 == Token::Ident("use".into()))
            {
                while j < tokens.len() && tokens[j].0 != Token::Punct(';') {
                    reexports.push(tokens[j].clone());
                    j += 1;
                }
                reexports.push((Token::Punct(';'), tokens.get(j).map_or(0, |t| t.1)));
                i = j + 1;
                continue;
            }
        }
        rest.push(tokens[i].clone());
        i += 1;
    }
    (rest, reexports)
}

/// Every path the tokens name, with `use` groups expanded: `use std::{fs, io::Read}` is
/// `std::fs` and `std::io::Read`.
fn paths(tokens: &[(Token, usize)]) -> Vec<Named> {
    fn path_at(
        tokens: &[(Token, usize)],
        mut i: usize,
        prefix: Vec<String>,
        out: &mut Vec<Named>,
    ) -> usize {
        let mut segments = prefix;
        let line = tokens[i].1;
        loop {
            match &tokens[i].0 {
                Token::Ident(name) => {
                    segments.push(name.clone());
                    i += 1;
                    // `use a::b as c`: the alias names nothing new, but a re-export is known by it.
                    if tokens
                        .get(i)
                        .is_some_and(|t| t.0 == Token::Ident("as".into()))
                    {
                        if segments.last().is_some_and(|s| s == "self") {
                            segments.pop();
                        }
                        let alias = match tokens.get(i + 1) {
                            Some((Token::Ident(alias), _)) => Some(alias.clone()),
                            _ => None,
                        };
                        out.push(Named {
                            segments,
                            line,
                            alias,
                        });
                        return i + 2;
                    }
                }
                Token::Punct('{') => {
                    i += 1;
                    while i < tokens.len() && tokens[i].0 != Token::Punct('}') {
                        if tokens[i].0 == Token::Punct(',') {
                            i += 1;
                        } else {
                            i = path_at(tokens, i, segments.clone(), out);
                        }
                    }
                    return i + 1;
                }
                _ => {
                    out.push(Named {
                        segments,
                        line,
                        alias: None,
                    });
                    return i;
                }
            }
            if i < tokens.len() && tokens[i].0 == Token::PathSep {
                i += 1;
            } else {
                if segments.last().is_some_and(|s| s == "self") {
                    segments.pop();
                }
                out.push(Named {
                    segments,
                    line,
                    alias: None,
                });
                return i;
            }
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let starts_path = matches!(tokens[i].0, Token::Ident(_))
            && tokens.get(i + 1).is_some_and(|t| t.0 == Token::PathSep)
            && (i == 0 || !matches!(tokens[i - 1].0, Token::PathSep | Token::Punct('.')));
        if starts_path {
            i = path_at(tokens, i, Vec::new(), &mut out);
        } else {
            i += 1;
        }
    }
    out
}

/// Where a path leads: an item of one of the checked crates, by its module path there, or
/// anything else.
#[derive(Clone, Debug)]
enum Target {
    Internal(&'static str, Vec<String>),
    External(Vec<String>),
}

/// What the `pub use` declarations of every checked crate re-export: by crate, module and the name
/// the item is known by there, where it really is.
type Reexports = BTreeMap<(&'static str, String, String), Target>;

/// Where `path`, named in module `module` of `krate`, leads. `crate`, `self` and `super` are this
/// crate; a bare first segment that is a child module of `module` is too, as Rust 2018 resolves
/// it; a workspace crate is that crate.
fn resolve(krate: &'static str, module: &str, path: &[String]) -> Target {
    let segments = |m: &str| -> Vec<String> {
        m.split("::")
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let Some(root) = path.first().map(String::as_str) else {
        return Target::External(Vec::new());
    };
    match root {
        "crate" => Target::Internal(krate, path[1..].to_vec()),
        "self" | "super" => {
            let mut base = segments(module);
            let mut rest = path;
            if rest.first().is_some_and(|s| s == "self") {
                rest = &rest[1..];
            }
            while rest.first().is_some_and(|s| s == "super") {
                base.pop();
                rest = &rest[1..];
            }
            base.extend(rest.iter().cloned());
            Target::Internal(krate, base)
        }
        _ => {
            if let Some(&(_, dir)) = WORKSPACE_CRATES.iter().find(|(name, _)| *name == root) {
                return Target::Internal(dir, path[1..].to_vec());
            }
            let child = if module.is_empty() {
                root.to_owned()
            } else {
                format!("{module}::{root}")
            };
            if roles_of(krate).iter().any(|(m, _)| *m == child) {
                let mut base = segments(module);
                base.extend(path.iter().cloned());
                return Target::Internal(krate, base);
            }
            Target::External(path.to_vec())
        }
    }
}

/// Why `path`, named in module `module` of `krate`, breaks the rule for a core module — if it does.
fn violation(
    krate: &'static str,
    module: &str,
    path: &[String],
    locked: &BTreeSet<String>,
    reexports: &Reexports,
) -> Option<String> {
    let shown = path.join("::");
    if path
        .windows(2)
        .any(|w| CLOCKS.contains(&w[0].as_str()) && w[1] == "now")
    {
        return Some(format!("{shown}: reads a clock"));
    }
    judge(&resolve(krate, module, path), locked, reexports, 0).map(|why| format!("{shown}: {why}"))
}

/// Why a path that leads to `target` breaks the rule — following a re-export to where the item
/// really is, so a core module re-exporting an adapter's item does not launder it.
fn judge(
    target: &Target,
    locked: &BTreeSet<String>,
    reexports: &Reexports,
    depth: usize,
) -> Option<String> {
    match target {
        Target::External(path) => {
            if CORE_PREFIXES
                .iter()
                .any(|p| path.len() >= p.len() && p.iter().zip(path).all(|(a, b)| a == b))
            {
                return None;
            }
            let root = path.first()?.as_str();
            match root {
                "std" | "core" | "alloc" => path
                    .get(1)
                    .filter(|m| STD_TECHNOLOGY.contains(&m.as_str()))
                    .map(|_| "a technology of the standard library".to_owned()),
                _ => (locked.contains(root) && !CORE_CRATES.contains(&root))
                    .then(|| format!("the technology crate {root}")),
            }
        }
        Target::Internal(krate, segments) => {
            if leaves_the_core(krate, segments) {
                return Some(format!("outside the core of {krate}"));
            }
            if depth > 8 {
                return None;
            }
            // The item the path names, in the deepest module it passes through.
            let module_len = (0..=segments.len())
                .rev()
                .find(|&n| {
                    let module = segments[..n].join("::");
                    roles_of(krate).iter().any(|(m, _)| *m == module)
                })
                .unwrap_or(0);
            let name = segments.get(module_len)?;
            let key = (*krate, segments[..module_len].join("::"), name.clone());
            let reexported = reexports.get(&key)?;
            let mut through = reexported.clone();
            let rest = &segments[module_len + 1..];
            match &mut through {
                Target::Internal(_, s) | Target::External(s) => s.extend(rest.iter().cloned()),
            }
            judge(&through, locked, reexports, depth + 1).map(|why| format!("{why}, re-exported"))
        }
    }
}

/// The `pub use` declarations of every checked crate, test code aside.
fn reexports() -> Reexports {
    let mut found = Reexports::new();
    for &(krate, _) in ROLES {
        let src = crates_dir().join(krate).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for file in files {
            let module = module_path(&src, &file);
            let tokens = tokenize(&fs::read_to_string(&file).unwrap());
            let (_, declared) = split_reexports(tokens);
            for named in paths(&declared) {
                if let Some(name) = named.name() {
                    let target = resolve(krate, &module, &named.segments);
                    found.insert((krate, module.clone(), name.to_owned()), target);
                }
            }
        }
    }
    found
}

/// Verifies: DEV-ports-and-adapters, G-8
#[test]
fn the_core_depends_on_no_adapter_and_no_technology() {
    let locked = locked_crates();
    let reexports = reexports();
    let (mut broken, mut unclassified, mut stale) = (Vec::new(), Vec::new(), Vec::new());
    for &(krate, roles) in ROLES {
        let src = crates_dir().join(krate).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        files.sort();
        let mut modules = Vec::new();
        for file in &files {
            let module = module_path(&src, file);
            let tokens = tokenize(&fs::read_to_string(file).unwrap());
            modules.extend(inline_modules(&module, &tokens));
            modules.push(module);
        }
        stale.extend(
            roles_without_module(roles, &modules)
                .into_iter()
                .map(|m| format!("{krate}: {m}")),
        );
        for file in files {
            let module = module_path(&src, &file);
            let Some(&(_, role)) = roles.iter().find(|(m, _)| *m == module) else {
                unclassified.push(format!("{krate}: {module}"));
                continue;
            };
            if role != Core {
                continue;
            }
            let tokens = tokenize(&fs::read_to_string(&file).unwrap());
            let (own, _) = split_reexports(tokens);
            let found: Vec<String> = paths(&own)
                .into_iter()
                .filter_map(|Named { segments, line, .. }| {
                    violation(krate, &module, &segments, &locked, &reexports).map(|why| {
                        format!(
                            "{}:{line}: {why}",
                            file.strip_prefix(crates_dir()).unwrap().display()
                        )
                    })
                })
                .collect();
            broken.extend(found);
        }
    }
    assert!(
        unclassified.is_empty(),
        "modules without a role in ROLES: {unclassified:#?}"
    );
    assert!(
        stale.is_empty(),
        "roles in ROLES without a source file: {stale:#?}"
    );
    assert!(
        broken.is_empty(),
        "a core module depends on an adapter or a technology: {broken:#?}"
    );
}

/// The modules a file declares with a body of their own (`mod name { … }`), as module paths
/// under the file's own module. `opamp::proto` is one: it has a role and no file of its own.
fn inline_modules(module: &str, tokens: &[(Token, usize)]) -> Vec<String> {
    tokens
        .windows(3)
        .filter_map(|w| match (&w[0].0, &w[1].0, &w[2].0) {
            (Token::Ident(m), Token::Ident(name), Token::Punct('{')) if m == "mod" => {
                Some(if module.is_empty() {
                    name.clone()
                } else {
                    format!("{module}::{name}")
                })
            }
            _ => None,
        })
        .collect()
}

/// The roles of a crate that name no module of its source files, nor one declared inline in
/// them: a module removed or moved without its entry.
fn roles_without_module(roles: &[(&'static str, Role)], modules: &[String]) -> Vec<&'static str> {
    roles
        .iter()
        .map(|&(m, _)| m)
        .filter(|m| !modules.iter().any(|f| f == m))
        .collect()
}

/// The reverse half of the table's check: a role is reported once its module has no file, and
/// not while it has one.
#[test]
fn a_role_without_a_source_file_is_reported() {
    let roles: &[(&str, Role)] = &[("", Root), ("fs", Adapter), ("fs::gone", Adapter)];
    let modules = vec![String::new(), "fs".to_owned()];
    assert_eq!(roles_without_module(roles, &modules), vec!["fs::gone"]);
    let modules = vec![String::new(), "fs".to_owned(), "fs::gone".to_owned()];
    assert!(roles_without_module(roles, &modules).is_empty());

    let src = "pub mod proto { include!(\"x.rs\"); }\nmod file;\n#[cfg(test)]\nmod tests {}";
    let tokens = tokenize(src);
    assert_eq!(inline_modules("", &tokens), vec!["proto"]);
    assert_eq!(inline_modules("fs", &tokens), vec!["fs::proto"]);
}

/// The reader the test stands on: a path in a string, a comment, or test code is not a
/// dependency, a `use` group names each of its members, and each path names its line.
#[test]
fn the_reader_finds_the_paths_code_names() {
    let src = r##"
        use std::{fs, io::Read};
        use crate::client::{self, ws::Driver as D};
        // tokio::spawn in a comment
        /* reqwest::get /* nested */ in a block */
        const S: &str = "axum::Router";
        const R: &str = r#"rustls::"#;
        fn f<'a>(c: char) -> &'a str { let _ = '"'; tracing::info!("x"); x.y::<u8>(); "" }
        #[cfg(test)]
        mod tests { use tokio::net::TcpStream; }
        #[cfg(test)]
        fn g() { libc::getpid(); }
        fn h() { std::time::Instant::now(); }
    "##;
    let named = paths(&tokenize(src));
    let found: Vec<String> = named
        .iter()
        .map(|named| named.segments.join("::"))
        .collect();
    // Each path carries the line of the source it stands on.
    let lines: Vec<usize> = named.iter().map(|named| named.line).collect();
    assert_eq!(lines, [2, 2, 3, 3, 8, 13]);
    assert_eq!(
        found,
        [
            "std::fs",
            "std::io::Read",
            "crate::client",
            "crate::client::ws::Driver",
            "tracing::info",
            "std::time::Instant::now",
        ]
    );
}

/// The rule's boundary between a value and a technology: a channel, a configuration value, and an
/// address are values; a socket, a file, a clock reading, and an adapter of the crate are not.
#[test]
fn the_rule_tells_a_value_from_a_technology() {
    let locked: BTreeSet<String> = ["tokio", "toml", "reqwest"].map(String::from).into();
    let judged = |path: &str| {
        let segments: Vec<String> = path.split("::").map(String::from).collect();
        violation(
            "fleet-agent",
            "supervisor::ports",
            &segments,
            &locked,
            &Reexports::new(),
        )
        .is_some()
    };
    for value in [
        "tokio::sync::mpsc",
        "toml::Table",
        "std::net::IpAddr",
        "std::env::consts::OS",
        "std::time::Duration",
        "crate::shutdown::Shutdown",
        "super::agent::PackageDownload",
        "opamp::client::protocol::AgentProtocol",
    ] {
        assert!(!judged(value), "{value} is a value the core may name");
    }
    for technology in [
        "tokio::net::TcpStream",
        "toml::from_str",
        "reqwest::Client",
        "std::net::TcpStream",
        "std::fs::read",
        "std::env::var",
        "std::time::SystemTime::now",
        "crate::service::runtime::run",
        "super::process::Runner",
        "opamp::client::ws::run",
    ] {
        assert!(
            judged(technology),
            "{technology} is a technology the core may not name"
        );
    }
}

/// A re-export does not launder an adapter: a path through a core module's `pub use` is judged by
/// where the item really is — and a bare child module is the module it names, as Rust 2018 has it.
#[test]
fn a_reexport_is_judged_by_where_the_item_is() {
    let locked = locked_crates();
    let reexports = reexports();
    let judged = |krate: &'static str, module: &str, path: &str| {
        let segments: Vec<String> = path.split("::").map(String::from).collect();
        violation(krate, module, &segments, &locked, &reexports)
    };
    // `opamp::client` is core and re-exports the backoff, whose jitter reads the system's randomness.
    assert!(judged(
        "fleet-agent",
        "supervisor::restart",
        "opamp::client::Backoff"
    )
    .is_some());
    assert!(judged("opamp", "client", "backoff::Backoff").is_some());
    // What `opamp::client` defines itself is core.
    assert!(judged(
        "fleet-agent",
        "supervisor::restart",
        "opamp::client::Session"
    )
    .is_none());
}

/// Composition roots no adapter may name, by crate and module, and why: an adapter that calls one
/// runs what the root wires, and the root is no longer the outermost ring.
const ROOTS_NO_ADAPTER_NAMES: &[(&str, &str, &str)] = &[(
    "fleet-agent",
    "service::enrol",
    "the privileged enrolment; the walk a privileged read takes lives in the adapter `nofollow`",
)];

/// The longest module of `krate` that `segments` lies in, and its role.
fn module_of(krate: &str, segments: &[String]) -> (&'static str, Role) {
    let depth = |module: &str| module.split("::").filter(|s| !s.is_empty()).count();
    let mut best = ("", Root);
    for &(module, role) in roles_of(krate) {
        let parts: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
        if parts.len() > depth(best.0)
            && segments.len() >= parts.len()
            && parts.iter().zip(segments).all(|(a, b)| a == b)
        {
            best = (module, role);
        }
    }
    best
}

/// Verifies: DEV-ports-and-adapters
#[test]
fn no_adapter_names_the_composition_roots_kept_from_adapters() {
    let mut found = Vec::new();
    for &(krate, roles) in ROLES {
        let src = crates_dir().join(krate).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        files.sort();
        for file in files {
            let module = module_path(&src, &file);
            if !roles
                .iter()
                .any(|&(m, role)| m == module && role == Adapter)
            {
                continue;
            }
            let tokens = tokenize(&fs::read_to_string(&file).unwrap());
            let (own, _) = split_reexports(tokens);
            for Named { segments, line, .. } in paths(&own) {
                let Target::Internal(target, segments) = resolve(krate, &module, &segments) else {
                    continue;
                };
                let (root, _) = module_of(target, &segments);
                if let Some((_, _, why)) = ROOTS_NO_ADAPTER_NAMES
                    .iter()
                    .find(|(k, m, _)| *k == target && *m == root)
                {
                    found.push(format!(
                        "{}:{line}: names {target}::{root} ({why})",
                        file.strip_prefix(crates_dir()).unwrap().display()
                    ));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "an adapter names a composition root: {found:#?}"
    );
}

/// What each module of a plane may not name in the Server (SRV-two-use-case-layers §2 and §4), by
/// module, with its submodules: an endpoint module names neither the other plane's use cases nor the
/// other plane's endpoints, a use-case module names neither the other plane's use cases nor an
/// endpoint, and `answers`, which both endpoints use, names neither plane.
const PLANE_REACH: &[(&str, &[&str])] = &[
    ("transport", &["operator_cases", "api"]),
    ("api", &["agent_cases", "transport"]),
    ("agent_cases", &["operator_cases", "api", "transport"]),
    ("operator_cases", &["agent_cases", "api", "transport"]),
    (
        "answers",
        &["agent_cases", "operator_cases", "api", "transport"],
    ),
];

/// The modules that reach no state, with their submodules: the endpoint modules, which reach it
/// only through their own plane's use-case module (SRV-two-use-case-layers §2), and `answers`,
/// which both endpoints use.
const STATELESS: &[&str] = &["transport", "api", "answers"];

/// The Server's types that hold state, and its ports, which no endpoint module names
/// (SRV-two-use-case-layers §2). The list is kept by hand: a new type that holds state, or a new
/// port, is added here when it is written.
const STATE_HOLDERS: &[&str] = &[
    "AppState",
    "ConfigStore",
    "DeploymentStore",
    "PackageStore",
    "PackageOffering",
    "Enrolment",
    "Revocations",
    "BootstrapCertificates",
    "Throttle",
    "AgentRate",
    "Audit",
    "CertificateSigner",
];

/// Whether `module` is `parent` or lies beneath it.
fn within(module: &str, parent: &str) -> bool {
    module == parent || module.starts_with(&format!("{parent}::"))
}

/// What the shipped code `src` of the Server's module `module` reaches that its plane may not:
/// a module of [`PLANE_REACH`], through any path or a re-export of the crate, and for an endpoint
/// a type of [`STATE_HOLDERS`], by name.
fn plane_reach(module: &str, src: &str, reexports: &Reexports) -> Vec<String> {
    let krate = "fleet-server";
    let tokens = tokenize(src);
    let mut found = Vec::new();
    let forbidden: Vec<&str> = PLANE_REACH
        .iter()
        .filter(|(m, _)| within(module, m))
        .flat_map(|(_, names)| names.iter().copied())
        .collect();
    for Named { segments, line, .. } in paths(&tokens) {
        let mut target = resolve(krate, module, &segments);
        // A path through a re-export is judged by where the item really is.
        for _ in 0..4 {
            let Target::Internal(k, ref s) = target else {
                break;
            };
            let Some((name, at)) = s.split_last() else {
                break;
            };
            match reexports.get(&(k, at.join("::"), name.clone())) {
                Some(next) => target = next.clone(),
                None => break,
            }
        }
        let Target::Internal(k, segments) = target else {
            continue;
        };
        if k != krate {
            continue;
        }
        let (reached, _) = module_of(k, &segments);
        if let Some(name) = forbidden.iter().find(|name| within(reached, name)) {
            found.push(format!(
                "{module}:{line}: names {name} ({})",
                segments.join("::")
            ));
        }
    }
    if STATELESS.iter().any(|stateless| within(module, stateless)) {
        for (token, line) in &tokens {
            if let Token::Ident(name) = token {
                if STATE_HOLDERS.contains(&name.as_str()) {
                    found.push(format!("{module}:{line}: names {name}, which holds state"));
                }
            }
        }
    }
    found
}

/// Each plane reaches the core through its own use-case module alone: `transport` through
/// `agent_cases`, `api` through `operator_cases`, neither names the other plane's modules nor a
/// type that holds state, and the use-case modules do not name each other.
///
/// Verifies: SRV-two-use-case-layers
#[test]
fn each_plane_reaches_the_core_through_its_own_use_cases() {
    let reexports = reexports();
    let src = crates_dir().join("fleet-server").join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    for file in files {
        let module = module_path(&src, &file);
        if !PLANE_REACH.iter().any(|(m, _)| within(&module, m)) {
            continue;
        }
        for (m, _) in PLANE_REACH {
            if within(&module, m) {
                seen.insert(*m);
            }
        }
        found.extend(plane_reach(
            &module,
            &fs::read_to_string(&file).unwrap(),
            &reexports,
        ));
    }
    // A module of the table that is gone fails, so the table never names a module the tree lacks.
    let missing: Vec<&str> = PLANE_REACH
        .iter()
        .map(|(m, _)| *m)
        .filter(|m| !seen.contains(m))
        .collect();
    assert!(
        missing.is_empty(),
        "modules of PLANE_REACH without a source file: {missing:?}"
    );
    assert!(
        found.is_empty(),
        "a plane reaches past its own use-case module: {found:#?}"
    );
}

/// The check the plane test stands on: a path to the other plane's modules, a type that holds
/// state in an endpoint, and either one in test code or in a string, each judged as it should be.
#[test]
fn the_plane_check_finds_what_a_plane_may_not_name() {
    let reexports = Reexports::new();
    let endpoint = r##"
        use crate::agent_cases::AgentCases;
        fn f() -> Response { crate::transport::busy() }
        fn g(state: &crate::fleet::AppState) {}
        const S: &str = "Throttle";
        #[cfg(test)]
        mod tests { use crate::fleet::AppState; use crate::transport::busy; }
    "##;
    let found = plane_reach("api", endpoint, &reexports);
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found[0].contains("names agent_cases"));
    assert!(found[1].contains("names transport"));
    assert!(found
        .iter()
        .any(|f| f.contains("names AppState, which holds state")));
    // A use-case module may name the core's state; it may not name the other plane.
    let cases = r##"
        use crate::fleet::AppState;
        use crate::operator_cases::OperatorCases;
    "##;
    let found = plane_reach("agent_cases", cases, &reexports);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].contains("names operator_cases"));
    // A submodule of an endpoint is held to its rule.
    let paging = "fn f() { let _ = crate::agent_cases::Proofs::default(); }";
    assert_eq!(plane_reach("api::paging", paging, &reexports).len(), 1);
}

/// The contexts of the Server's fleet core, from the bottom (SRV-fleet-contexts-call-by-rank §1):
/// the module root first, each context naming only those before it. A submodule of a context
/// takes the context's rank.
const FLEET_RANKS: &[&str] = &[
    "fleet",
    "fleet::placement",
    "fleet::held",
    "fleet::delivery",
    "fleet::rollout",
    "fleet::views",
    "fleet::approval",
    "fleet::session",
];

/// The submodules of `fleet` that only the tests build, and so have no rank (§6).
const FLEET_TEST_ONLY: &[&str] = &["fleet::fixtures"];

/// The context of `fleet` that `module` lies in: the root itself, or its submodule `module` is
/// or lies beneath. `None` outside `fleet`.
fn fleet_context(module: &str) -> Option<String> {
    let mut parts = module.split("::");
    if parts.next() != Some("fleet") {
        return None;
    }
    Some(match parts.next() {
        Some(context) => format!("fleet::{context}"),
        None => "fleet".to_owned(),
    })
}

/// The rank of a context of `fleet`, by [`FLEET_RANKS`].
fn fleet_rank(context: &str) -> Option<usize> {
    FLEET_RANKS.iter().position(|c| *c == context)
}

/// The bodies of the `impl AppState` and `impl … for AppState` blocks of `tokens`, each from its
/// `{` to its `}`. A `for AppState {` opens only an impl block: a `for` loop's pattern is followed
/// by `in`.
fn app_state_blocks(tokens: &[Located]) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 2 < tokens.len() {
        let opens = matches!(&tokens[i].0, Token::Ident(k) if k == "impl" || k == "for")
            && tokens[i + 1].0 == Token::Ident("AppState".into())
            && tokens[i + 2].0 == Token::Punct('{');
        if !opens {
            i += 1;
            continue;
        }
        let mut depth = 0;
        let mut j = i + 2;
        while j < tokens.len() {
            match tokens[j].0 {
                Token::Punct('{') => depth += 1,
                Token::Punct('}') => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        found.push(i + 2..j + 1);
        i = j;
    }
    found
}

/// The methods the `impl AppState` and `impl … for AppState` blocks of `tokens` define, by name.
fn app_state_methods(tokens: &[Located]) -> Vec<String> {
    let mut found = Vec::new();
    for block in app_state_blocks(tokens) {
        let mut depth = 0;
        for i in block {
            match &tokens[i].0 {
                Token::Punct('{') => depth += 1,
                Token::Punct('}') => depth -= 1,
                Token::Ident(fn_) if fn_ == "fn" && depth == 1 => {
                    if let Some((Token::Ident(name), _)) = tokens.get(i + 1) {
                        found.push(name.clone());
                    }
                }
                _ => {}
            }
        }
    }
    found
}

/// The ranges of `tokens` that are `use` declarations, `pub use` among them.
fn use_declarations(tokens: &[Located]) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i].0 == Token::Ident("use".into()) {
            let start = i;
            while i < tokens.len() && tokens[i].0 != Token::Punct(';') {
                i += 1;
            }
            found.push(start..i);
        }
        i += 1;
    }
    found
}

/// Where `target` really is, following the re-exports of the Server.
fn through_reexports(mut target: Target, reexports: &Reexports) -> Target {
    for _ in 0..8 {
        let Target::Internal(k, ref s) = target else {
            break;
        };
        let Some((name, at)) = s.split_last() else {
            break;
        };
        match reexports.get(&(k, at.join("::"), name.clone())) {
            Some(next) => target = next.clone(),
            None => break,
        }
    }
    target
}

/// The context of `fleet` that a path, named in `module`, really leads to.
fn fleet_context_reached(
    module: &str,
    segments: &[String],
    reexports: &Reexports,
) -> Option<String> {
    let krate = "fleet-server";
    let Target::Internal(k, segments) =
        through_reexports(resolve(krate, module, segments), reexports)
    else {
        return None;
    };
    if k != krate {
        return None;
    }
    let (reached, _) = module_of(k, &segments);
    fleet_context(reached)
}

/// What the module root's `use` declarations bring into scope from a context, by the name they
/// bring it in as: what a context's `use super::*`, and the root's own code, reach through them.
fn fleet_root_imports(root: &[Located], reexports: &Reexports) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for range in use_declarations(root) {
        for named in paths(&root[range]) {
            let (Some(name), Some(context)) = (
                named.name(),
                fleet_context_reached("fleet", &named.segments, reexports),
            ) else {
                continue;
            };
            if context != "fleet" {
                found.insert(name.to_owned(), context);
            }
        }
    }
    found
}

/// What the shipped code `src` of `module`, a ranked module of `fleet`, names at or above its own
/// context's rank (SRV-fleet-contexts-call-by-rank §2 and §3): a method of `methods`, by the
/// context that defines it on `AppState`, called on `self`, `Self::` or `AppState::`; a path or a
/// `use` that leads into such a context, through a re-export or not; and, in the root, a name its
/// `use` declarations bring in from one, in `imports`. The root's own `pub use` declarations are
/// not its code.
///
/// Its limits, which review covers (SRV-fleet-contexts-call-by-rank, *Consequences*):
/// - it resolves `super::` from the file's module, so a `mod` with a body inside a context,
///   outside the tests, would have its paths resolved one level off; no context has one;
/// - it collects methods from blocks written `impl AppState` or `impl … for AppState` alone, not
///   from `impl super::AppState` or `impl crate::fleet::AppState`;
/// - it resolves calls on `self`, `Self::` and `AppState::` alone, so a call on another binding
///   of `AppState` (`state.place_all()`) is not seen;
/// - it resolves calls of `AppState`'s methods alone, so a method a context defines on another
///   type of the state, called through a field (`self.kept.forget(..)`), is not seen; matching
///   such calls by name would take every `lock` or `remove` of the standard library for one.
fn rank_breaches(
    module: &str,
    src: &str,
    methods: &BTreeMap<String, String>,
    imports: &BTreeMap<String, String>,
    reexports: &Reexports,
) -> Vec<String> {
    let Some(own) = fleet_context(module) else {
        return Vec::new();
    };
    let rank = fleet_rank(&own).expect("a ranked module");
    let above = |context: &str| context != own && fleet_rank(context).is_some_and(|r| r >= rank);
    let mut tokens = tokenize(src);
    if own == "fleet" {
        tokens = split_reexports(tokens).0;
    }
    let mut found = Vec::new();
    // Calls of a method a context above defines: on `self` and `Self::` within an
    // `impl AppState` block, where they are `AppState`, and on `AppState::` anywhere. A method
    // named as a function value (`Self::f`, `.map(AppState::f)`) counts; after `self.` only a
    // call does, which tells a method from a field.
    let blocks = app_state_blocks(&tokens);
    for (i, w) in tokens.windows(4).enumerate() {
        let in_impl = || blocks.iter().any(|b| b.contains(&i));
        let receiver = match (&w[0].0, &w[1].0) {
            (Token::Ident(r), Token::Punct('.')) if r == "self" => in_impl(),
            (Token::Ident(r), Token::PathSep) if r == "Self" => in_impl(),
            (Token::Ident(r), Token::PathSep) => r == "AppState",
            _ => false,
        };
        let called =
            w[1].0 != Token::Punct('.') || matches!(w[3].0, Token::Punct('(') | Token::PathSep);
        if let (true, true, Token::Ident(method)) = (receiver, called, &w[2].0) {
            if let Some(context) = methods.get(method).filter(|c| above(c)) {
                found.push(format!("{module}:{}: calls {method} of {context}", w[2].1));
            }
        }
    }
    // Paths and `use` declarations that lead into a context above.
    for Named { segments, line, .. } in paths(&tokens) {
        if let Some(context) =
            fleet_context_reached(module, &segments, reexports).filter(|c| above(c))
        {
            found.push(format!(
                "{module}:{line}: names {} of {context}",
                segments.join("::")
            ));
        }
    }
    // Names of a context the root's `use` declarations bring in, named by the root's own code. A
    // context could reach them only through `use super::*`, which `clippy::wildcard_imports`,
    // denied in `fleet`, refuses; its named imports are paths, judged above.
    if own == "fleet" {
        let declared = use_declarations(&tokens);
        for (i, (token, line)) in tokens.iter().enumerate() {
            let Token::Ident(name) = token else { continue };
            let qualified = i > 0 && matches!(tokens[i - 1].0, Token::PathSep | Token::Punct('.'));
            if qualified || declared.iter().any(|r| r.contains(&i)) {
                continue;
            }
            if let Some(context) = imports.get(name).filter(|c| above(c)) {
                found.push(format!(
                    "{module}:{line}: names {name} of {context}, through the root"
                ));
            }
        }
    }
    found
}

/// Each context of the Server's fleet core names only the contexts ranked below it, and the
/// module root names none: no call on `self`, `Self::` or `AppState::` to a method another context
/// defines at or above the caller's rank, and no path, `use` or name through the root that leads
/// into one. Every submodule of `fleet` outside the tests has a rank.
///
/// Verifies: SRV-fleet-contexts-call-by-rank
#[test]
fn each_fleet_context_names_only_the_contexts_ranked_below_it() {
    let reexports = reexports();
    let src = crates_dir().join("fleet-server").join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();
    let mut ranked = Vec::new();
    let mut unranked = Vec::new();
    for file in files {
        let module = module_path(&src, &file);
        let Some(context) = fleet_context(&module) else {
            continue;
        };
        if FLEET_TEST_ONLY.contains(&context.as_str()) {
            continue;
        }
        if fleet_rank(&context).is_none() {
            unranked.push(module);
            continue;
        }
        ranked.push((module, fs::read_to_string(&file).unwrap()));
    }
    assert!(
        unranked.is_empty(),
        "submodules of fleet without a rank in FLEET_RANKS: {unranked:#?}"
    );
    let gone: Vec<&str> = FLEET_RANKS
        .iter()
        .copied()
        .filter(|c| {
            !ranked
                .iter()
                .any(|(m, _)| fleet_context(m).as_deref() == Some(*c))
        })
        .collect();
    assert!(
        gone.is_empty(),
        "contexts of FLEET_RANKS without a source file: {gone:?}"
    );
    let mut methods = BTreeMap::new();
    let mut imports = BTreeMap::new();
    for (module, text) in &ranked {
        let tokens = tokenize(text);
        let context = fleet_context(module).unwrap();
        for method in app_state_methods(&tokens) {
            methods.insert(method, context.clone());
        }
        if module == "fleet" {
            imports = fleet_root_imports(&tokens, &reexports);
        }
    }
    let found: Vec<String> = ranked
        .iter()
        .flat_map(|(module, text)| rank_breaches(module, text, &methods, &imports, &reexports))
        .collect();
    assert!(
        found.is_empty(),
        "a context of fleet names one ranked at or above it: {found:#?}"
    );
}

/// The check the rank test stands on: a call on `self`, `Self::` and `AppState::`, a path, and a
/// name through the root, each judged by the rank of the context it reaches; and the same in test
/// code, in a string, and in the root's `pub use`, left alone.
#[test]
fn the_rank_check_finds_a_context_naming_one_above_it() {
    let reexports: Reexports = [(
        ("fleet-server", "fleet".to_owned(), "bad_request".to_owned()),
        Target::Internal(
            "fleet-server",
            vec!["fleet".into(), "approval".into(), "bad_request".into()],
        ),
    )]
    .into_iter()
    .collect();
    let methods: BTreeMap<String, String> = [
        ("issue", "fleet::approval"),
        ("place_all", "fleet::placement"),
        ("take_row", "fleet::held"),
    ]
    .into_iter()
    .map(|(m, c)| (m.to_owned(), c.to_owned()))
    .collect();
    let imports: BTreeMap<String, String> = [("Placement", "fleet::placement")]
        .into_iter()
        .map(|(m, c)| (m.to_owned(), c.to_owned()))
        .collect();
    let held = r##"
        impl AppState {
            fn f(&self) { self.take_row(); Self::issue(); AppState::place_all(); }
            fn g(&self) { let _ = super::session::bad_request(); let _ = "self.issue()"; }
            fn h(&self) { let _ = xs.map(AppState::issue); let _ = self.issue; }
            fn k(&self) { let _ = super::bad_request(""); }
        }
        #[cfg(test)]
        mod tests { fn t(s: &AppState) { s.issue(); super::session::bad_request(); } }
    "##;
    let found = rank_breaches("fleet::held", held, &methods, &imports, &reexports);
    assert_eq!(found.len(), 4, "{found:#?}");
    assert!(found[0].contains("calls issue of fleet::approval"));
    assert!(found[1].contains("calls issue of fleet::approval"));
    assert!(found[2].contains("names super::session::bad_request of fleet::session"));
    // A name the root re-exports is judged by where it really is.
    assert!(found[3].contains("names super::bad_request of fleet::approval"));
    // A trait's block for `AppState` defines methods too; a `for` loop opens none.
    let blocks = r##"
        impl std::fmt::Display for AppState {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }
        }
        impl AppState { fn g(&self) { for x in y { let _ = x; } } }
    "##;
    assert_eq!(app_state_methods(&tokenize(blocks)), ["fmt", "g"]);
    // The root names no context; its `pub use` of one keeps a public path.
    let root = r##"
        mod placement;
        pub use placement::Placement;
        use placement::Groups;
        struct AgentRecord { placement: Placement }
        impl AppState { fn new() { let _ = Self::place_all; } }
    "##;
    let found = rank_breaches("fleet", root, &methods, &imports, &reexports);
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found[0].contains("calls place_all of fleet::placement"));
    assert!(found[1].contains("names placement::Groups of fleet::placement"));
    assert!(found[2].contains("names Placement of fleet::placement, through the root"));
}
