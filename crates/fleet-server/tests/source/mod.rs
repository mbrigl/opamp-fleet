//! The production code of a crate, read as a syntax tree: the reader this crate's structural tests
//! share. The cut is a copy of the one in `crates/fleet-util/tests/who_shares.rs`: sharing it
//! across crates would take a crate of its own (DEV-six-crates-tokio-axum).

use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};
use quote::ToTokens;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit_mut::{self, VisitMut};
use syn::{Attribute, Expr, ForeignItem, ImplItem, Item, Meta, Stmt, Token, TraitItem};

// --- The production code of a crate -------------------------------------------------------------

/// The production code of the crate whose sources are under `src`, as tokens separated by
/// whitespace: the modules its `lib.rs` and `main.rs` reach, each without its items under a `cfg`
/// that cannot hold without `test`. A module declared under such a `cfg` is cut before it is
/// reached, so its file is not read. Comments are no tokens.
// Each test binary that includes this module uses only some of these.
#[allow(dead_code)]
pub fn production(src: &Path) -> String {
    let mut out = String::new();
    each_production_file(src, &mut |_, code| spaced(code.to_token_stream(), &mut out));
    out
}

/// The production code of the crate whose sources are under `src`, as `production` reads it, but
/// one syntax tree per file, with the file's path.
// Each test binary that includes this module uses only some of these.
#[allow(dead_code)]
pub fn production_files(src: &Path) -> Vec<(PathBuf, syn::File)> {
    let mut files = Vec::new();
    each_production_file(src, &mut |file, code| {
        files.push((file.to_path_buf(), code));
    });
    files
}

/// Hands `visit` each file of production code the crate under `src` reaches, cut as `shipped`
/// cuts it, the modules a file declares before the file itself.
fn each_production_file(src: &Path, visit: &mut dyn FnMut(&Path, syn::File)) {
    for root in ["lib.rs", "main.rs"] {
        let root = src.join(root);
        if root.exists() {
            read_module(&root, src, visit);
        }
    }
}

/// Hands `visit` the production code of the module in `file` and of the modules it declares,
/// whose files are under `dir`.
fn read_module(file: &Path, dir: &Path, visit: &mut dyn FnMut(&Path, syn::File)) {
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
    let code = shipped(&text).unwrap_or_else(|e| panic!("parse {file:?}: {e}"));
    read_items(&code.items, file, dir, visit);
    visit(file, code);
}

/// Appends `tokens` to `out`, each token followed by whitespace but a punctuation character that
/// joins the next, so `::` stays one token; a group's delimiters stand as tokens of their own.
// Each test binary that includes this module uses only some of these.
#[allow(dead_code)]
pub fn spaced(tokens: TokenStream, out: &mut String) {
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
fn read_items(items: &[Item], file: &Path, dir: &Path, visit: &mut dyn FnMut(&Path, syn::File)) {
    for item in items {
        let Item::Mod(module) = item else { continue };
        let name = module
            .ident
            .to_string()
            .trim_start_matches("r#")
            .to_string();
        let child = dir.join(&name);
        match &module.content {
            Some((_, inner)) => read_items(inner, file, &child, visit),
            None => {
                let path = [dir.join(format!("{name}.rs")), child.join("mod.rs")]
                    .into_iter()
                    .find(|path| path.exists())
                    .unwrap_or_else(|| panic!("no file for `mod {name};` of {file:?}"));
                read_module(&path, &child, visit);
            }
        }
    }
}

/// The shipped code of a source text: its syntax tree, with every item, statement, field, variant
/// and arm under a `cfg` attribute that cannot hold without `test` cut.
// Each test binary that includes this module uses only some of these.
#[allow(dead_code)]
pub fn shipped(text: &str) -> syn::Result<syn::File> {
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
