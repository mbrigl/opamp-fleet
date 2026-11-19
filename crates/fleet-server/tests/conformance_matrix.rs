//! The capability matrix of `docs/CONFORMANCE.md` against the code (goal G-12): every capability
//! of the Baseline has a row, each row's bit is the Baseline's, and a capability is listed as
//! implemented or partial exactly when the side it belongs to declares it.
//!
//! The code is read as a syntax tree, without the items under a `cfg` that cannot hold without
//! `test`. The cut is a copy of the one in `crates/fleet-util/tests/who_shares.rs`: sharing it
//! across crates would take a crate of its own (DEV-six-crates-tokio-axum).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opamp::proto::{AgentCapabilities, ServerCapabilities};
use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit_mut::{self, VisitMut};
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Meta, Stmt, Token, TraitItem};

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

// --- The production code of a crate -------------------------------------------------------------

/// The production code of the crate whose sources are under `src`, as tokens separated by
/// whitespace: the modules its `lib.rs` and `main.rs` reach, each without its items under a `cfg`
/// that cannot hold without `test`. A module declared under such a `cfg` is cut before it is
/// reached, so its file is not read. Comments are no tokens.
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
    let code = shipped(&text).unwrap_or_else(|e| panic!("parse {file:?}: {e}"));
    read_items(&code.items, file, dir, out);
    spaced(code.to_token_stream(), out);
}

/// Appends `tokens` to `out`, each token followed by whitespace but a punctuation character that
/// joins the next, so `::` stays one token; a group's delimiters stand as tokens of their own.
fn spaced(tokens: TokenStream, out: &mut String) {
    for tree in tokens {
        match tree {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                out.push_str(open);
                out.push(' ');
                spaced(group.stream(), out);
                out.push_str(close);
                out.push(' ');
            }
            TokenTree::Punct(punct) => {
                out.push(punct.as_char());
                if punct.spacing() == Spacing::Alone {
                    out.push(' ');
                }
            }
            other => {
                out.push_str(&other.to_string());
                out.push(' ');
            }
        }
    }
}

/// Follows each out-of-line module that `items` declare into its file under `dir`, and each
/// inline module into its own items, whose modules are under `dir/<name>`.
fn read_items(items: &[Item], file: &Path, dir: &Path, out: &mut String) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        let name = module
            .ident
            .to_string()
            .trim_start_matches("r#")
            .to_string();
        let child = dir.join(&name);
        match &module.content {
            Some((_, inner)) => read_items(inner, file, &child, out),
            None => {
                let path = [dir.join(format!("{name}.rs")), child.join("mod.rs")]
                    .into_iter()
                    .find(|path| path.exists())
                    .unwrap_or_else(|| panic!("no file for `mod {name};` of {file:?}"));
                read_module(&path, &child, out);
            }
        }
    }
}

/// The shipped code of a source text: its syntax tree, with every item, statement, field, variant
/// and arm under a `cfg` attribute that cannot hold without `test` cut.
fn shipped(text: &str) -> syn::Result<syn::File> {
    let mut file = syn::parse_file(text)?;
    CutTestCode.visit_file_mut(&mut file);
    Ok(file)
}

/// Whether `attrs` hold a `cfg` that cannot hold without `test`: `cfg(test)` and
/// `cfg(all(test, unix))` do, `cfg(not(test))` and `cfg(any(test, unix))` do not.
fn test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<Meta>()
                .is_ok_and(|predicate| holds_without_test(&predicate) == Some(false))
    })
}

