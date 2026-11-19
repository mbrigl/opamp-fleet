//! The Server reads an Agent's type and version, and a key from both attribute sets, through the
//! lookups of `opamp::attributes` alone (CLT-what-an-agent-reports §1 and §4), so the list, the
//! filter, the Configurations, the Packages and the Hosts view cannot read them apart again.
//!
//! The production code is read as a syntax tree by the reader in `tests/source`. Three things are
//! refused there:
//! - a path to the key constants `SERVICE_NAME` or `SERVICE_VERSION`;
//! - the keys spelled as the literals `"service.name"` or `"service.version"`, but where the
//!   Server writes the key of its own OpenTelemetry resource;
//! - a call of `string_value` on an `identifying_attributes` set, but in the one path that reads
//!   the sets the other way round on purpose, the Platform (`Platform::reported`, which reads the
//!   non-identifying set first, since an Agent reports its platform there).

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

mod source;
use source::production_files;

/// The files where a literal key is a write, not a read: the Server's own resource.
const WRITES_A_KEY: [&str; 1] = ["audit_export.rs"];

/// The functions that read the identifying set themselves, each with its reason above.
const READS_THE_SETS_ITSELF: [(&str, &str); 1] = [("packages.rs", "reported")];

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

/// Whether `expr` names the field `identifying_attributes`, through any reference or `?`.
fn names_identifying_attributes(expr: &syn::Expr) -> bool {
    struct Finds(bool);
    impl<'ast> Visit<'ast> for Finds {
        fn visit_member(&mut self, member: &'ast syn::Member) {
            if matches!(member, syn::Member::Named(ident) if ident == "identifying_attributes") {
                self.0 = true;
            }
        }
    }
    let mut finds = Finds(false);
    finds.visit_expr(expr);
    finds.0
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

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if let Some(last) = path.segments.last() {
            if last.ident == "SERVICE_NAME" || last.ident == "SERVICE_VERSION" {
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

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        let calls_string_value = matches!(
            &*call.func,
            syn::Expr::Path(path)
                if path.path.segments.last().is_some_and(|last| last.ident == "string_value")
        );
        let exempt = READS_THE_SETS_ITSELF.contains(&(self.name, self.within().as_str()));
        if calls_string_value
            && !exempt
            && call.args.first().is_some_and(names_identifying_attributes)
        {
            self.found.push(format!(
                "{} in `{}`: reads the identifying attributes with string_value",
                self.name,
                self.within()
            ));
        }
        visit::visit_expr_call(self, call);
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// Verifies: CLT-what-an-agent-reports
#[test]
fn the_server_reads_an_agents_type_version_and_keys_through_the_shared_lookups() {
    let src: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = production_files(&src);
    assert!(
        files
            .iter()
            .any(|(path, _)| file_name(path) == "listing.rs"),
        "the reader found no listing.rs under {src:?}"
    );
    let found: Vec<String> = files
        .iter()
        .flat_map(|(path, code)| reads_past_the_lookups(&file_name(path), code))
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
        "#,
    )
    .expect("the fixture parses");
    let found = reads_past_the_lookups("configs.rs", &code);
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found[0].contains("`fits`") && found[0].contains("identifying"));
    assert!(found[1].contains("`fits`") && found[1].contains("SERVICE_NAME"));
    assert!(found[2].contains("`version`") && found[2].contains("service.version"));
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
