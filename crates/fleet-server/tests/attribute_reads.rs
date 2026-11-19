//! The Server reads an Agent's type and version, and a key from both attribute sets, through the
//! lookups of `opamp::attributes` alone (CLT-what-an-agent-reports §1 and §4), so the list, the
//! filter, the Configurations, the Packages and the Hosts view cannot read them apart again.
//!
//! The production code is read as a syntax tree by the reader in `tests/source`. Three things are
//! refused there:
//! - a path to the key constants `SERVICE_NAME` or `SERVICE_VERSION`, or a `use` of them;
//! - the keys spelled as the literals `"service.name"` or `"service.version"`, but where the
//!   Server writes the key of its own OpenTelemetry resource;
//! - any access to a description's `identifying_attributes` field, but in the functions named in
//!   `READS_THE_SETS_ITSELF`, each with its reason: the walkers that take every attribute, and the
//!   Platform, which reads the non-identifying set first since an Agent reports its platform there.
//!
//! What a macro's body holds is no syntax tree to `syn`, so a read inside `format!` or `matches!`
//! is not seen; the code reads through no such macro today.

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

mod source;
use source::production_files;

/// The files, below `src/`, where a literal key is a write, not a read: the Server's own resource.
const WRITES_A_KEY: [&str; 1] = ["audit_export.rs"];

/// The functions, by file below `src/`, that take a description's identifying set themselves.
const READS_THE_SETS_ITSELF: [(&str, &str); 7] = [
    // The Platform reads the non-identifying set first: an Agent reports its platform there.
    ("packages.rs", "reported"),
    // Walkers over every attribute, which look up no key: the labels an Agent reported, the
    // effective description and its merge, the map the view shows, and the list's search text.
    ("labels.rs", "reported_keys"),
    ("labels.rs", "adds_nothing"),
    ("labels.rs", "effective_description"),
    ("listing.rs", "display_values"),
    ("listing.rs", "derive"),
    // The API's copy of the view's map of attributes, not a description's set.
    ("api.rs", "from"),
];

/// What `file` reads past the shared lookups, one line per finding.
fn reads_past_the_lookups(name: &str, file: &syn::File) -> Vec<String> {
    let mut reader = Reads {
        name,
        functions: Vec::new(),
        found: Vec::new(),
    };
    reader.visit_file(file);
    reader.found
}

struct Reads<'a> {
    name: &'a str,
    functions: Vec<String>,
    found: Vec<String>,
}

impl Reads<'_> {
    fn within(&self) -> String {
        self.functions.last().cloned().unwrap_or_default()
    }
}

/// Whether `ident` is one of the two key constants.
fn is_key_constant(ident: &syn::Ident) -> bool {
    ident == "SERVICE_NAME" || ident == "SERVICE_VERSION"
}

impl<'ast> Visit<'ast> for Reads<'_> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        self.functions.push(item.sig.ident.to_string());
        visit::visit_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.functions.push(item.sig.ident.to_string());
        visit::visit_impl_item_fn(self, item);
        self.functions.pop();
    }

    fn visit_use_name(&mut self, name: &'ast syn::UseName) {
        if is_key_constant(&name.ident) {
            self.found
                .push(format!("{}: uses `{}`", self.name, name.ident));
        }
    }

    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if is_key_constant(&rename.ident) {
            self.found
                .push(format!("{}: uses `{}`", self.name, rename.ident));
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(last) = path.segments.last() {
            if is_key_constant(&last.ident) {
                self.found.push(format!(
                    "{} in `{}`: names `{}`",
                    self.name,
                    self.within(),
                    last.ident
                ));
            }
        }
        visit::visit_path(self, path);
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        let value = literal.value();
        if (value == "service.name" || value == "service.version")
            && !WRITES_A_KEY.contains(&self.name)
        {
            self.found.push(format!(
                "{} in `{}`: spells \"{value}\"",
                self.name,
                self.within()
            ));
        }
    }

    fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
        let identifying = matches!(
            &field.member,
            syn::Member::Named(ident) if ident == "identifying_attributes"
        );
        let exempt = READS_THE_SETS_ITSELF.contains(&(self.name, self.within().as_str()));
        if identifying && !exempt {
            self.found.push(format!(
                "{} in `{}`: reads the identifying attributes",
                self.name,
                self.within()
            ));
        }
        visit::visit_expr_field(self, field);
    }
}

/// `path` below `src`, with `/` on every host.
fn below(src: &Path, path: &Path) -> String {
    path.strip_prefix(src)
        .unwrap_or(path)
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

// Verifies: CLT-what-an-agent-reports
#[test]
fn the_server_reads_an_agents_type_version_and_keys_through_the_shared_lookups() {
    let src: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = production_files(&src);
    assert!(
        files
            .iter()
            .any(|(path, _)| below(&src, path) == "listing.rs"),
        "the reader found no listing.rs under {src:?}"
    );
    let found: Vec<String> = files
        .iter()
        .flat_map(|(path, code)| reads_past_the_lookups(&below(&src, path), code))
        .collect();
    assert!(
        found.is_empty(),
        "read these through opamp::attributes (reported, agent_type, agent_version):\n{}",
        found.join("\n")
    );
}

#[test]
fn the_check_names_each_read_past_the_lookups() {
    let code = syn::parse_file(
        r#"
        fn fits(d: &AgentDescription) -> bool {
            string_value(&d.identifying_attributes, attributes::SERVICE_NAME).is_some()
        }
        fn version(d: &AgentDescription) -> Option<&str> {
            opamp::attributes::string_value(&d.non_identifying_attributes, "service.version")
        }
        fn instance(d: &AgentDescription) -> Option<&str> {
            string_value(&d.non_identifying_attributes, SERVICE_INSTANCE_NAME)
        }
        use opamp::attributes::SERVICE_VERSION as V;
        fn by_hand(d: &AgentDescription, key: &str) -> bool {
            let ids = &d.identifying_attributes;
            ids.iter().any(|kv| kv.key == key)
        }
        "#,
    )
    .expect("the fixture parses");
    let found = reads_past_the_lookups("configs.rs", &code);
    assert_eq!(found.len(), 5, "{found:#?}");
    let has = |within: &str, what: &str| {
        found
            .iter()
            .any(|finding| finding.contains(within) && finding.contains(what))
    };
    assert!(has("`fits`", "identifying"), "{found:#?}");
    assert!(has("`fits`", "SERVICE_NAME"), "{found:#?}");
    assert!(has("`version`", "service.version"), "{found:#?}");
    assert!(has("uses", "SERVICE_VERSION"), "{found:#?}");
    assert!(has("`by_hand`", "identifying"), "{found:#?}");
}

#[test]
fn the_check_leaves_the_named_exceptions() {
    let code = syn::parse_file(
        r#"
        impl Platform {
            fn reported(d: &AgentDescription) -> Option<&str> {
                string_value(&d.non_identifying_attributes, "os.type")
                    .or_else(|| string_value(&d.identifying_attributes, "os.type"))
            }
        }
        fn resource() -> Resource {
            Resource::new([KeyValue::new("service.name", SERVICE)])
        }
        "#,
    )
    .expect("the fixture parses");
    assert!(reads_past_the_lookups("packages.rs", &code)
        .iter()
        .all(|finding| !finding.contains("identifying")));
    assert!(reads_past_the_lookups("audit_export.rs", &code)
        .iter()
        .all(|finding| !finding.contains("spells")));
    assert!(reads_past_the_lookups("configs.rs", &code)
        .iter()
        .any(|finding| finding.contains("spells")));
}