/// Whether the `cfg` predicate holds in a build without `test`: `Some(false)` where it cannot,
/// `None` where it depends on what else the build sets, such as `unix` or a feature, or where it
/// is no predicate, such as a macro's `$m`.
fn holds_without_test(predicate: &Meta) -> Option<bool> {
    match predicate {
        Meta::Path(path) => path.is_ident("test").then_some(false),
        Meta::NameValue(_) => None,
        Meta::List(list) => {
            let values: Vec<Option<bool>> = Punctuated::<Meta, Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .ok()?
                .iter()
                .map(holds_without_test)
                .collect();
            if list.path.is_ident("not") {
                values.first().copied().flatten().map(|v| !v)
            } else if list.path.is_ident("all") {
                if values.contains(&Some(false)) {
                    Some(false)
                } else {
                    values.iter().all(|v| *v == Some(true)).then_some(true)
                }
            } else if list.path.is_ident("any") {
                if values.contains(&Some(true)) {
                    Some(true)
                } else {
                    values.iter().all(|v| *v == Some(false)).then_some(false)
                }
            } else {
                None
            }
        }
    }
}

fn item_attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

fn impl_item_attrs(item: &ImplItem) -> &[Attribute] {
    match item {
        ImplItem::Const(i) => &i.attrs,
        ImplItem::Fn(i) => &i.attrs,
        ImplItem::Type(i) => &i.attrs,
        ImplItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &TraitItem) -> &[Attribute] {
    match item {
        TraitItem::Const(i) => &i.attrs,
        TraitItem::Fn(i) => &i.attrs,
        TraitItem::Type(i) => &i.attrs,
        TraitItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn foreign_item_attrs(item: &ForeignItem) -> &[Attribute] {
    match item {
        ForeignItem::Fn(i) => &i.attrs,
        ForeignItem::Static(i) => &i.attrs,
        ForeignItem::Type(i) => &i.attrs,
        ForeignItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

/// The outer attributes of an expression, of every kind `syn` names; `Verbatim`, tokens it could
/// not parse, carries none.
fn expr_attrs(expr: &Expr) -> &[Attribute] {
    match expr {
        Expr::Array(e) => &e.attrs,
        Expr::Assign(e) => &e.attrs,
        Expr::Async(e) => &e.attrs,
        Expr::Await(e) => &e.attrs,
        Expr::Binary(e) => &e.attrs,
        Expr::Block(e) => &e.attrs,
        Expr::Break(e) => &e.attrs,
        Expr::Call(e) => &e.attrs,
        Expr::Cast(e) => &e.attrs,
        Expr::Closure(e) => &e.attrs,
        Expr::Const(e) => &e.attrs,
        Expr::Continue(e) => &e.attrs,
        Expr::Field(e) => &e.attrs,
        Expr::ForLoop(e) => &e.attrs,
        Expr::Group(e) => &e.attrs,
        Expr::If(e) => &e.attrs,
        Expr::Index(e) => &e.attrs,
        Expr::Infer(e) => &e.attrs,
        Expr::Let(e) => &e.attrs,
        Expr::Lit(e) => &e.attrs,
        Expr::Loop(e) => &e.attrs,
        Expr::Macro(e) => &e.attrs,
        Expr::Match(e) => &e.attrs,
        Expr::MethodCall(e) => &e.attrs,
        Expr::Paren(e) => &e.attrs,
        Expr::Path(e) => &e.attrs,
        Expr::Range(e) => &e.attrs,
        Expr::RawAddr(e) => &e.attrs,
        Expr::Reference(e) => &e.attrs,
        Expr::Repeat(e) => &e.attrs,
        Expr::Return(e) => &e.attrs,
        Expr::Struct(e) => &e.attrs,
        Expr::Try(e) => &e.attrs,
        Expr::TryBlock(e) => &e.attrs,
        Expr::Tuple(e) => &e.attrs,
        Expr::Unary(e) => &e.attrs,
        Expr::Unsafe(e) => &e.attrs,
        Expr::While(e) => &e.attrs,
        Expr::Yield(e) => &e.attrs,
        _ => &[],
    }
}

/// The outer attributes a statement's expression starts with: `syn` gives `#[a] *n += 1` the
/// attribute on its leftmost operand, `*n`.
fn leading_attrs(expr: &Expr) -> &[Attribute] {
    let own = expr_attrs(expr);
    if !own.is_empty() {
        return own;
    }
    match expr {
        Expr::Assign(e) => leading_attrs(&e.left),
        Expr::Await(e) => leading_attrs(&e.base),
        Expr::Binary(e) => leading_attrs(&e.left),
        Expr::Call(e) => leading_attrs(&e.func),
        Expr::Cast(e) => leading_attrs(&e.expr),
        Expr::Field(e) => leading_attrs(&e.base),
        Expr::Index(e) => leading_attrs(&e.expr),
        Expr::MethodCall(e) => leading_attrs(&e.receiver),
        Expr::Range(e) => e.start.as_deref().map_or(&[], leading_attrs),
        Expr::Try(e) => leading_attrs(&e.expr),
        _ => &[],
    }
}

fn stmt_attrs(stmt: &Stmt) -> &[Attribute] {
    match stmt {
        Stmt::Local(local) => &local.attrs,
        Stmt::Item(item) => item_attrs(item),
        Stmt::Expr(expr, _) => leading_attrs(expr),
        Stmt::Macro(mac) => &mac.attrs,
    }
}

/// Cuts what a `cfg` keeps out of a build without `test`, wherever such a `cfg` stands in this
/// project's code.
struct CutTestCode;

impl VisitMut for CutTestCode {
    fn visit_file_mut(&mut self, file: &mut syn::File) {
        file.items.retain(|item| !test_only(item_attrs(item)));
        visit_mut::visit_file_mut(self, file);
    }

    fn visit_item_mod_mut(&mut self, module: &mut syn::ItemMod) {
        if let Some((_, items)) = &mut module.content {
            items.retain(|item| !test_only(item_attrs(item)));
        }
        visit_mut::visit_item_mod_mut(self, module);
    }

    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        block.stmts.retain(|stmt| !test_only(stmt_attrs(stmt)));
        visit_mut::visit_block_mut(self, block);
    }

    fn visit_item_impl_mut(&mut self, item: &mut syn::ItemImpl) {
        item.items.retain(|i| !test_only(impl_item_attrs(i)));
        visit_mut::visit_item_impl_mut(self, item);
    }

    fn visit_item_trait_mut(&mut self, item: &mut syn::ItemTrait) {
        item.items.retain(|i| !test_only(trait_item_attrs(i)));
        visit_mut::visit_item_trait_mut(self, item);
    }

    fn visit_item_foreign_mod_mut(&mut self, item: &mut syn::ItemForeignMod) {
        item.items.retain(|i| !test_only(foreign_item_attrs(i)));
        visit_mut::visit_item_foreign_mod_mut(self, item);
    }

    fn visit_expr_match_mut(&mut self, expr: &mut syn::ExprMatch) {
        expr.arms.retain(|arm| !test_only(&arm.attrs));
        visit_mut::visit_expr_match_mut(self, expr);
    }

    fn visit_expr_struct_mut(&mut self, expr: &mut syn::ExprStruct) {
        expr.fields = std::mem::take(&mut expr.fields)
            .into_iter()
            .filter(|field| !test_only(&field.attrs))
            .collect();
        visit_mut::visit_expr_struct_mut(self, expr);
    }

    fn visit_fields_named_mut(&mut self, fields: &mut syn::FieldsNamed) {
        fields.named = std::mem::take(&mut fields.named)
            .into_iter()
            .filter(|field| !test_only(&field.attrs))
            .collect();
        visit_mut::visit_fields_named_mut(self, fields);
    }

    fn visit_item_enum_mut(&mut self, item: &mut syn::ItemEnum) {
        item.variants = std::mem::take(&mut item.variants)
            .into_iter()
            .filter(|variant| !test_only(&variant.attrs))
            .collect();
        visit_mut::visit_item_enum_mut(self, item);
    }
}

// --- The matrix ---------------------------------------------------------------------------------

/// Whether `code`, tokens separated by whitespace, names the path `enum_name::variant`.
fn names(code: &str, enum_name: &str, variant: &str) -> bool {
    let tokens: Vec<&str> = code.split_whitespace().collect();
    tokens
        .windows(3)
        .any(|window| window == [enum_name, "::", variant])
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
        let declared = names(code, enum_name, rust);
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

/// The production code of one source text, as `production` reads it.
fn shipped_code(text: &str) -> String {
    let mut out = String::new();
    spaced(
        shipped(text).expect("the fixture parses").to_token_stream(),
        &mut out,
    );
    out
}

#[test]
fn the_reader_finds_a_declaration_after_a_test_only_item() {
    let code = shipped_code(
        "struct Held {\n    #[cfg(test)]\n    dropped: HashMap<Uid, Vec<u8>>,\n    kept: u8,\n}\n\
         impl Held {\n    #[cfg(test)]\n    #[must_use]\n    fn keeps<T>(&self, uid: T) -> bool {\n        \
         if self.kept > 0 { true } else { '}' == '{' }\n    }\n\n    \
         fn capabilities() -> u64 {\n        #[cfg(test)]\n        let unused = \"}\";\n        \
         #[cfg(test)]\n        let pair = |a, b| (a, b);\n        \
         ServerCapabilities::AcceptsStatus as u64\n    }\n}\n",
    );
    assert!(
        names(&code, "ServerCapabilities", "AcceptsStatus"),
        "{code}"
    );
    assert!(code.contains("kept : u8"), "{code}");
    for test_only in ["dropped", "keeps", "unused", "pair"] {
        assert!(!code.contains(test_only), "{test_only} in {code}");
    }
}

#[test]
fn the_reader_skips_everything_inside_a_test_module() {
    let code = shipped_code(
        "pub fn serve() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    \
         #[test]\n    fn declares() {\n        let _ = ServerCapabilities::OffersRemoteConfig;\n    \
         }\n}\n\npub fn after() {}\n",
    );
    assert!(!code.contains("OffersRemoteConfig"), "{code}");
    assert!(code.contains("pub fn after"), "{code}");
}

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
    assert!(
        !names(&code, "ServerCapabilities", "OffersRemoteConfig"),
        "{code}"
    );
    assert!(
        names(&code, "ServerCapabilities", "AcceptsStatus"),
        "{code}"
    );
}

/// A `cfg` that needs `test` among other options keeps its item out as `cfg(test)` does, the file
/// of a module included; one that holds without `test` keeps it in.
#[test]
fn the_reader_skips_code_under_cfg_all_test_and_unix() {
    let src = tempfile::tempdir().expect("tempdir");
    let write =
        |name: &str, text: &str| std::fs::write(src.path().join(name), text).expect("write");
    write(
        "lib.rs",
        "#[cfg(all(test, unix))]\nmod lock_tests;\nmod held;\n\
         #[cfg(all(test, unix))]\nmod peer {\n    \
         fn f() { let _ = ServerCapabilities::OffersPackages; }\n}\n\
         #[cfg(all(test, unix))]\npub(crate) fn skipped() { ServerCapabilities::AcceptsEffectiveConfig; }\n\
         #[cfg(any(test, unix))]\nfn kept() { ServerCapabilities::OffersConnectionSettings; }\n",
    );
    write(
        "lock_tests.rs",
        "const C: u64 = ServerCapabilities::OffersRemoteConfig as u64;\n",
    );
    write(
        "held.rs",
        "const C: u64 = ServerCapabilities::AcceptsStatus as u64;\n",
    );
    let code = production(src.path());
    for test_only in [
        "OffersRemoteConfig",
        "OffersPackages",
        "AcceptsEffectiveConfig",
    ] {
        assert!(
            !names(&code, "ServerCapabilities", test_only),
            "{test_only} in {code}"
        );
    }
    assert!(
        names(&code, "ServerCapabilities", "AcceptsStatus"),
        "{code}"
    );
    assert!(
        names(&code, "ServerCapabilities", "OffersConnectionSettings"),
        "{code}"
    );
}
